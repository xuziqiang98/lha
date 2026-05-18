// Forbid accidental stdout/stderr writes in the *library* portion of the TUI.
// The standalone `adam-tui` binary prints a short help message before the
// alternate‑screen mode starts; that file opts‑out locally via `allow`.
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]
use adam_agent::AuthManager;
use adam_agent::INTERACTIVE_SESSION_SOURCES;
use adam_agent::RolloutRecorder;
use adam_agent::ThreadSortKey;
use adam_agent::config::Config;
use adam_agent::config::ConfigBuilder;
use adam_agent::config::ConfigOverrides;
use adam_agent::config::find_adam_home;
use adam_agent::config::load_config_as_toml_with_cli_overrides;
use adam_agent::config_loader::CloudRequirementsLoader;
use adam_agent::config_loader::ConfigLoadError;
use adam_agent::config_loader::format_config_error_with_source;
use adam_agent::default_client::set_default_client_residency_requirement;
use adam_agent::find_thread_path_by_id_str;
use adam_agent::find_thread_path_by_name_str;
use adam_agent::path_utils;
use adam_agent::protocol::AskForApproval;
use adam_agent::read_effective_thread_cwd;
use adam_agent::windows_sandbox::WindowsSandboxLevelExt;
use adam_protocol::config_types::SandboxMode;
use adam_protocol::config_types::WindowsSandboxLevel;
use adam_state::log_db;
use adam_utils_absolute_path::AbsolutePathBuf;
use additional_dirs::add_dir_warning_message;
use app::App;
pub use app::AppExitInfo;
pub use app::ExitReason;
use cwd_prompt::CwdPromptAction;
use cwd_prompt::CwdSelection;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use tracing::error;
use tracing_appender::non_blocking;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

mod additional_dirs;
mod agent_selection_modal;
mod app;
mod app_backtrack;
mod app_event;
mod app_event_sender;
mod approval_mode_modal;
mod bottom_pane;
mod buddy;
mod changelog;
mod chatwidget;
mod cli;
mod clipboard_paste;
mod clipboard_text;
mod collab;
mod color;
pub mod custom_terminal;
mod cwd_prompt;
mod diff_render;
mod exec_cell;
mod exec_command;
mod experimental_features_modal;
mod external_editor;
mod file_search;
mod get_git_diff;
mod history_cell;
mod identities;
mod identity_modal;
pub mod insert_history;
mod key_hint;
mod line_truncation;
pub mod live_wrap;
mod markdown;
mod markdown_render;
mod markdown_stream;
mod mcp_tools_modal;
mod model_migration;
mod model_selection_modal;
mod mouse;
mod multi_agents;
mod notifications;
#[allow(dead_code)]
pub mod onboarding;
mod pager_overlay;
mod project_trust_modal;
mod provider_config;
mod provider_config_modal;
pub mod public_widgets;
mod render;
mod resume_picker;
mod review_modal;
mod selection_list;
mod session_log;
mod shimmer;
mod sidebar;
mod skills_helpers;
mod skills_modal;
mod slash_command;
mod status;
mod status_indicator_widget;
mod streaming;
mod style;
mod terminal_palette;
mod text_formatting;
mod tooltips;
mod transcript_selection;
mod transcript_view;
mod tui;
mod ui_consts;
pub mod update_action;
mod update_prompt;
mod updates;
mod version;

mod wrapping;

#[cfg(test)]
pub mod test_backend;

use crate::tui::Tui;
pub use cli::Cli;
pub use markdown_render::render_markdown_text;
pub use public_widgets::composer_input::ComposerAction;
pub use public_widgets::composer_input::ComposerInput;
// (tests access modules directly within the crate)

pub async fn run_main(
    mut cli: Cli,
    codex_linux_sandbox_exe: Option<PathBuf>,
) -> std::io::Result<AppExitInfo> {
    let (sandbox_mode, approval_policy) = if cli.full_auto {
        (
            Some(SandboxMode::WorkspaceWrite),
            Some(AskForApproval::OnRequest),
        )
    } else if cli.dangerously_bypass_approvals_and_sandbox {
        (
            Some(SandboxMode::DangerFullAccess),
            Some(AskForApproval::Never),
        )
    } else {
        (
            cli.sandbox_mode.map(Into::<SandboxMode>::into),
            cli.approval_policy.map(Into::into),
        )
    };

    // Map the legacy --search flag to the canonical web_search mode.
    if cli.web_search {
        cli.config_overrides
            .raw_overrides
            .push("web_search=\"live\"".to_string());
    }

    let raw_overrides = cli.config_overrides.raw_overrides.clone();
    let overrides_cli = adam_common::CliConfigOverrides { raw_overrides };
    let cli_kv_overrides = match overrides_cli.parse_overrides() {
        // Parse `-c` overrides from the CLI.
        Ok(v) => v,
        #[allow(clippy::print_stderr)]
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };

    // we load config.toml here to determine project state.
    #[allow(clippy::print_stderr)]
    let adam_home = match find_adam_home() {
        Ok(adam_home) => adam_home.to_path_buf(),
        Err(err) => {
            eprintln!("Error finding codex home: {err}");
            std::process::exit(1);
        }
    };

    let cwd = cli.cwd.clone();
    let config_cwd = match cwd.as_deref() {
        Some(path) => AbsolutePathBuf::from_absolute_path(path.canonicalize()?)?,
        None => AbsolutePathBuf::current_dir()?,
    };

    #[allow(clippy::print_stderr)]
    let config_toml = match load_config_as_toml_with_cli_overrides(
        &adam_home,
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

    if let Err(err) =
        adam_agent::personality_migration::maybe_migrate_personality(&adam_home, &config_toml).await
    {
        tracing::warn!(error = %err, "failed to run personality migration");
    }

    let cloud_requirements = CloudRequirementsLoader::default();

    let model = cli.model.clone();

    let additional_dirs = cli.add_dir.clone();

    let overrides = ConfigOverrides {
        model,
        approval_policy,
        sandbox_mode,
        cwd,
        model_provider: None,
        config_profile: cli.config_profile.clone(),
        codex_linux_sandbox_exe,
        show_raw_agent_reasoning: None,
        additional_writable_roots: additional_dirs,
        ..Default::default()
    };

    let config = load_config_or_exit(
        cli_kv_overrides.clone(),
        overrides.clone(),
        cloud_requirements.clone(),
    )
    .await;
    set_default_client_residency_requirement(config.enforce_residency.value());

    if let Some(warning) = add_dir_warning_message(&cli.add_dir, config.sandbox_policy.get()) {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("Error adding directories: {warning}");
            std::process::exit(1);
        }
    }

    let log_dir = adam_agent::config::log_dir(&config)?;
    std::fs::create_dir_all(&log_dir)?;
    // Open (or create) your log file, appending to it.
    let mut log_file_opts = OpenOptions::new();
    log_file_opts.create(true).append(true);

    // Ensure the file is only readable and writable by the current user.
    // Doing the equivalent to `chmod 600` on Windows is quite a bit more code
    // and requires the Windows API crates, so we can reconsider that when
    // Adam CLI is officially supported on Windows.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_file_opts.mode(0o600);
    }

    let log_file = log_file_opts.open(log_dir.join("adam-tui.log"))?;

    // Wrap file in non‑blocking writer.
    let (non_blocking, _guard) = non_blocking(log_file);

    // use RUST_LOG env var, default to info for codex crates.
    let env_filter = || {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("adam_agent=info,adam_tui=info,adam_rmcp_client=info")
        })
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        // `with_target(true)` is the default, but we previously disabled it for file output.
        // Keep it enabled so we can selectively enable targets via `RUST_LOG=...` and then
        // grep for a specific module/target while troubleshooting.
        .with_target(true)
        .with_ansi(false)
        .with_span_events(
            tracing_subscriber::fmt::format::FmtSpan::NEW
                | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
        )
        .with_filter(env_filter());

    let feedback = adam_feedback::CodexFeedback::new();
    let feedback_layer = feedback.logger_layer();
    let feedback_metadata_layer = feedback.metadata_layer();

    let otel = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adam_agent::otel_init::build_provider(&config, env!("CARGO_PKG_VERSION"), None, true)
    })) {
        Ok(Ok(otel)) => otel,
        Ok(Err(e)) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Could not create otel exporter: {e}");
            }
            None
        }
        Err(_) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Could not create otel exporter: panicked during initialization");
            }
            None
        }
    };

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());

    let log_db_layer = adam_agent::state_db::get_state_db(&config, None)
        .await
        .map(|db| log_db::start(db).with_filter(env_filter()));

    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(feedback_layer)
        .with(feedback_metadata_layer)
        .with(log_db_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();

    run_ratatui_app(
        cli,
        config,
        overrides,
        cli_kv_overrides,
        cloud_requirements,
        feedback,
    )
    .await
    .map_err(|err| std::io::Error::other(err.to_string()))
}

async fn run_ratatui_app(
    cli: Cli,
    initial_config: Config,
    overrides: ConfigOverrides,
    cli_kv_overrides: Vec<(String, toml::Value)>,
    cloud_requirements: CloudRequirementsLoader,
    feedback: adam_feedback::CodexFeedback,
) -> color_eyre::Result<AppExitInfo> {
    color_eyre::install()?;

    // Forward panic reports through tracing so they appear in the UI status
    // line, but do not swallow the default/color-eyre panic handler.
    // Chain to the previous hook so users still get a rich panic report
    // (including backtraces) after we restore the terminal.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        prev_hook(info);
    }));
    let use_mouse_capture = resolve_mouse_capture(&cli, &initial_config);
    let mut terminal = tui::init(use_mouse_capture)?;
    terminal.clear()?;

    let mut tui = Tui::new(terminal, use_mouse_capture);
    tui.enter_alt_screen()?;
    let mut terminal_restore_guard = TerminalRestoreGuard::new();

    #[cfg(not(debug_assertions))]
    {
        use crate::update_prompt::UpdatePromptOutcome;

        let skip_update_prompt = cli.prompt.as_ref().is_some_and(|prompt| !prompt.is_empty());
        if !skip_update_prompt {
            match update_prompt::run_update_prompt_if_needed(&mut tui, &initial_config).await? {
                UpdatePromptOutcome::Continue => {}
                UpdatePromptOutcome::RunUpdate(action) => {
                    terminal_restore_guard.restore()?;
                    return Ok(AppExitInfo {
                        token_usage: adam_agent::protocol::TokenUsage::default(),
                        thread_id: None,
                        thread_name: None,
                        update_action: Some(action),
                        exit_reason: ExitReason::UserRequested,
                    });
                }
            }
        }
    }

    // Initialize high-fidelity session event logging if enabled.
    session_log::maybe_init(&initial_config);

    let auth_manager = AuthManager::shared(initial_config.adam_home.clone(), false);
    let config = initial_config;
    tui.set_mouse_capture_enabled(resolve_mouse_capture(&cli, &config))?;

    let mut missing_session_exit = |id_str: &str, action: &str| {
        error!("Error finding conversation path: {id_str}");
        terminal_restore_guard.restore_silently();
        session_log::log_session_end();
        let _ = tui.terminal.clear();
        Ok(AppExitInfo {
            token_usage: adam_agent::protocol::TokenUsage::default(),
            thread_id: None,
            thread_name: None,
            update_action: None,
            exit_reason: ExitReason::Fatal(format!(
                "No saved session found with ID {id_str}. Run `adam {action}` without an ID to choose from existing sessions."
            )),
        })
    };

    let use_fork = cli.fork_picker || cli.fork_last || cli.fork_session_id.is_some();
    let session_selection = if use_fork {
        if let Some(id_str) = cli.fork_session_id.as_deref() {
            let is_uuid = Uuid::parse_str(id_str).is_ok();
            let path = if is_uuid {
                find_thread_path_by_id_str(&config.adam_home, id_str).await?
            } else {
                find_thread_path_by_name_str(&config.adam_home, id_str).await?
            };
            match path {
                Some(path) => resume_picker::SessionSelection::Fork(path),
                None => return missing_session_exit(id_str, "fork"),
            }
        } else if cli.fork_last {
            let filter_cwd = if cli.fork_show_all {
                None
            } else {
                Some(config.cwd.as_path())
            };
            match RolloutRecorder::find_latest_thread_path(
                &config.adam_home,
                1,
                None,
                ThreadSortKey::UpdatedAt,
                INTERACTIVE_SESSION_SOURCES,
                None,
                &config.model_provider_id,
                filter_cwd,
            )
            .await
            {
                Ok(Some(path)) => resume_picker::SessionSelection::Fork(path),
                _ => resume_picker::SessionSelection::StartFresh,
            }
        } else if cli.fork_picker {
            match resume_picker::run_fork_picker(
                &mut tui,
                &config.adam_home,
                &config.model_provider_id,
                Some(config.cwd.as_path()),
                cli.fork_show_all,
            )
            .await?
            {
                resume_picker::SessionSelection::Exit => {
                    terminal_restore_guard.restore_silently();
                    session_log::log_session_end();
                    return Ok(AppExitInfo {
                        token_usage: adam_agent::protocol::TokenUsage::default(),
                        thread_id: None,
                        thread_name: None,
                        update_action: None,
                        exit_reason: ExitReason::UserRequested,
                    });
                }
                other => other,
            }
        } else {
            resume_picker::SessionSelection::StartFresh
        }
    } else if let Some(id_str) = cli.resume_session_id.as_deref() {
        let is_uuid = Uuid::parse_str(id_str).is_ok();
        let path = if is_uuid {
            find_thread_path_by_id_str(&config.adam_home, id_str).await?
        } else {
            find_thread_path_by_name_str(&config.adam_home, id_str).await?
        };
        match path {
            Some(path) => resume_picker::SessionSelection::Resume(path),
            None => return missing_session_exit(id_str, "resume"),
        }
    } else if cli.resume_last {
        let filter_cwd = if cli.resume_show_all {
            None
        } else {
            Some(config.cwd.as_path())
        };
        match RolloutRecorder::find_latest_thread_path(
            &config.adam_home,
            1,
            None,
            ThreadSortKey::UpdatedAt,
            INTERACTIVE_SESSION_SOURCES,
            None,
            &config.model_provider_id,
            filter_cwd,
        )
        .await
        {
            Ok(Some(path)) => resume_picker::SessionSelection::Resume(path),
            _ => resume_picker::SessionSelection::StartFresh,
        }
    } else if cli.resume_picker {
        match resume_picker::run_resume_picker(
            &mut tui,
            &config.adam_home,
            &config.model_provider_id,
            Some(config.cwd.as_path()),
            cli.resume_show_all,
        )
        .await?
        {
            resume_picker::SessionSelection::Exit => {
                terminal_restore_guard.restore_silently();
                session_log::log_session_end();
                return Ok(AppExitInfo {
                    token_usage: adam_agent::protocol::TokenUsage::default(),
                    thread_id: None,
                    thread_name: None,
                    update_action: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
            other => other,
        }
    } else {
        resume_picker::SessionSelection::StartFresh
    };

    let current_cwd = config.cwd.clone();
    let allow_prompt = cli.cwd.is_none();
    let action_and_path_if_resume_or_fork = match &session_selection {
        resume_picker::SessionSelection::Resume(path) => Some((CwdPromptAction::Resume, path)),
        resume_picker::SessionSelection::Fork(path) => Some((CwdPromptAction::Fork, path)),
        _ => None,
    };
    let fallback_cwd = match action_and_path_if_resume_or_fork {
        Some((action, path)) => {
            resolve_cwd_for_resume_or_fork(&mut tui, &current_cwd, path, action, allow_prompt)
                .await?
        }
        None => None,
    };

    let config = match &session_selection {
        resume_picker::SessionSelection::Resume(_) | resume_picker::SessionSelection::Fork(_) => {
            load_config_or_exit_with_fallback_cwd(
                cli_kv_overrides.clone(),
                overrides.clone(),
                cloud_requirements.clone(),
                fallback_cwd,
            )
            .await
        }
        _ => config,
    };
    tui.set_mouse_capture_enabled(resolve_mouse_capture(&cli, &config))?;
    let active_profile = config.active_profile.clone();
    let show_provider_popup_on_startup = config.provider_config_required;
    let show_trust_popup_on_startup = should_show_trust_screen(&config);
    let is_first_run = show_provider_popup_on_startup || show_trust_popup_on_startup;

    let Cli { prompt, images, .. } = cli;

    let app_result = App::run(
        &mut tui,
        auth_manager,
        config,
        cli_kv_overrides.clone(),
        overrides.clone(),
        active_profile,
        prompt,
        images,
        session_selection,
        feedback,
        is_first_run,
        show_provider_popup_on_startup,
        show_trust_popup_on_startup,
    )
    .await;

    terminal_restore_guard.restore_silently();
    // Mark the end of the recorded session.
    session_log::log_session_end();
    // ignore error when collecting usage – report underlying error instead
    app_result
}

pub(crate) async fn read_session_cwd(path: &Path) -> Option<PathBuf> {
    match read_effective_thread_cwd(path).await {
        Ok(cwd) => cwd,
        Err(err) => {
            let rollout_path = path.display().to_string();
            tracing::warn!(
                %rollout_path,
                %err,
                "Failed to read session metadata from rollout"
            );
            None
        }
    }
}

pub(crate) fn cwds_differ(current_cwd: &Path, session_cwd: &Path) -> bool {
    match (
        path_utils::normalize_for_path_comparison(current_cwd),
        path_utils::normalize_for_path_comparison(session_cwd),
    ) {
        (Ok(current), Ok(session)) => current != session,
        _ => current_cwd != session_cwd,
    }
}

pub(crate) async fn resolve_cwd_for_resume_or_fork(
    tui: &mut Tui,
    current_cwd: &Path,
    path: &Path,
    action: CwdPromptAction,
    allow_prompt: bool,
) -> color_eyre::Result<Option<PathBuf>> {
    let Some(history_cwd) = read_session_cwd(path).await else {
        return Ok(None);
    };
    if allow_prompt && cwds_differ(current_cwd, &history_cwd) {
        let selection =
            cwd_prompt::run_cwd_selection_prompt(tui, action, current_cwd, &history_cwd).await?;
        return Ok(Some(match selection {
            CwdSelection::Current => current_cwd.to_path_buf(),
            CwdSelection::Session => history_cwd,
        }));
    }
    Ok(Some(history_cwd))
}

#[expect(
    clippy::print_stderr,
    reason = "TUI should no longer be displayed, so we can write to stderr."
)]
fn restore() {
    if let Err(err) = tui::restore() {
        eprintln!(
            "failed to restore terminal. Run `reset` or restart your terminal to recover: {err}"
        );
    }
}

struct TerminalRestoreGuard {
    active: bool,
}

impl TerminalRestoreGuard {
    fn new() -> Self {
        Self { active: true }
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    fn restore(&mut self) -> color_eyre::Result<()> {
        if self.active {
            crate::tui::restore()?;
            self.active = false;
        }
        Ok(())
    }

    fn restore_silently(&mut self) {
        if self.active {
            restore();
            self.active = false;
        }
    }
}

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        self.restore_silently();
    }
}

async fn load_config_or_exit(
    cli_kv_overrides: Vec<(String, toml::Value)>,
    overrides: ConfigOverrides,
    cloud_requirements: CloudRequirementsLoader,
) -> Config {
    load_config_or_exit_with_fallback_cwd(cli_kv_overrides, overrides, cloud_requirements, None)
        .await
}

async fn load_config_or_exit_with_fallback_cwd(
    cli_kv_overrides: Vec<(String, toml::Value)>,
    overrides: ConfigOverrides,
    cloud_requirements: CloudRequirementsLoader,
    fallback_cwd: Option<PathBuf>,
) -> Config {
    #[allow(clippy::print_stderr)]
    match ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .cloud_requirements(cloud_requirements)
        .fallback_cwd(fallback_cwd)
        .build()
        .await
    {
        Ok(config) => config,
        Err(err) => {
            eprintln!("Error loading configuration: {err}");
            std::process::exit(1);
        }
    }
}

fn resolve_mouse_capture(cli: &Cli, config: &Config) -> bool {
    if cli.no_mouse_capture {
        false
    } else if cli.mouse_capture {
        true
    } else {
        config.tui_mouse_capture
    }
}

/// Determine if user has configured a sandbox / approval policy,
/// or if the current cwd project is already trusted. If not, we need to
/// show the trust screen.
fn should_show_trust_screen(config: &Config) -> bool {
    if cfg!(target_os = "windows")
        && WindowsSandboxLevel::from_config(config) == WindowsSandboxLevel::Disabled
    {
        // If the experimental sandbox is not enabled, Native Windows cannot enforce sandboxed write access; skip the trust prompt entirely.
        return false;
    }
    if config.did_user_set_custom_approval_policy_or_sandbox_mode {
        // Respect explicit approval/sandbox overrides made by the user.
        return false;
    }
    // otherwise, show only if no trust decision has been made
    config.active_project.trust_level.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use adam_agent::INTERACTIVE_SESSION_SOURCES;
    use adam_agent::RolloutRecorder;
    use adam_agent::ThreadSortKey;
    use adam_agent::config::ConfigBuilder;
    use adam_agent::config::ConfigOverrides;
    use adam_agent::config::ProjectConfig;
    use adam_agent::protocol::AskForApproval;
    use adam_protocol::protocol::RolloutItem;
    use adam_protocol::protocol::RolloutLine;
    use adam_protocol::protocol::SessionMeta;
    use adam_protocol::protocol::SessionMetaLine;
    use adam_protocol::protocol::TurnContextItem;
    use chrono::Utc;
    use serial_test::serial;
    use std::time::Duration;
    use tempfile::TempDir;
    use uuid::Uuid;

    async fn build_config(temp_dir: &TempDir) -> std::io::Result<Config> {
        ConfigBuilder::default()
            .adam_home(temp_dir.path().to_path_buf())
            .build()
            .await
    }

    #[tokio::test]
    #[serial]
    async fn windows_skips_trust_prompt_without_sandbox() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.did_user_set_custom_approval_policy_or_sandbox_mode = false;
        config.active_project = ProjectConfig { trust_level: None };
        config.set_windows_sandbox_enabled(false);

        let should_show = should_show_trust_screen(&config);
        if cfg!(target_os = "windows") {
            assert!(
                !should_show,
                "Windows trust prompt should always be skipped on native Windows"
            );
        } else {
            assert!(
                should_show,
                "Non-Windows should still show trust prompt when project is untrusted"
            );
        }
        Ok(())
    }
    #[tokio::test]
    #[serial]
    async fn windows_shows_trust_prompt_with_sandbox() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.did_user_set_custom_approval_policy_or_sandbox_mode = false;
        config.active_project = ProjectConfig { trust_level: None };
        config.set_windows_sandbox_enabled(true);

        let should_show = should_show_trust_screen(&config);
        if cfg!(target_os = "windows") {
            assert!(
                should_show,
                "Windows trust prompt should be shown on native Windows with sandbox enabled"
            );
        } else {
            assert!(
                should_show,
                "Non-Windows should still show trust prompt when project is untrusted"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn resolve_mouse_capture_uses_config_by_default() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        let cli = Cli::default();

        config.tui_mouse_capture = true;
        assert!(resolve_mouse_capture(&cli, &config));

        config.tui_mouse_capture = false;
        assert!(!resolve_mouse_capture(&cli, &config));

        Ok(())
    }

    #[tokio::test]
    async fn resolve_mouse_capture_no_flag_overrides_config() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.tui_mouse_capture = true;
        let cli = Cli {
            no_mouse_capture: true,
            ..Default::default()
        };

        assert!(!resolve_mouse_capture(&cli, &config));

        Ok(())
    }

    #[tokio::test]
    async fn resolve_mouse_capture_flag_overrides_config() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.tui_mouse_capture = false;
        let cli = Cli {
            mouse_capture: true,
            ..Default::default()
        };

        assert!(resolve_mouse_capture(&cli, &config));

        Ok(())
    }

    #[tokio::test]
    async fn untrusted_project_skips_trust_prompt() -> std::io::Result<()> {
        use adam_protocol::config_types::TrustLevel;
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.did_user_set_custom_approval_policy_or_sandbox_mode = false;
        config.active_project = ProjectConfig {
            trust_level: Some(TrustLevel::Untrusted),
        };

        let should_show = should_show_trust_screen(&config);
        assert!(
            !should_show,
            "Trust prompt should not be shown for projects explicitly marked as untrusted"
        );
        Ok(())
    }

    fn build_turn_context(config: &Config, cwd: PathBuf) -> TurnContextItem {
        let model = config
            .model
            .clone()
            .unwrap_or_else(|| "gpt-5.1".to_string());
        TurnContextItem {
            cwd,
            approval_policy: config.approval_policy.value(),
            sandbox_policy: config.sandbox_policy.get().clone(),
            model,
            personality: None,
            identity: None,
            effort: config.model_reasoning_effort,
            summary: config.model_reasoning_summary,
            user_instructions: None,
            developer_instructions: None,
            final_output_json_schema: None,
            truncation_policy: None,
        }
    }

    fn write_rollout(
        adam_home: &std::path::Path,
        file_ts: &str,
        cwd: &std::path::Path,
        provider: &str,
        preview: &str,
    ) -> std::io::Result<PathBuf> {
        let sessions_root = adam_home.join("sessions");
        let date = &file_ts[..10];
        let year = &date[..4];
        let month = &date[5..7];
        let day = &date[8..10];
        let dir = sessions_root.join(year).join(month).join(day);
        std::fs::create_dir_all(&dir)?;

        let filename = format!(
            "rollout-{}-{}.jsonl",
            file_ts.replace(':', "-"),
            Uuid::new_v4()
        );
        let path = dir.join(filename);
        let now = Utc::now().to_rfc3339();
        let meta = RolloutLine {
            timestamp: now.clone(),
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta: SessionMeta {
                    id: adam_protocol::ThreadId::default(),
                    timestamp: now.clone(),
                    cwd: cwd.to_path_buf(),
                    originator: "test".to_string(),
                    cli_version: "0.0.0".to_string(),
                    rollout_schema_version: adam_protocol::protocol::ROLLOUT_SCHEMA_VERSION_V3,
                    source: adam_protocol::protocol::SessionSource::Cli,
                    model_provider: Some(provider.to_string()),
                    base_instructions: None,
                    dynamic_tools: None,
                    forked_from_id: None,
                },
                git: None,
            }),
        };
        let user = RolloutLine {
            timestamp: now,
            item: RolloutItem::EventMsg(adam_protocol::protocol::EventMsg::UserMessage(
                adam_protocol::protocol::UserMessageEvent {
                    message: preview.to_string(),
                    images: None,
                    local_images: Vec::new(),
                    text_elements: Vec::new(),
                },
            )),
        };

        let mut text = String::new();
        for line in [meta, user] {
            text.push_str(&serde_json::to_string(&line).expect("serialize rollout"));
            text.push('\n');
        }
        std::fs::write(&path, text)?;
        Ok(path)
    }

    #[tokio::test]
    async fn read_session_cwd_prefers_latest_turn_context() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let config = build_config(&temp_dir).await?;
        let first = temp_dir.path().join("first");
        let second = temp_dir.path().join("second");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&second)?;

        let rollout_path = temp_dir.path().join("rollout.jsonl");
        let lines = vec![
            RolloutLine {
                timestamp: "t0".to_string(),
                item: RolloutItem::TurnContext(build_turn_context(&config, first)),
            },
            RolloutLine {
                timestamp: "t1".to_string(),
                item: RolloutItem::TurnContext(build_turn_context(&config, second.clone())),
            },
        ];
        let mut text = String::new();
        for line in lines {
            text.push_str(&serde_json::to_string(&line).expect("serialize rollout"));
            text.push('\n');
        }
        std::fs::write(&rollout_path, text)?;

        let cwd = read_session_cwd(&rollout_path).await.expect("expected cwd");
        assert_eq!(cwd, second);
        Ok(())
    }

    #[tokio::test]
    async fn should_prompt_when_meta_matches_current_but_latest_turn_differs() -> std::io::Result<()>
    {
        let temp_dir = TempDir::new()?;
        let config = build_config(&temp_dir).await?;
        let current = temp_dir.path().join("current");
        let latest = temp_dir.path().join("latest");
        std::fs::create_dir_all(&current)?;
        std::fs::create_dir_all(&latest)?;

        let rollout_path = temp_dir.path().join("rollout.jsonl");
        let session_meta = SessionMeta {
            cwd: current.clone(),
            ..SessionMeta::default()
        };
        let lines = vec![
            RolloutLine {
                timestamp: "t0".to_string(),
                item: RolloutItem::SessionMeta(SessionMetaLine {
                    meta: session_meta,
                    git: None,
                }),
            },
            RolloutLine {
                timestamp: "t1".to_string(),
                item: RolloutItem::TurnContext(build_turn_context(&config, latest.clone())),
            },
        ];
        let mut text = String::new();
        for line in lines {
            text.push_str(&serde_json::to_string(&line).expect("serialize rollout"));
            text.push('\n');
        }
        std::fs::write(&rollout_path, text)?;

        let session_cwd = read_session_cwd(&rollout_path).await.expect("expected cwd");
        assert_eq!(session_cwd, latest);
        assert!(cwds_differ(&current, &session_cwd));
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn find_latest_thread_path_prefers_same_cwd_across_providers() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        let current_cwd = temp_dir.path().join("project");
        let other_cwd = temp_dir.path().join("other");
        std::fs::create_dir_all(&current_cwd)?;
        std::fs::create_dir_all(&other_cwd)?;
        config.cwd = current_cwd.clone();
        config.model_provider_id = "openai".to_string();

        let _same_provider_same_cwd = write_rollout(
            temp_dir.path(),
            "2025-01-02T00:00:00",
            &current_cwd,
            "openai",
            "same provider same cwd",
        )?;
        std::thread::sleep(Duration::from_secs(1));
        let expected = write_rollout(
            temp_dir.path(),
            "2025-01-03T00:00:00",
            &current_cwd,
            "anthropic",
            "other provider same cwd",
        )?;
        std::thread::sleep(Duration::from_secs(1));
        let _same_provider_other_cwd = write_rollout(
            temp_dir.path(),
            "2025-01-04T00:00:00",
            &other_cwd,
            "openai",
            "same provider other cwd",
        )?;

        let latest = RolloutRecorder::find_latest_thread_path(
            &config.adam_home,
            1,
            None,
            ThreadSortKey::UpdatedAt,
            INTERACTIVE_SESSION_SOURCES,
            None,
            &config.model_provider_id,
            Some(config.cwd.as_path()),
        )
        .await?;

        assert_eq!(latest, Some(expected));
        Ok(())
    }

    #[tokio::test]
    async fn config_rebuild_changes_trust_defaults_with_cwd() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let adam_home = temp_dir.path().to_path_buf();
        let trusted = temp_dir.path().join("trusted");
        let untrusted = temp_dir.path().join("untrusted");
        std::fs::create_dir_all(&trusted)?;
        std::fs::create_dir_all(&untrusted)?;

        // TOML keys need escaped backslashes on Windows paths.
        let trusted_display = trusted.display().to_string().replace('\\', "\\\\");
        let untrusted_display = untrusted.display().to_string().replace('\\', "\\\\");
        let config_toml = format!(
            r#"[projects."{trusted_display}"]
trust_level = "trusted"

[projects."{untrusted_display}"]
trust_level = "untrusted"
"#
        );
        std::fs::write(temp_dir.path().join("config.toml"), config_toml)?;

        let trusted_overrides = ConfigOverrides {
            cwd: Some(trusted.clone()),
            ..Default::default()
        };
        let trusted_config = ConfigBuilder::default()
            .adam_home(adam_home.clone())
            .harness_overrides(trusted_overrides.clone())
            .build()
            .await?;
        assert_eq!(
            trusted_config.approval_policy.value(),
            AskForApproval::OnRequest
        );

        let untrusted_overrides = ConfigOverrides {
            cwd: Some(untrusted),
            ..trusted_overrides
        };
        let untrusted_config = ConfigBuilder::default()
            .adam_home(adam_home)
            .harness_overrides(untrusted_overrides)
            .build()
            .await?;
        assert_eq!(
            untrusted_config.approval_policy.value(),
            AskForApproval::UnlessTrusted
        );
        Ok(())
    }

    #[tokio::test]
    async fn read_session_cwd_falls_back_to_session_meta() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let _config = build_config(&temp_dir).await?;
        let session_cwd = temp_dir.path().join("session");
        std::fs::create_dir_all(&session_cwd)?;

        let rollout_path = temp_dir.path().join("rollout.jsonl");
        let session_meta = SessionMeta {
            cwd: session_cwd.clone(),
            ..SessionMeta::default()
        };
        let meta_line = RolloutLine {
            timestamp: "t0".to_string(),
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta: session_meta,
                git: None,
            }),
        };
        let text = format!(
            "{}\n",
            serde_json::to_string(&meta_line).expect("serialize meta")
        );
        std::fs::write(&rollout_path, text)?;

        let cwd = read_session_cwd(&rollout_path).await.expect("expected cwd");
        assert_eq!(cwd, session_cwd);
        Ok(())
    }
}
