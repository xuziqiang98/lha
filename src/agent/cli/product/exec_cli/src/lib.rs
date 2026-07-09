// - In the default output mode, it is paramount that the only thing written to
//   stdout is the final message (if any).
// - In --json mode, stdout must be valid JSONL, one event per line.
// For both modes, any other output must be written to stderr.
#![deny(clippy::print_stdout)]

mod cli;
mod event_processor;
mod event_processor_with_human_output;
pub mod event_processor_with_jsonl_output;
mod event_processor_with_raw_event_output;
pub mod exec_events;

use crate::product::agent::AuthManager;
use crate::product::agent::NewThread;
use crate::product::agent::ThreadManager;
use crate::product::agent::config::Config;
use crate::product::agent::config::ConfigBuilder;
use crate::product::agent::config::ConfigOverrides;
use crate::product::agent::config::find_lha_home;
use crate::product::agent::config::load_config_as_toml_with_cli_overrides;
use crate::product::agent::config_loader::CloudRequirementsLoader;
use crate::product::agent::config_loader::ConfigLoadError;
use crate::product::agent::config_loader::format_config_error_with_source;
use crate::product::agent::env::LHA_AGENT_JOB_AUTH_TOKEN_ENV_VAR;
use crate::product::agent::env::LHA_AGENT_JOB_PROVIDER_CONTEXT_ENV_VAR;
use crate::product::agent::env::LHA_AGENT_JOB_REASONING_EFFORT_ENV_VAR;
use crate::product::agent::env::LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR;
use crate::product::agent::env::LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL_ENV_VAR;
use crate::product::agent::git_info::get_git_repo_root;
use crate::product::agent::protocol::AskForApproval;
use crate::product::agent::protocol::Event;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::Op;
use crate::product::agent::protocol::ReviewRequest;
use crate::product::agent::protocol::ReviewTarget;
use crate::product::agent::protocol::SandboxPolicy;
use crate::product::agent::protocol::SessionSource;
use crate::product::protocol::approvals::ElicitationAction;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::SandboxMode;
use crate::product::protocol::config_types::Settings;
use crate::product::protocol::config_types::WindowsSandboxLevel;
use crate::product::protocol::openai_models::ReasoningEffort;
use crate::product::protocol::user_input::UserInput;
use crate::product::utils_absolute_path::AbsolutePathBuf;
pub use cli::Cli;
pub use cli::Command;
pub use cli::ReviewArgs;
pub(crate) use cli::parse_with_config_overrides;
use event_processor_with_human_output::EventProcessorWithHumanOutput;
use event_processor_with_jsonl_output::EventProcessorWithJsonOutput;
use event_processor_with_raw_event_output::EventProcessorWithRawEventOutput;
use lha_llm::CatalogRefreshStrategy;
use lha_llm::RuntimeEndpoint;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use supports_color::Stream;
use tokio::sync::Mutex;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

use crate::product::agent::default_client::set_default_client_residency_requirement;
use crate::product::agent::default_client::set_default_originator;
use crate::product::agent::find_thread_path_by_id_str;
use crate::product::agent::find_thread_path_by_name_str;
use crate::product::exec_cli::cli::Command as ExecCommand;
use crate::product::exec_cli::event_processor::CodexStatus;
use crate::product::exec_cli::event_processor::EventProcessor;

enum InitialOperation {
    UserTurn {
        items: Vec<UserInput>,
        output_schema: Option<Value>,
    },
    Review {
        review_request: ReviewRequest,
    },
}

#[derive(Clone)]
struct ThreadEventEnvelope {
    thread_id: crate::product::protocol::ThreadId,
    thread: Arc<crate::product::agent::CodexThread>,
    event: Event,
}

fn should_skip_attached_thread_event(
    thread_id: crate::product::protocol::ThreadId,
    primary_thread_id: crate::product::protocol::ThreadId,
    msg: &EventMsg,
) -> bool {
    thread_id != primary_thread_id
        && matches!(
            msg,
            EventMsg::TurnStarted(_) | EventMsg::TurnComplete(_) | EventMsg::InputSlimming(_)
        )
}

#[derive(Debug, Deserialize, Serialize)]
struct AgentJobProviderContext {
    model_provider_id: String,
    model_provider: RuntimeEndpoint,
}

#[derive(Debug, Default)]
struct AgentJobStartupContext {
    model_provider_overrides: HashMap<String, RuntimeEndpoint>,
    sandbox_policy: Option<SandboxPolicy>,
    windows_sandbox_level: Option<WindowsSandboxLevel>,
    reasoning_effort: Option<Option<ReasoningEffort>>,
}

fn take_agent_job_startup_context() -> anyhow::Result<AgentJobStartupContext> {
    let provider_context = std::env::var(LHA_AGENT_JOB_PROVIDER_CONTEXT_ENV_VAR).ok();
    let auth_token = std::env::var(LHA_AGENT_JOB_AUTH_TOKEN_ENV_VAR)
        .ok()
        .filter(|token| !token.trim().is_empty());
    let sandbox_policy = std::env::var(LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR).ok();
    let windows_sandbox_level = std::env::var(LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL_ENV_VAR).ok();
    let reasoning_effort = std::env::var(LHA_AGENT_JOB_REASONING_EFFORT_ENV_VAR).ok();
    clear_agent_job_context_env();

    let model_provider_overrides = match provider_context {
        Some(provider_context) => {
            let mut context: AgentJobProviderContext = serde_json::from_str(&provider_context)?;
            if let Some(auth_token) = auth_token {
                context.model_provider.bearer_token = Some(auth_token);
                context.model_provider.env_key = None;
            }
            HashMap::from([(context.model_provider_id, context.model_provider)])
        }
        None => HashMap::new(),
    };

    let sandbox_policy = parse_agent_job_env_json::<SandboxPolicy>(
        LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR,
        sandbox_policy,
    )?;
    let windows_sandbox_level = parse_agent_job_env_json::<WindowsSandboxLevel>(
        LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL_ENV_VAR,
        windows_sandbox_level,
    )?;
    let reasoning_effort = parse_agent_job_env_json::<Option<ReasoningEffort>>(
        LHA_AGENT_JOB_REASONING_EFFORT_ENV_VAR,
        reasoning_effort,
    )?;

    Ok(AgentJobStartupContext {
        model_provider_overrides,
        sandbox_policy,
        windows_sandbox_level,
        reasoning_effort,
    })
}

fn parse_agent_job_env_json<T>(key: &str, value: Option<String>) -> anyhow::Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    value
        .map(|value| {
            serde_json::from_str(&value)
                .map_err(|err| anyhow::anyhow!("invalid {key} value: {err}"))
        })
        .transpose()
}

fn clear_agent_job_context_env() {
    // SAFETY: `lha-exec` calls this during startup before it creates session
    // services or any worker threads that could concurrently read the process
    // environment.
    unsafe {
        std::env::remove_var(LHA_AGENT_JOB_PROVIDER_CONTEXT_ENV_VAR);
        std::env::remove_var(LHA_AGENT_JOB_AUTH_TOKEN_ENV_VAR);
        std::env::remove_var(LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR);
        std::env::remove_var(LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL_ENV_VAR);
        std::env::remove_var(LHA_AGENT_JOB_REASONING_EFFORT_ENV_VAR);
    }
}

fn startup_sandbox_overrides(
    full_auto: bool,
    dangerously_bypass_approvals_and_sandbox: bool,
    inherited_sandbox_policy: Option<&SandboxPolicy>,
    sandbox_mode_cli_arg: Option<crate::product::common::SandboxModeCliArg>,
) -> (Option<SandboxPolicy>, Option<SandboxMode>) {
    if full_auto {
        (None, Some(SandboxMode::WorkspaceWrite))
    } else if dangerously_bypass_approvals_and_sandbox {
        (None, Some(SandboxMode::DangerFullAccess))
    } else if let Some(policy) = inherited_sandbox_policy {
        (Some(policy.clone()), None)
    } else {
        (None, sandbox_mode_cli_arg.map(Into::<SandboxMode>::into))
    }
}

pub async fn run_main(cli: Cli, codex_linux_sandbox_exe: Option<PathBuf>) -> anyhow::Result<()> {
    if let Err(err) = set_default_originator("lha_exec".to_string()) {
        tracing::warn!(?err, "Failed to set lha exec originator override {err:?}");
    }

    let Cli {
        command,
        images,
        model: model_cli_arg,
        config_profile,
        identity: identity_cli_arg,
        full_auto,
        dangerously_bypass_approvals_and_sandbox,
        cwd,
        skip_git_repo_check,
        add_dir,
        color,
        last_message_file,
        json: json_mode,
        internal_raw_events,
        sandbox_mode: sandbox_mode_cli_arg,
        prompt,
        output_schema: output_schema_path,
        config_overrides,
    } = cli;

    if json_mode && internal_raw_events {
        anyhow::bail!("--json cannot be combined with --internal-raw-events");
    }

    let (stdout_with_ansi, stderr_with_ansi) = match color {
        cli::Color::Always => (true, true),
        cli::Color::Never => (false, false),
        cli::Color::Auto => (
            supports_color::on_cached(Stream::Stdout).is_some(),
            supports_color::on_cached(Stream::Stderr).is_some(),
        ),
    };

    // Build fmt layer (existing logging) to compose with OTEL layer.
    let default_level = "error";

    // Build env_filter separately and attach via with_filter.
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(stderr_with_ansi)
        .with_writer(std::io::stderr)
        .with_filter(env_filter);

    // Parse `-c` overrides from the CLI.
    let cli_kv_overrides = match config_overrides.parse_overrides() {
        Ok(v) => v,
        #[allow(clippy::print_stderr)]
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };
    let AgentJobStartupContext {
        model_provider_overrides,
        sandbox_policy: inherited_sandbox_policy,
        windows_sandbox_level: inherited_windows_sandbox_level,
        reasoning_effort: inherited_reasoning_effort,
    } = take_agent_job_startup_context()?;

    let (sandbox_policy, sandbox_mode) = startup_sandbox_overrides(
        full_auto,
        dangerously_bypass_approvals_and_sandbox,
        inherited_sandbox_policy.as_ref(),
        sandbox_mode_cli_arg,
    );

    let resolved_cwd = cwd.clone();
    let config_cwd = match resolved_cwd.as_deref() {
        Some(path) => AbsolutePathBuf::from_absolute_path(path.canonicalize()?)?,
        None => AbsolutePathBuf::current_dir()?,
    };

    // we load config.toml here to determine project state.
    #[allow(clippy::print_stderr)]
    let lha_home = match find_lha_home() {
        Ok(lha_home) => lha_home,
        Err(err) => {
            eprintln!("Error finding lha home: {err}");
            std::process::exit(1);
        }
    };

    #[allow(clippy::print_stderr)]
    let _config_toml = match load_config_as_toml_with_cli_overrides(
        &lha_home,
        &config_cwd,
        cli_kv_overrides.clone(),
    )
    .await
    {
        Ok(config_toml) => config_toml,
        Err(err) => {
            let config_error = err
                .get_ref()
                .and_then(|err| err.downcast_ref::<ConfigLoadError>())
                .map(ConfigLoadError::config_error);
            if let Some(config_error) = config_error {
                eprintln!(
                    "Error loading config.toml:\n{}",
                    format_config_error_with_source(config_error)
                );
            } else {
                eprintln!("Error loading config.toml: {err}");
            }
            std::process::exit(1);
        }
    };

    let cloud_requirements = CloudRequirementsLoader::default();

    let model = model_cli_arg;

    // Load configuration and determine approval policy
    let overrides = ConfigOverrides {
        model,
        review_model: None,
        config_profile,
        // Default to never ask for approvals in headless mode. Feature flags can override.
        approval_policy: Some(AskForApproval::Never),
        sandbox_policy,
        sandbox_mode,
        cwd: resolved_cwd,
        model_provider: None,
        codex_linux_sandbox_exe,
        base_instructions: None,
        developer_instructions: None,
        personality: None,
        compact_prompt: None,
        include_apply_patch_tool: None,
        show_raw_agent_reasoning: None,
        tools_web_search_request: None,
        ephemeral: None,
        model_provider_overrides,
        additional_writable_roots: add_dir,
    };

    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .cloud_requirements(cloud_requirements)
        .build()
        .await?;
    set_default_client_residency_requirement(config.enforce_residency.value());

    let otel = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::product::agent::otel_init::build_provider(
            &config,
            env!("CARGO_PKG_VERSION"),
            None,
            false,
        )
    })) {
        Ok(Ok(otel)) => otel,
        Ok(Err(e)) => {
            eprintln!("Could not create otel exporter: {e}");
            None
        }
        Err(_) => {
            eprintln!("Could not create otel exporter: panicked during initialization");
            None
        }
    };

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());

    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_tracing_layer)
        .with(otel_logger_layer)
        .try_init();

    let mut event_processor: Box<dyn EventProcessor> = if internal_raw_events {
        Box::new(EventProcessorWithRawEventOutput::new(
            last_message_file.clone(),
        ))
    } else if json_mode {
        Box::new(EventProcessorWithJsonOutput::new(last_message_file.clone()))
    } else {
        Box::new(EventProcessorWithHumanOutput::create_with_ansi(
            stdout_with_ansi,
            &config,
            last_message_file.clone(),
        ))
    };
    let default_cwd = config.cwd.to_path_buf();
    let default_approval_policy = config.approval_policy.value();
    let default_sandbox_policy = inherited_sandbox_policy
        .clone()
        .unwrap_or_else(|| config.sandbox_policy.get().clone());
    let configured_effort = config.model_reasoning_effort;
    let default_effort = inherited_reasoning_effort.unwrap_or(configured_effort);
    let default_summary = config.model_reasoning_summary;

    // When --yolo (dangerously_bypass_approvals_and_sandbox) is set, also skip the git repo check
    // since the user is explicitly running in an externally sandboxed environment.
    if !skip_git_repo_check
        && !dangerously_bypass_approvals_and_sandbox
        && get_git_repo_root(&default_cwd).is_none()
    {
        eprintln!("Not inside a trusted directory and --skip-git-repo-check was not specified.");
        std::process::exit(1);
    }

    let auth_manager = AuthManager::shared(config.lha_home.clone(), true);
    let thread_manager = Arc::new(ThreadManager::new(
        config.lha_home.clone(),
        auth_manager.clone(),
        config.model_provider_id.as_str(),
        config.model_provider.clone(),
        SessionSource::Exec,
    ));
    let default_model = thread_manager
        .get_default_model(
            &config.model,
            &config,
            CatalogRefreshStrategy::OnlineIfUncached,
        )
        .await?;
    let selected_identity = identity_cli_arg.map(IdentityKind::from).map(|kind| {
        identity_for_kind_with_inherited_effort(
            kind,
            default_model.clone(),
            default_effort,
            inherited_reasoning_effort,
        )
    });

    // Handle resume subcommand by resolving a rollout path and using explicit resume API.
    let NewThread {
        thread_id: primary_thread_id,
        thread,
        session_configured,
    } = if let Some(ExecCommand::Resume(args)) = command.as_ref() {
        let resume_path = resolve_resume_path(&config, args).await?;

        if let Some(path) = resume_path {
            thread_manager
                .resume_thread_from_rollout(config.clone(), path, auth_manager.clone())
                .await?
        } else {
            thread_manager.start_thread(config.clone()).await?
        }
    } else {
        thread_manager.start_thread(config.clone()).await?
    };
    let (initial_operation, prompt_summary) = match (command, prompt, images) {
        (Some(ExecCommand::Review(review_cli)), _, _) => {
            let review_request = build_review_request(review_cli)?;
            let summary =
                crate::product::agent::review_prompts::user_facing_hint(&review_request.target);
            (InitialOperation::Review { review_request }, summary)
        }
        (Some(ExecCommand::Resume(args)), root_prompt, imgs) => {
            let prompt_arg = args
                .prompt
                .clone()
                .or_else(|| {
                    if args.last {
                        args.session_id.clone()
                    } else {
                        None
                    }
                })
                .or(root_prompt);
            let prompt_text = resolve_prompt(prompt_arg);
            let mut items: Vec<UserInput> = imgs
                .into_iter()
                .chain(args.images.into_iter())
                .map(|path| UserInput::LocalImage { path })
                .collect();
            items.push(UserInput::Text {
                text: prompt_text.clone(),
                // CLI input doesn't track UI element ranges, so none are available here.
                text_elements: Vec::new(),
            });
            let output_schema = load_output_schema(output_schema_path.clone());
            (
                InitialOperation::UserTurn {
                    items,
                    output_schema,
                },
                prompt_text,
            )
        }
        (None, root_prompt, imgs) => {
            let prompt_text = resolve_prompt(root_prompt);
            let mut items: Vec<UserInput> = imgs
                .into_iter()
                .map(|path| UserInput::LocalImage { path })
                .collect();
            items.push(UserInput::Text {
                text: prompt_text.clone(),
                // CLI input doesn't track UI element ranges, so none are available here.
                text_elements: Vec::new(),
            });
            let output_schema = load_output_schema(output_schema_path);
            (
                InitialOperation::UserTurn {
                    items,
                    output_schema,
                },
                prompt_text,
            )
        }
    };

    // Print the effective configuration and initial request so users can see what LHA
    // is using.
    event_processor.print_config_summary(&config, &prompt_summary, &session_configured);

    info!("LHA initialized with event: {session_configured:?}");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ThreadEventEnvelope>();
    let attached_threads = Arc::new(Mutex::new(HashSet::from([primary_thread_id])));
    spawn_thread_listener(primary_thread_id, thread.clone(), tx.clone());

    {
        let thread = thread.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::debug!("Keyboard interrupt");
                // Immediately notify LHA to abort any in-flight task.
                thread.submit(Op::Interrupt).await.ok();
            }
        });
    }

    {
        let thread_manager = Arc::clone(&thread_manager);
        let attached_threads = Arc::clone(&attached_threads);
        let tx = tx.clone();
        let mut thread_created_rx = thread_manager.subscribe_thread_created();
        tokio::spawn(async move {
            loop {
                match thread_created_rx.recv().await {
                    Ok(thread_id) => {
                        if attached_threads.lock().await.contains(&thread_id) {
                            continue;
                        }
                        match thread_manager.get_thread(thread_id).await {
                            Ok(thread) => {
                                attached_threads.lock().await.insert(thread_id);
                                spawn_thread_listener(thread_id, thread, tx.clone());
                            }
                            Err(err) => {
                                warn!("failed to attach listener for thread {thread_id}: {err}")
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        warn!("thread_created receiver lagged; skipping resync");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    if let Some(op) = agent_job_context_override_op(
        inherited_sandbox_policy.clone(),
        inherited_windows_sandbox_level,
    ) {
        // Agent jobs inherit write sandbox scope, but approvals intentionally
        // remain headless (`Never`) because child approval requests cannot be
        // answered by the parent process yet.
        thread.submit(op).await?;
    }

    match initial_operation {
        InitialOperation::UserTurn {
            items,
            output_schema,
        } => {
            let task_id = thread
                .submit(Op::UserTurn {
                    items,
                    cwd: default_cwd,
                    approval_policy: default_approval_policy,
                    sandbox_policy: default_sandbox_policy,
                    model: default_model,
                    effort: default_effort,
                    summary: default_summary,
                    final_output_json_schema: output_schema,
                    identity: selected_identity.clone(),
                    personality: None,
                    tui_buddy: None,
                })
                .await?;
            info!("Sent prompt with event ID: {task_id}");
            task_id
        }
        InitialOperation::Review { review_request } => {
            if let Some(op) = review_identity_override_op(selected_identity.clone()) {
                thread.submit(op).await?;
            }
            let task_id = thread.submit(Op::Review { review_request }).await?;
            info!("Sent review request with event ID: {task_id}");
            task_id
        }
    };

    // Run the loop until the task is complete.
    // Track whether a fatal error was reported by the server so we can
    // exit with a non-zero status for automation-friendly signaling.
    let mut error_seen = false;
    while let Some(envelope) = rx.recv().await {
        let ThreadEventEnvelope {
            thread_id,
            thread,
            event,
        } = envelope;
        if let EventMsg::ElicitationRequest(ev) = &event.msg {
            // Automatically cancel elicitation requests in exec mode.
            thread
                .submit(Op::ResolveElicitation {
                    server_name: ev.server_name.clone(),
                    request_id: ev.id.clone(),
                    decision: ElicitationAction::Cancel,
                })
                .await?;
        }
        if matches!(event.msg, EventMsg::Error(_)) {
            error_seen = true;
        }
        if should_skip_attached_thread_event(thread_id, primary_thread_id, &event.msg) {
            continue;
        }
        let shutdown = event_processor.process_event(event);
        if thread_id != primary_thread_id && matches!(shutdown, CodexStatus::InitiateShutdown) {
            continue;
        }
        match shutdown {
            CodexStatus::Running => continue,
            CodexStatus::InitiateShutdown => {
                thread.submit(Op::Shutdown).await?;
            }
            CodexStatus::Shutdown if thread_id == primary_thread_id => break,
            CodexStatus::Shutdown => continue,
        }
    }
    event_processor.print_final_output();
    if error_seen {
        std::process::exit(1);
    }

    Ok(())
}

fn spawn_thread_listener(
    thread_id: crate::product::protocol::ThreadId,
    thread: Arc<crate::product::agent::CodexThread>,
    tx: tokio::sync::mpsc::UnboundedSender<ThreadEventEnvelope>,
) {
    tokio::spawn(async move {
        loop {
            match thread.next_event().await {
                Ok(event) => {
                    debug!("Received event: {event:?}");

                    let is_shutdown_complete = matches!(event.msg, EventMsg::ShutdownComplete);
                    if let Err(err) = tx.send(ThreadEventEnvelope {
                        thread_id,
                        thread: Arc::clone(&thread),
                        event,
                    }) {
                        error!("Error sending event: {err:?}");
                        break;
                    }
                    if is_shutdown_complete {
                        info!(
                            "Received shutdown event for thread {thread_id}, exiting event loop."
                        );
                        break;
                    }
                }
                Err(err) => {
                    error!("Error receiving event: {err:?}");
                    break;
                }
            }
        }
    });
}

async fn resolve_resume_path(
    config: &Config,
    args: &crate::product::exec_cli::cli::ResumeArgs,
) -> anyhow::Result<Option<PathBuf>> {
    if args.last {
        let filter_cwd = if args.all {
            None
        } else {
            Some(config.cwd.as_path())
        };
        match crate::product::agent::RolloutRecorder::find_latest_thread_path(
            &config.lha_home,
            1,
            None,
            crate::product::agent::ThreadSortKey::UpdatedAt,
            &[],
            None,
            &config.model_provider_id,
            filter_cwd,
        )
        .await
        {
            Ok(path) => Ok(path),
            Err(e) => {
                error!("Error listing threads: {e}");
                Ok(None)
            }
        }
    } else if let Some(id_str) = args.session_id.as_deref() {
        if Uuid::parse_str(id_str).is_ok() {
            let path = find_thread_path_by_id_str(&config.lha_home, id_str).await?;
            Ok(path)
        } else {
            let path = find_thread_path_by_name_str(&config.lha_home, id_str).await?;
            Ok(path)
        }
    } else {
        Ok(None)
    }
}

fn load_output_schema(path: Option<PathBuf>) -> Option<Value> {
    let path = path?;

    let schema_str = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            eprintln!(
                "Failed to read output schema file {}: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    };

    match serde_json::from_str::<Value>(&schema_str) {
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!(
                "Output schema file {} is not valid JSON: {err}",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

fn identity_for_kind(
    kind: IdentityKind,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
) -> Identity {
    let base = Identity {
        kind: IdentityKind::Nobody,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let mask = match kind {
        IdentityKind::Nobody => crate::product::identity::nobody_preset(),
        IdentityKind::Planner => crate::product::identity::planner_preset(),
        IdentityKind::Programmer => crate::product::identity::programmer_preset(),
        IdentityKind::Explorer => crate::product::identity::explorer_preset(),
        IdentityKind::Reviewer => crate::product::identity::reviewer_preset(),
    };
    base.apply_mask(&mask)
}

fn identity_for_kind_with_inherited_effort(
    kind: IdentityKind,
    model: String,
    effective_effort: Option<ReasoningEffort>,
    inherited_effort: Option<Option<ReasoningEffort>>,
) -> Identity {
    let identity = identity_for_kind(kind, model, effective_effort);
    if inherited_effort.is_some() {
        identity.with_updates(None, Some(effective_effort), None)
    } else {
        identity
    }
}

fn review_identity_override_op(identity: Option<Identity>) -> Option<Op> {
    identity.map(|identity| Op::OverrideTurnContext {
        cwd: None,
        approval_policy: None,
        sandbox_policy: None,
        windows_sandbox_level: None,
        model: None,
        effort: None,
        summary: None,
        identity: Some(identity),
        personality: None,
    })
}

fn agent_job_context_override_op(
    sandbox_policy: Option<SandboxPolicy>,
    windows_sandbox_level: Option<WindowsSandboxLevel>,
) -> Option<Op> {
    if sandbox_policy.is_none() && windows_sandbox_level.is_none() {
        return None;
    }

    Some(Op::OverrideTurnContext {
        cwd: None,
        approval_policy: None,
        sandbox_policy,
        windows_sandbox_level,
        model: None,
        effort: None,
        summary: None,
        identity: None,
        personality: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptDecodeError {
    InvalidUtf8 { valid_up_to: usize },
    InvalidUtf16 { encoding: &'static str },
    UnsupportedBom { encoding: &'static str },
}

impl std::fmt::Display for PromptDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptDecodeError::InvalidUtf8 { valid_up_to } => write!(
                f,
                "input is not valid UTF-8 (invalid byte at offset {valid_up_to}). Convert it to UTF-8 and retry (e.g., `iconv -f <ENC> -t UTF-8 prompt.txt`)."
            ),
            PromptDecodeError::InvalidUtf16 { encoding } => write!(
                f,
                "input looked like {encoding} but could not be decoded. Convert it to UTF-8 and retry."
            ),
            PromptDecodeError::UnsupportedBom { encoding } => write!(
                f,
                "input appears to be {encoding}. Convert it to UTF-8 and retry."
            ),
        }
    }
}

fn decode_prompt_bytes(input: &[u8]) -> Result<String, PromptDecodeError> {
    let input = input.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(input);

    if input.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return Err(PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32LE",
        });
    }

    if input.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return Err(PromptDecodeError::UnsupportedBom {
            encoding: "UTF-32BE",
        });
    }

    if let Some(rest) = input.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, "UTF-16LE", u16::from_le_bytes);
    }

    if let Some(rest) = input.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, "UTF-16BE", u16::from_be_bytes);
    }

    std::str::from_utf8(input)
        .map(str::to_string)
        .map_err(|e| PromptDecodeError::InvalidUtf8 {
            valid_up_to: e.valid_up_to(),
        })
}

fn decode_utf16(
    input: &[u8],
    encoding: &'static str,
    decode_unit: fn([u8; 2]) -> u16,
) -> Result<String, PromptDecodeError> {
    if !input.len().is_multiple_of(2) {
        return Err(PromptDecodeError::InvalidUtf16 { encoding });
    }

    let units: Vec<u16> = input
        .chunks_exact(2)
        .map(|chunk| decode_unit([chunk[0], chunk[1]]))
        .collect();

    String::from_utf16(&units).map_err(|_| PromptDecodeError::InvalidUtf16 { encoding })
}

fn resolve_prompt(prompt_arg: Option<String>) -> String {
    match prompt_arg {
        Some(p) if p != "-" => p,
        maybe_dash => {
            let force_stdin = matches!(maybe_dash.as_deref(), Some("-"));

            if std::io::stdin().is_terminal() && !force_stdin {
                eprintln!(
                    "No prompt provided. Either specify one as an argument or pipe the prompt into stdin."
                );
                std::process::exit(1);
            }

            if !force_stdin {
                eprintln!("Reading prompt from stdin...");
            }

            let mut bytes = Vec::new();
            if let Err(e) = std::io::stdin().read_to_end(&mut bytes) {
                eprintln!("Failed to read prompt from stdin: {e}");
                std::process::exit(1);
            }

            let buffer = match decode_prompt_bytes(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to read prompt from stdin: {e}");
                    std::process::exit(1);
                }
            };

            if buffer.trim().is_empty() {
                eprintln!("No prompt provided via stdin.");
                std::process::exit(1);
            }
            buffer
        }
    }
}

fn build_review_request(args: ReviewArgs) -> anyhow::Result<ReviewRequest> {
    let target = if args.uncommitted {
        ReviewTarget::UncommittedChanges
    } else if let Some(branch) = args.base {
        ReviewTarget::BaseBranch { branch }
    } else if let Some(sha) = args.commit {
        ReviewTarget::Commit {
            sha,
            title: args.commit_title,
        }
    } else if let Some(prompt_arg) = args.prompt {
        let prompt = resolve_prompt(Some(prompt_arg)).trim().to_string();
        if prompt.is_empty() {
            anyhow::bail!("Review prompt cannot be empty");
        }
        ReviewTarget::Custom {
            instructions: prompt,
        }
    } else {
        anyhow::bail!(
            "Specify --uncommitted, --base, --commit, or provide custom review instructions"
        );
    };

    Ok(ReviewRequest {
        target,
        user_facing_hint: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::protocol::AgentMessageEvent;
    use crate::product::agent::protocol::InputSlimmingEvent;
    use crate::product::agent::protocol::InputSlimmingScope;
    use crate::product::agent::protocol::InputSlimmingTokenStats;
    use crate::product::agent::protocol::NetworkAccess;
    use crate::product::agent::protocol::TurnCompleteEvent;
    use crate::product::agent::protocol::TurnStartedEvent;
    use crate::product::protocol::ThreadId;
    use pretty_assertions::assert_eq;
    use std::sync::Mutex as StdMutex;

    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    fn set_env_var(key: &str, value: &str) {
        // SAFETY: tests that mutate process env hold `ENV_LOCK`.
        unsafe {
            std::env::set_var(key, value);
        }
    }

    fn test_thread_id(s: &str) -> ThreadId {
        ThreadId::from_string(s).expect("valid thread id")
    }

    fn input_slimming_msg() -> EventMsg {
        let stats = InputSlimmingTokenStats {
            tokens_before: 100,
            tokens_after: 40,
            tokens_saved: 60,
            replacements: 1,
            saved_usd_micros: None,
        };

        EventMsg::InputSlimming(InputSlimmingEvent {
            scope: InputSlimmingScope::HistoricalToolOutputs,
            last: stats,
            total: stats,
        })
    }

    fn turn_started_msg() -> EventMsg {
        EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        })
    }

    #[test]
    fn attached_thread_filter_skips_turn_boundaries_and_input_slimming() {
        let primary_thread_id = test_thread_id("00000000-0000-0000-0000-000000000001");
        let child_thread_id = test_thread_id("00000000-0000-0000-0000-000000000002");
        let cases = [
            ("child turn started", child_thread_id, turn_started_msg()),
            (
                "child turn complete",
                child_thread_id,
                EventMsg::TurnComplete(TurnCompleteEvent {
                    last_agent_message: None,
                }),
            ),
            (
                "child input slimming",
                child_thread_id,
                input_slimming_msg(),
            ),
            (
                "child ordinary message",
                child_thread_id,
                EventMsg::AgentMessage(AgentMessageEvent {
                    message: "child update".to_string(),
                    memory_citation: None,
                }),
            ),
            (
                "primary input slimming",
                primary_thread_id,
                input_slimming_msg(),
            ),
            (
                "primary turn started",
                primary_thread_id,
                turn_started_msg(),
            ),
            (
                "primary turn complete",
                primary_thread_id,
                EventMsg::TurnComplete(TurnCompleteEvent {
                    last_agent_message: None,
                }),
            ),
        ];

        let actual = cases
            .iter()
            .map(|(name, thread_id, msg)| {
                (
                    *name,
                    should_skip_attached_thread_event(*thread_id, primary_thread_id, msg),
                )
            })
            .collect::<Vec<_>>();
        let expected = vec![
            ("child turn started", true),
            ("child turn complete", true),
            ("child input slimming", true),
            ("child ordinary message", false),
            ("primary input slimming", false),
            ("primary turn started", false),
            ("primary turn complete", false),
        ];

        assert_eq!(actual, expected);
    }

    #[test]
    fn builds_uncommitted_review_request() {
        let request = build_review_request(ReviewArgs {
            uncommitted: true,
            base: None,
            commit: None,
            commit_title: None,
            prompt: None,
        })
        .expect("builds uncommitted review request");

        let expected = ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn agent_job_provider_context_overrides_provider_and_consumes_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        clear_agent_job_context_env();
        let mut provider =
            RuntimeEndpoint::openai_compatible_responses("mock", "http://127.0.0.1:9/v1");
        provider.env_key = Some("SHOULD_NOT_BE_USED".to_string());
        let context = AgentJobProviderContext {
            model_provider_id: "mock.main".to_string(),
            model_provider: provider,
        };
        set_env_var(
            LHA_AGENT_JOB_PROVIDER_CONTEXT_ENV_VAR,
            &serde_json::to_string(&context).expect("provider context json"),
        );
        set_env_var(LHA_AGENT_JOB_AUTH_TOKEN_ENV_VAR, "secret-token");

        let context = take_agent_job_startup_context().expect("startup context");

        assert!(std::env::var(LHA_AGENT_JOB_PROVIDER_CONTEXT_ENV_VAR).is_err());
        assert!(std::env::var(LHA_AGENT_JOB_AUTH_TOKEN_ENV_VAR).is_err());
        let provider = context
            .model_provider_overrides
            .get("mock.main")
            .expect("mock provider");
        assert_eq!(provider.base_url.as_deref(), Some("http://127.0.0.1:9/v1"));
        assert_eq!(provider.bearer_token.as_deref(), Some("secret-token"));
        assert_eq!(provider.env_key, None);
    }

    #[test]
    fn agent_job_startup_context_defaults_without_sandbox_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        clear_agent_job_context_env();

        let context = take_agent_job_startup_context().expect("startup context");

        assert!(context.model_provider_overrides.is_empty());
        assert_eq!(context.sandbox_policy, None);
        assert_eq!(context.windows_sandbox_level, None);
    }

    #[test]
    fn agent_job_startup_context_consumes_sandbox_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        clear_agent_job_context_env();
        let expected_policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: false,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };
        set_env_var(
            LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR,
            &serde_json::to_string(&expected_policy).expect("sandbox policy json"),
        );
        set_env_var(
            LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL_ENV_VAR,
            &serde_json::to_string(&WindowsSandboxLevel::RestrictedToken)
                .expect("windows sandbox level json"),
        );

        let context = take_agent_job_startup_context().expect("startup context");

        assert!(std::env::var(LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR).is_err());
        assert!(std::env::var(LHA_AGENT_JOB_WINDOWS_SANDBOX_LEVEL_ENV_VAR).is_err());
        assert_eq!(context.sandbox_policy, Some(expected_policy));
        assert_eq!(
            context.windows_sandbox_level,
            Some(WindowsSandboxLevel::RestrictedToken)
        );
    }

    #[test]
    fn agent_job_startup_context_rejects_invalid_sandbox_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        clear_agent_job_context_env();
        set_env_var(LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR, "not json");

        let err = take_agent_job_startup_context().expect_err("invalid sandbox env");

        assert!(std::env::var(LHA_AGENT_JOB_SANDBOX_POLICY_ENV_VAR).is_err());
        assert!(
            err.to_string()
                .contains("invalid LHA_AGENT_JOB_SANDBOX_POLICY value")
        );
    }

    #[test]
    fn startup_sandbox_overrides_preserve_inherited_external_sandbox() {
        let policy = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Restricted,
        };

        let overrides = startup_sandbox_overrides(
            false,
            false,
            Some(&policy),
            Some(crate::product::common::SandboxModeCliArg::DangerFullAccess),
        );

        assert_eq!(overrides, (Some(policy), None));
    }

    #[test]
    fn startup_sandbox_overrides_preserve_inherited_workspace_policy() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: true,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let overrides = startup_sandbox_overrides(
            false,
            false,
            Some(&policy),
            Some(crate::product::common::SandboxModeCliArg::ReadOnly),
        );

        assert_eq!(overrides, (Some(policy), None));
    }

    #[test]
    fn startup_sandbox_overrides_use_cli_mode_without_inherited_policy() {
        let overrides = startup_sandbox_overrides(
            false,
            false,
            None,
            Some(crate::product::common::SandboxModeCliArg::WorkspaceWrite),
        );

        assert_eq!(overrides, (None, Some(SandboxMode::WorkspaceWrite)));
    }

    #[test]
    fn startup_sandbox_overrides_keep_explicit_mode_flags_highest_priority() {
        let policy = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Restricted,
        };

        assert_eq!(
            startup_sandbox_overrides(false, true, Some(&policy), None),
            (None, Some(SandboxMode::DangerFullAccess))
        );
        assert_eq!(
            startup_sandbox_overrides(true, false, Some(&policy), None),
            (None, Some(SandboxMode::WorkspaceWrite))
        );
    }

    #[test]
    fn builds_commit_review_request_with_title() {
        let request = build_review_request(ReviewArgs {
            uncommitted: false,
            base: None,
            commit: Some("123456789".to_string()),
            commit_title: Some("Add review command".to_string()),
            prompt: None,
        })
        .expect("builds commit review request");

        let expected = ReviewRequest {
            target: ReviewTarget::Commit {
                sha: "123456789".to_string(),
                title: Some("Add review command".to_string()),
            },
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn builds_custom_review_request_trims_prompt() {
        let request = build_review_request(ReviewArgs {
            uncommitted: false,
            base: None,
            commit: None,
            commit_title: None,
            prompt: Some("  custom review instructions  ".to_string()),
        })
        .expect("builds custom review request");

        let expected = ReviewRequest {
            target: ReviewTarget::Custom {
                instructions: "custom review instructions".to_string(),
            },
            user_facing_hint: None,
        };

        assert_eq!(request, expected);
    }

    #[test]
    fn review_identity_override_op_applies_selected_identity() {
        let identity = identity_for_kind(IdentityKind::Reviewer, "gpt-5.1".to_string(), None);
        let op = review_identity_override_op(Some(identity.clone()));

        let expected = Some(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity),
            personality: None,
        });

        assert_eq!(op, expected);
    }

    #[test]
    fn inherited_effort_overrides_explorer_identity_preset() {
        let identity = identity_for_kind_with_inherited_effort(
            IdentityKind::Explorer,
            "gpt-5.1".to_string(),
            Some(ReasoningEffort::High),
            Some(Some(ReasoningEffort::High)),
        );

        assert_eq!(identity.reasoning_effort(), Some(ReasoningEffort::High));
    }

    #[test]
    fn inherited_null_effort_clears_reviewer_identity_preset() {
        let identity = identity_for_kind_with_inherited_effort(
            IdentityKind::Reviewer,
            "gpt-5.1".to_string(),
            None,
            Some(None),
        );

        assert_eq!(identity.reasoning_effort(), None);
    }

    #[test]
    fn absent_inherited_effort_keeps_explorer_identity_preset() {
        let identity = identity_for_kind_with_inherited_effort(
            IdentityKind::Explorer,
            "gpt-5.1".to_string(),
            Some(ReasoningEffort::High),
            None,
        );

        assert_eq!(identity.reasoning_effort(), Some(ReasoningEffort::Low));
    }

    #[test]
    fn absent_inherited_effort_keeps_reviewer_identity_preset() {
        let identity = identity_for_kind_with_inherited_effort(
            IdentityKind::Reviewer,
            "gpt-5.1".to_string(),
            None,
            None,
        );

        assert_eq!(identity.reasoning_effort(), Some(ReasoningEffort::Medium));
    }

    #[test]
    fn review_identity_override_op_is_absent_without_selected_identity() {
        let op = review_identity_override_op(None);

        assert_eq!(op, None);
    }

    #[test]
    fn decode_prompt_bytes_strips_utf8_bom() {
        let input = [0xEF, 0xBB, 0xBF, b'h', b'i', b'\n'];

        let out = decode_prompt_bytes(&input).expect("decode utf-8 with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_decodes_utf16le_bom() {
        // UTF-16LE BOM + "hi\n"
        let input = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00, b'\n', 0x00];

        let out = decode_prompt_bytes(&input).expect("decode utf-16le with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_decodes_utf16be_bom() {
        // UTF-16BE BOM + "hi\n"
        let input = [0xFE, 0xFF, 0x00, b'h', 0x00, b'i', 0x00, b'\n'];

        let out = decode_prompt_bytes(&input).expect("decode utf-16be with BOM");

        assert_eq!(out, "hi\n");
    }

    #[test]
    fn decode_prompt_bytes_rejects_utf32le_bom() {
        // UTF-32LE BOM + "hi\n"
        let input = [
            0xFF, 0xFE, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00, 0x00, b'\n', 0x00,
            0x00, 0x00,
        ];

        let err = decode_prompt_bytes(&input).expect_err("utf-32le should be rejected");

        assert_eq!(
            err,
            PromptDecodeError::UnsupportedBom {
                encoding: "UTF-32LE"
            }
        );
    }

    #[test]
    fn decode_prompt_bytes_rejects_utf32be_bom() {
        // UTF-32BE BOM + "hi\n"
        let input = [
            0x00, 0x00, 0xFE, 0xFF, 0x00, 0x00, 0x00, b'h', 0x00, 0x00, 0x00, b'i', 0x00, 0x00,
            0x00, b'\n',
        ];

        let err = decode_prompt_bytes(&input).expect_err("utf-32be should be rejected");

        assert_eq!(
            err,
            PromptDecodeError::UnsupportedBom {
                encoding: "UTF-32BE"
            }
        );
    }

    #[test]
    fn decode_prompt_bytes_rejects_invalid_utf8() {
        // Invalid UTF-8 sequence: 0xC3 0x28
        let input = [0xC3, 0x28];

        let err = decode_prompt_bytes(&input).expect_err("invalid utf-8 should fail");

        assert_eq!(err, PromptDecodeError::InvalidUtf8 { valid_up_to: 0 });
    }
}
