use crate::agent_selection_modal::AgentSelectionModal;
use crate::agent_selection_modal::AgentSelectionModalAction;
use crate::agent_selection_modal::AgentSelectionModalItem;
use crate::app_backtrack::BacktrackState;
use crate::app_event::AppEvent;
use crate::app_event::BuddyConfigEdit;
use crate::app_event::ExitMode;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxFallbackReason;
use crate::app_event_sender::AppEventSender;
use crate::approval_mode_modal::ApprovalModeAction;
use crate::approval_mode_modal::ApprovalModeItem;
use crate::approval_mode_modal::ApprovalModeModal;
use crate::approval_mode_modal::ApprovalModeModalAction;
use crate::bottom_pane::ApprovalRequest;
use crate::changelog::ChangelogOutput;
use crate::changelog::DirectorySnapshot;
use crate::changelog::get_git_changelog;
use crate::changelog::get_non_git_changelog;
use crate::changelog::git_repo_root;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::ExternalEditorState;
use crate::chatwidget::SkillsModalItems;
use crate::chatwidget::UserMessage;
use crate::cwd_prompt::CwdPromptAction;
use crate::diff_render::DiffSummary;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::experimental_features_modal::ExperimentalFeatureItem;
use crate::experimental_features_modal::ExperimentalFeaturesModal;
use crate::experimental_features_modal::ExperimentalFeaturesModalAction;
use crate::external_editor;
use crate::file_search::FileSearchManager;
use crate::history_cell;
use crate::history_cell::HistoryCell;
#[cfg(not(debug_assertions))]
use crate::history_cell::UpdateAvailableHistoryCell;
use crate::identities;
use crate::identity_modal::IdentityModal;
use crate::identity_modal::IdentityModalAction;
use crate::mcp_tools_modal::McpToolsModal;
use crate::mcp_tools_modal::McpToolsModalAction;
use crate::model_migration::ModelMigrationOutcome;
use crate::model_migration::migration_copy_for_models;
use crate::model_migration::run_model_migration_prompt;
use crate::model_selection_modal::ModelSelectionModal;
use crate::model_selection_modal::ModelSelectionModalAction;
use crate::model_selection_modal::ModelSelectionModalContext;
use crate::multi_agents::AgentPickerThreadEntry;
use crate::multi_agents::format_agent_picker_item_name;
use crate::multi_agents::sort_agent_picker_threads;
use crate::pager_overlay::Overlay;
use crate::project_trust_modal::ProjectTrustModal;
use crate::project_trust_modal::ProjectTrustModalAction;
use crate::provider_config::CustomProviderConfig;
use crate::provider_config::custom_provider_ref;
use crate::provider_config::persist_custom_provider_files;
use crate::provider_config_modal::ProviderConfigModal;
use crate::provider_config_modal::ProviderConfigModalAction;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::SessionSelection;
use crate::review_modal::ReviewModal;
use crate::review_modal::ReviewModalAction;
use crate::sidebar::AgentPanelEntry;
use crate::skills_modal::SkillsModal;
use crate::skills_modal::SkillsModalAction;
use crate::tui;
use crate::tui::TuiEvent;
use crate::update_action::UpdateAction;
use adam_agent::AuthManager;
use adam_agent::ThreadManager;
use adam_agent::config::Config;
use adam_agent::config::ConfigBuilder;
use adam_agent::config::ConfigOverrides;
use adam_agent::config::display_model_provider_ref;
use adam_agent::config::edit::ConfigEdit;
use adam_agent::config::edit::ConfigEditsBuilder;
use adam_agent::config::model_ref::ModelRef;
use adam_agent::config::models_json::ModelsJson;
use adam_agent::config::set_project_trust_level;
use adam_agent::config::state_json::AdamStateStore;
use adam_agent::config::types::TuiBuddy;
use adam_agent::config_loader::ConfigLayerStackOrdering;
use adam_agent::features::FEATURES;
use adam_agent::features::Feature;
use adam_agent::git_info::resolve_root_git_project_for_trust;
use adam_agent::models_manager::model_presets::HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG;
use adam_agent::models_manager::model_presets::HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG;
use adam_agent::protocol::AgentStatus;
use adam_agent::protocol::AskForApproval;
use adam_agent::protocol::Event;
use adam_agent::protocol::EventMsg;
use adam_agent::protocol::FinalOutput;
use adam_agent::protocol::ListSkillsResponseEvent;
use adam_agent::protocol::Op;
use adam_agent::protocol::ReviewRequest;
use adam_agent::protocol::SandboxPolicy;
use adam_agent::protocol::SessionSource;
use adam_agent::protocol::SkillErrorInfo;
use adam_agent::protocol::SubAgentSource;
use adam_agent::protocol::TokenUsage;
use adam_agent::protocol::TurnAbortReason;
#[cfg(target_os = "windows")]
use adam_agent::windows_sandbox::WindowsSandboxLevelExt;
use adam_ansi_escape::ansi_escape_line;
use adam_app_server_protocol::ConfigLayerSource;
use adam_common::approval_presets::ApprovalPreset;
use adam_common::approval_presets::builtin_approval_presets;
use adam_llm::CatalogRefreshStrategy;
use adam_llm::RuntimeEndpoint;
use adam_otel::OtelManager;
use adam_protocol::ThreadId;
use adam_protocol::config_types::IdentityMask;
use adam_protocol::config_types::Personality;
use adam_protocol::config_types::TrustLevel;
#[cfg(target_os = "windows")]
use adam_protocol::config_types::WindowsSandboxLevel;
use adam_protocol::items::TurnItem;
use adam_protocol::openai_models::ModelPreset;
use adam_protocol::openai_models::ModelUpgrade;
use adam_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use adam_protocol::protocol::SessionConfiguredEvent;
use adam_utils_absolute_path::AbsolutePathBuf;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseEvent;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::watch;
use toml::Value as TomlValue;

const EXTERNAL_EDITOR_HINT: &str = "Save and close external editor to continue.";
const SHIFT_MOUSE_BYPASS_DURATION: Duration = Duration::from_millis(1500);
const THREAD_EVENT_CHANNEL_CAPACITY: usize = 32768;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub thread_id: Option<ThreadId>,
    pub thread_name: Option<String>,
    pub update_action: Option<UpdateAction>,
    pub exit_reason: ExitReason,
}

impl AppExitInfo {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            token_usage: TokenUsage::default(),
            thread_id: None,
            thread_name: None,
            update_action: None,
            exit_reason: ExitReason::Fatal(message.into()),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AppRunControl {
    Continue,
    Exit(ExitReason),
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    UserRequested,
    Fatal(String),
}

#[derive(Debug, Clone)]
enum DeferredStartupContinuation {
    StartFresh,
    Resume(PathBuf),
    Fork(PathBuf),
}

fn session_summary(
    token_usage: TokenUsage,
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
) -> Option<SessionSummary> {
    if token_usage.is_zero() {
        return None;
    }

    let usage_line = FinalOutput::from(token_usage).to_string();
    let resume_command = adam_agent::util::resume_command(thread_name.as_deref(), thread_id);
    Some(SessionSummary {
        usage_line,
        resume_command,
    })
}

fn errors_for_cwd(cwd: &Path, response: &ListSkillsResponseEvent) -> Vec<SkillErrorInfo> {
    response
        .skills
        .iter()
        .find(|entry| entry.cwd.as_path() == cwd)
        .map(|entry| entry.errors.clone())
        .unwrap_or_default()
}

fn emit_skill_load_warnings(app_event_tx: &AppEventSender, errors: &[SkillErrorInfo]) {
    if errors.is_empty() {
        return;
    }

    let error_count = errors.len();
    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        crate::history_cell::new_warning_event(format!(
            "Skipped loading {error_count} skill(s) due to invalid SKILL.md files."
        )),
    )));

    for error in errors {
        let path = error.path.display();
        let message = error.message.as_str();
        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            crate::history_cell::new_warning_event(format!("{path}: {message}")),
        )));
    }
}

fn emit_project_config_warnings(app_event_tx: &AppEventSender, config: &Config) {
    let mut disabled_folders = Vec::new();

    for layer in config
        .config_layer_stack
        .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
    {
        let ConfigLayerSource::Project { dot_adam_folder } = &layer.name else {
            continue;
        };
        if layer.disabled_reason.is_none() {
            continue;
        }
        disabled_folders.push((
            dot_adam_folder.as_path().display().to_string(),
            layer
                .disabled_reason
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "config.toml is disabled.".to_string()),
        ));
    }

    if disabled_folders.is_empty() {
        return;
    }

    let mut message = concat!(
        "Project config.toml files are disabled in the following folders. ",
        "Settings in those files are ignored, but skills and exec policies still load.\n",
    )
    .to_string();
    for (index, (folder, reason)) in disabled_folders.iter().enumerate() {
        let display_index = index + 1;
        message.push_str(&format!("    {display_index}. {folder}\n"));
        message.push_str(&format!("       {reason}\n"));
    }

    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        history_cell::new_warning_event(message),
    )));
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSummary {
    usage_line: String,
    resume_command: Option<String>,
}

#[derive(Debug, Clone)]
struct ThreadEventSnapshot {
    session_configured: Option<Event>,
    events: Vec<Event>,
}

#[derive(Debug)]
struct ThreadEventStore {
    session_configured: Option<Event>,
    buffer: VecDeque<Event>,
    user_message_ids: HashSet<String>,
    capacity: usize,
    active: bool,
}

impl ThreadEventStore {
    fn new(capacity: usize) -> Self {
        Self {
            session_configured: None,
            buffer: VecDeque::new(),
            user_message_ids: HashSet::new(),
            capacity,
            active: false,
        }
    }

    fn new_with_session_configured(capacity: usize, event: Event) -> Self {
        let mut store = Self::new(capacity);
        store.session_configured = Some(event);
        store
    }

    fn push_event(&mut self, event: Event) {
        match &event.msg {
            EventMsg::SessionConfigured(_) => {
                self.session_configured = Some(event);
                return;
            }
            EventMsg::ItemCompleted(completed) => {
                if let TurnItem::UserMessage(item) = &completed.item {
                    if !event.id.is_empty() && self.user_message_ids.contains(&event.id) {
                        return;
                    }
                    let legacy = Event {
                        id: event.id,
                        msg: item.as_legacy_event(),
                    };
                    self.push_legacy_event(legacy);
                    return;
                }
            }
            _ => {}
        }

        self.push_legacy_event(event);
    }

    fn push_legacy_event(&mut self, event: Event) {
        if let EventMsg::UserMessage(_) = &event.msg
            && !event.id.is_empty()
            && !self.user_message_ids.insert(event.id.clone())
        {
            return;
        }
        self.buffer.push_back(event);
        if self.buffer.len() > self.capacity
            && let Some(removed) = self.buffer.pop_front()
            && matches!(removed.msg, EventMsg::UserMessage(_))
            && !removed.id.is_empty()
        {
            self.user_message_ids.remove(&removed.id);
        }
    }

    fn snapshot(&self) -> ThreadEventSnapshot {
        ThreadEventSnapshot {
            session_configured: self.session_configured.clone(),
            events: self.buffer.iter().cloned().collect(),
        }
    }
}

#[derive(Debug)]
struct ThreadEventChannel {
    sender: mpsc::Sender<Event>,
    receiver: Option<mpsc::Receiver<Event>>,
    store: Arc<Mutex<ThreadEventStore>>,
}

impl ThreadEventChannel {
    fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new(capacity))),
        }
    }

    fn new_with_session_configured(capacity: usize, event: Event) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Some(receiver),
            store: Arc::new(Mutex::new(ThreadEventStore::new_with_session_configured(
                capacity, event,
            ))),
        }
    }
}

fn should_show_model_migration_prompt(
    current_model: &str,
    target_model: &str,
    seen_migrations: &BTreeMap<String, String>,
    available_models: &[ModelPreset],
) -> bool {
    if target_model == current_model {
        return false;
    }

    if let Some(seen_target) = seen_migrations.get(current_model)
        && seen_target == target_model
    {
        return false;
    }

    if available_models
        .iter()
        .any(|preset| preset.model == current_model && preset.upgrade.is_some())
    {
        return true;
    }

    if available_models
        .iter()
        .any(|preset| preset.upgrade.as_ref().map(|u| u.id.as_str()) == Some(target_model))
    {
        return true;
    }

    false
}

fn migration_prompt_hidden(config: &Config, migration_config_key: &str) -> bool {
    match migration_config_key {
        HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG => config
            .notices
            .hide_gpt_5_1_codex_max_migration_prompt
            .unwrap_or(false),
        HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG => {
            config.notices.hide_gpt5_1_migration_prompt.unwrap_or(false)
        }
        _ => false,
    }
}

fn target_preset_for_upgrade<'a>(
    available_models: &'a [ModelPreset],
    target_model: &str,
) -> Option<&'a ModelPreset> {
    available_models
        .iter()
        .find(|preset| preset.model == target_model)
}

async fn handle_model_migration_prompt_if_needed(
    tui: &mut tui::Tui,
    config: &mut Config,
    model: &str,
    app_event_tx: &AppEventSender,
    available_models: Vec<ModelPreset>,
) -> Option<AppExitInfo> {
    let upgrade = available_models
        .iter()
        .find(|preset| preset.model == model)
        .and_then(|preset| preset.upgrade.as_ref());

    if let Some(ModelUpgrade {
        id: target_model,
        reasoning_effort_mapping,
        migration_config_key,
        model_link,
        upgrade_copy,
        migration_markdown,
    }) = upgrade
    {
        if migration_prompt_hidden(config, migration_config_key.as_str()) {
            return None;
        }

        let target_model = target_model.to_string();
        if !should_show_model_migration_prompt(
            model,
            &target_model,
            &config.notices.model_migrations,
            &available_models,
        ) {
            return None;
        }

        let current_preset = available_models.iter().find(|preset| preset.model == model);
        let target_preset = target_preset_for_upgrade(&available_models, &target_model);
        let target_preset = target_preset?;
        let target_display_name = target_preset.display_name.clone();
        let heading_label = if target_display_name == model {
            target_model.clone()
        } else {
            target_display_name.clone()
        };
        let target_description =
            (!target_preset.description.is_empty()).then(|| target_preset.description.clone());
        let can_opt_out = current_preset.is_some();
        let prompt_copy = migration_copy_for_models(
            model,
            &target_model,
            model_link.clone(),
            upgrade_copy.clone(),
            migration_markdown.clone(),
            heading_label,
            target_description,
            can_opt_out,
        );
        match run_model_migration_prompt(tui, prompt_copy).await {
            ModelMigrationOutcome::Accepted => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });

                let mapped_effort = if let Some(reasoning_effort_mapping) = reasoning_effort_mapping
                    && let Some(reasoning_effort) = config.model_reasoning_effort
                {
                    reasoning_effort_mapping
                        .get(&reasoning_effort)
                        .cloned()
                        .or(config.model_reasoning_effort)
                } else {
                    config.model_reasoning_effort
                };

                config.model = Some(target_model.clone());
                config.model_reasoning_effort = mapped_effort;
                app_event_tx.send(AppEvent::PersistModelSelection {
                    model: target_model.clone(),
                    provider_id: None,
                    effort: mapped_effort,
                });
            }
            ModelMigrationOutcome::Rejected => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });
            }
            ModelMigrationOutcome::Exit => {
                return Some(AppExitInfo {
                    token_usage: TokenUsage::default(),
                    thread_id: None,
                    thread_name: None,
                    update_action: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
        }
    }

    None
}

pub(crate) struct App {
    pub(crate) server: Arc<ThreadManager>,
    pub(crate) otel_manager: OtelManager,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_widget: ChatWidget,
    pub(crate) auth_manager: Arc<AuthManager>,
    /// Config is stored here so we can recreate ChatWidgets as needed.
    pub(crate) config: Config,
    pub(crate) active_profile: Option<String>,
    cli_kv_overrides: Vec<(String, TomlValue)>,
    harness_overrides: ConfigOverrides,
    runtime_approval_policy_override: Option<AskForApproval>,
    runtime_sandbox_policy_override: Option<SandboxPolicy>,

    pub(crate) file_search: FileSearchManager,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,

    // Pager overlay state (Transcript or Static like Diff)
    pub(crate) overlay: Option<Overlay>,

    pub(crate) enhanced_keys_supported: bool,

    /// Controls the animation thread that sends CommitTick events.
    pub(crate) commit_anim_running: Arc<AtomicBool>,

    // Esc-backtracking state grouped
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    /// When set, the next draw re-renders the transcript after a rollback.
    pub(crate) backtrack_render_pending: bool,
    pub(crate) feedback: adam_feedback::CodexFeedback,
    /// Set when the user confirms an update; propagated on exit.
    pub(crate) pending_update_action: Option<UpdateAction>,

    /// Ignore the next ShutdownComplete event when we're intentionally
    /// stopping a thread (e.g., before starting a new one).
    suppress_shutdown_complete: bool,

    windows_sandbox: WindowsSandboxState,
    shift_mouse_bypass_active: bool,
    shift_mouse_bypass_restore_at: Option<Instant>,

    thread_event_channels: HashMap<ThreadId, ThreadEventChannel>,
    agent_picker_threads: HashMap<ThreadId, AgentPickerThreadEntry>,
    active_thread_id: Option<ThreadId>,
    active_thread_rx: Option<mpsc::Receiver<Event>>,
    thread_created_rx: broadcast::Receiver<ThreadId>,
    listen_for_threads: bool,
    primary_thread_id: Option<ThreadId>,
    primary_session_configured: Option<SessionConfiguredEvent>,
    pending_primary_events: VecDeque<Event>,
    review_parent_by_child: HashMap<ThreadId, ThreadId>,
    non_git_changelog_baselines: HashMap<PathBuf, Arc<NonGitBaselineTracker>>,
    provider_config_modal: Option<ProviderConfigModalState>,
    project_trust_modal: Option<ProjectTrustModal>,
    identity_modal: Option<IdentityModal>,
    model_selection_modal: Option<ModelSelectionModal>,
    experimental_features_modal: Option<ExperimentalFeaturesModal>,
    mcp_tools_modal: Option<McpToolsModal>,
    pending_mcp_tools_modal_responses: usize,
    skills_modal: Option<SkillsModal>,
    pending_skills_modal_open: bool,
    approval_mode_modal: Option<ApprovalModeModal>,
    agent_selection_modal: Option<AgentSelectionModal>,
    review_modal: Option<ReviewModal>,
    pending_startup_trust_prompt: bool,
    deferred_initial_user_message: Option<UserMessage>,
    deferred_startup_continuation: Option<DeferredStartupContinuation>,
}

struct ProviderConfigModalState {
    mode: ProviderConfigModalMode,
    modal: ProviderConfigModal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderConfigModalMode {
    Startup,
    InSession,
}

#[derive(Default)]
struct WindowsSandboxState {
    setup_started_at: Option<Instant>,
    // One-shot suppression of the next world-writable scan after user confirmation.
    skip_world_writable_scan_once: bool,
}

#[derive(Debug)]
struct NonGitBaselineTracker {
    result: watch::Sender<Option<Result<Arc<DirectorySnapshot>, String>>>,
}

impl NonGitBaselineTracker {
    fn store(&self, result: Result<Arc<DirectorySnapshot>, String>) {
        self.result.send_replace(Some(result));
    }

    async fn wait_ready(&self) -> Result<Arc<DirectorySnapshot>, String> {
        let mut receiver = self.result.subscribe();

        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }

            receiver.changed().await.map_err(|_| {
                "changelog baseline tracker closed before a result was produced".to_string()
            })?;
        }
    }
}

impl Default for NonGitBaselineTracker {
    fn default() -> Self {
        let (result, _) = watch::channel(None);
        Self { result }
    }
}

fn detached_review_session_source(
    parent_thread_id: ThreadId,
    parent_session_source: &SessionSource,
) -> SessionSource {
    let depth = match parent_session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { depth, .. }) => {
            (*depth).saturating_add(1)
        }
        _ => 1,
    };
    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth,
        agent_nickname: None,
        agent_role: Some("review".to_string()),
    })
}

fn should_mirror_detached_review_event(msg: &EventMsg) -> bool {
    matches!(
        msg,
        EventMsg::TurnStarted(_)
            | EventMsg::TurnComplete(_)
            | EventMsg::TurnAborted(_)
            | EventMsg::Error(_)
            | EventMsg::EnteredReviewMode(_)
            | EventMsg::ExitedReviewMode(_)
    )
}

fn is_terminal_detached_review_event(msg: &EventMsg) -> bool {
    matches!(
        msg,
        EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) | EventMsg::Error(_)
    )
}

fn agent_status_from_lifecycle_event(msg: &EventMsg) -> Option<AgentStatus> {
    match msg {
        EventMsg::TurnStarted(_) => Some(AgentStatus::Running),
        EventMsg::TurnComplete(ev) => Some(AgentStatus::Completed(ev.last_agent_message.clone())),
        EventMsg::TurnAborted(ev) => match ev.reason {
            TurnAbortReason::Interrupted => Some(AgentStatus::Interrupted),
            TurnAbortReason::Replaced | TurnAbortReason::ReviewEnded => {
                Some(AgentStatus::Errored(format!("{:?}", ev.reason)))
            }
        },
        EventMsg::Error(ev) => Some(AgentStatus::Errored(ev.message.clone())),
        EventMsg::ShutdownComplete => Some(AgentStatus::Shutdown),
        _ => None,
    }
}

fn normalize_harness_overrides_for_cwd(
    mut overrides: ConfigOverrides,
    base_cwd: &Path,
) -> Result<ConfigOverrides> {
    if overrides.additional_writable_roots.is_empty() {
        return Ok(overrides);
    }

    let mut normalized = Vec::with_capacity(overrides.additional_writable_roots.len());
    for root in overrides.additional_writable_roots.drain(..) {
        let absolute = AbsolutePathBuf::resolve_path_against_base(root, base_cwd)?;
        normalized.push(absolute.into_path_buf());
    }
    overrides.additional_writable_roots = normalized;
    Ok(overrides)
}

fn set_buddy_path(path: &[&str], value: toml_edit::Item) -> ConfigEdit {
    let mut segments = vec!["tui".to_string(), "buddy".to_string()];
    segments.extend(path.iter().map(|segment| (*segment).to_string()));
    ConfigEdit::SetPath { segments, value }
}

fn buddy_success_message(config: &TuiBuddy) -> String {
    match config
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        Some(name) => {
            let species = config
                .species
                .map(|species| species.to_string())
                .unwrap_or_else(|| "buddy".to_string());
            if config.enabled && !config.muted {
                format!("Buddy ready: {name} the {species}")
            } else if config.muted {
                format!("Buddy muted: {name} the {species}")
            } else {
                format!("Buddy hidden: {name} the {species}")
            }
        }
        None => "Buddy settings updated".to_string(),
    }
}

impl App {
    async fn ensure_non_git_changelog_baseline(&mut self, cwd: PathBuf) -> Result<(), String> {
        if self.non_git_changelog_baselines.contains_key(&cwd) {
            return Ok(());
        }

        if git_repo_root(&cwd)
            .await
            .map_err(|err| err.to_string())?
            .is_some()
        {
            return Ok(());
        }

        let tracker = Arc::new(NonGitBaselineTracker::default());
        self.non_git_changelog_baselines
            .insert(cwd.clone(), tracker.clone());

        tokio::spawn(async move {
            let result = crate::changelog::capture_directory_snapshot(cwd)
                .await
                .map(Arc::new)
                .map_err(|err| err.to_string());
            tracker.store(result);
        });

        Ok(())
    }

    async fn request_changelog(&mut self) {
        let cwd = self.config.cwd.clone();
        let tx = self.app_event_tx.clone();

        match git_repo_root(&cwd).await {
            Ok(Some(_)) => {
                tokio::spawn(async move {
                    let result = get_git_changelog(&cwd)
                        .await
                        .and_then(|output| {
                            output.ok_or_else(|| io::Error::other("git changelog unavailable"))
                        })
                        .map_err(|err| err.to_string());
                    tx.send(AppEvent::ChangelogResult(result));
                });
                return;
            }
            Ok(None) => {}
            Err(err) => {
                self.app_event_tx
                    .send(AppEvent::ChangelogResult(Err(err.to_string())));
                return;
            }
        }

        if let Err(err) = self.ensure_non_git_changelog_baseline(cwd.clone()).await {
            self.app_event_tx.send(AppEvent::ChangelogResult(Err(err)));
            return;
        }

        let Some(tracker) = self.non_git_changelog_baselines.get(&cwd).cloned() else {
            self.app_event_tx
                .send(AppEvent::ChangelogResult(Err(format!(
                    "missing changelog baseline for {}",
                    cwd.display()
                ))));
            return;
        };

        tokio::spawn(async move {
            let result = match tracker.wait_ready().await {
                Ok(baseline) => get_non_git_changelog(&cwd, baseline.as_ref())
                    .await
                    .map_err(|err| err.to_string()),
                Err(err) => Err(err),
            };
            tx.send(AppEvent::ChangelogResult(result));
        });
    }

    fn insert_history_cell(&mut self, cell: Box<dyn HistoryCell>, tui: &mut tui::Tui) {
        let cell: Arc<dyn HistoryCell> = cell.into();
        self.insert_history_cell_arc(cell, tui);
    }

    fn insert_history_cell_for_thread(
        &mut self,
        thread_id: ThreadId,
        cell: Box<dyn HistoryCell>,
        tui: &mut tui::Tui,
    ) {
        let cell: Arc<dyn HistoryCell> = cell.into();
        if self.insert_history_cell_arc_for_thread(thread_id, cell.clone()) {
            tui.frame_requester().schedule_frame();
        }
    }

    fn insert_history_cell_arc_for_thread(
        &mut self,
        thread_id: ThreadId,
        cell: Arc<dyn HistoryCell>,
    ) -> bool {
        if self.chat_widget.thread_id() != Some(thread_id) {
            return false;
        }

        self.insert_history_cell_state(cell.clone());
        true
    }

    fn insert_history_cell_arc(&mut self, cell: Arc<dyn HistoryCell>, tui: &mut tui::Tui) {
        self.insert_history_cell_state(cell.clone());
        tui.frame_requester().schedule_frame();
    }

    fn insert_history_cell_state(&mut self, cell: Arc<dyn HistoryCell>) {
        if let Some(Overlay::Transcript(t)) = &mut self.overlay {
            t.insert_cell(cell.clone());
        }
        self.transcript_cells.push(cell.clone());
        self.chat_widget.insert_transcript_cell(cell);
    }

    pub fn chatwidget_init_for_forked_or_resumed_thread(
        &self,
        tui: &mut tui::Tui,
        cfg: adam_agent::config::Config,
    ) -> crate::chatwidget::ChatWidgetInit {
        crate::chatwidget::ChatWidgetInit {
            config: cfg,
            thread_manager: self.server.clone(),
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            // Fork/resume bootstraps here don't carry any prefilled message content.
            initial_user_message: None,
            enhanced_keys_supported: self.enhanced_keys_supported,
            auth_manager: self.auth_manager.clone(),
            feedback: self.feedback.clone(),
            is_first_run: false,
            startup: crate::chatwidget::ChatWidgetStartup::Configured {
                model: Some(self.chat_widget.current_model().to_string()),
            },
            otel_manager: self.otel_manager.clone(),
        }
    }

    async fn rebuild_config_for_cwd(&self, cwd: PathBuf) -> Result<Config> {
        let mut overrides = self.harness_overrides.clone();
        overrides.cwd = Some(cwd.clone());
        let cwd_display = cwd.display().to_string();
        let config = ConfigBuilder::default()
            .adam_home(self.config.adam_home.clone())
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(overrides)
            .build()
            .await
            .wrap_err_with(|| format!("Failed to rebuild config for cwd {cwd_display}"))?;
        Ok(config)
    }

    async fn resolve_startup_model(
        tui: &mut tui::Tui,
        config: &mut Config,
        thread_manager: &ThreadManager,
        app_event_tx: &AppEventSender,
    ) -> std::result::Result<Option<String>, AppExitInfo> {
        if config.provider_config_required {
            return Ok(None);
        }

        let mut resolved_model = thread_manager
            .get_default_model(&config.model, config, CatalogRefreshStrategy::Offline)
            .await
            .map_err(|err| AppExitInfo::fatal(err.to_string()))?;
        let available_models = thread_manager
            .list_models(config, CatalogRefreshStrategy::Offline)
            .await;
        if let Some(exit_info) = handle_model_migration_prompt_if_needed(
            tui,
            config,
            resolved_model.as_str(),
            app_event_tx,
            available_models,
        )
        .await
        {
            return Err(exit_info);
        }
        if let Some(updated_model) = config.model.clone() {
            resolved_model = updated_model;
        }

        Ok(Some(resolved_model))
    }

    async fn restart_chat_after_project_trust(
        &mut self,
        tui: &mut tui::Tui,
        mut config: Config,
    ) -> std::result::Result<AppRunControl, AppExitInfo> {
        let continuation = self
            .deferred_startup_continuation
            .clone()
            .unwrap_or(DeferredStartupContinuation::StartFresh);
        let thread_manager = Arc::new(ThreadManager::new(
            config.adam_home.clone(),
            self.auth_manager.clone(),
            config.model_provider_id.as_str(),
            config.model_provider.clone(),
            SessionSource::Cli,
        ));
        let model = match Self::resolve_startup_model(
            tui,
            &mut config,
            thread_manager.as_ref(),
            &self.app_event_tx,
        )
        .await
        {
            Ok(model) => model,
            Err(exit_info) => return Ok(AppRunControl::Exit(exit_info.exit_reason)),
        };
        if let Err(err) = self
            .replace_chat_after_project_trust(
                config,
                thread_manager,
                model,
                tui.frame_requester(),
                continuation,
            )
            .await
        {
            self.chat_widget.add_error_message(err);
            self.project_trust_modal = Some(ProjectTrustModal::new(self.config.cwd.clone()));
        }
        Ok(AppRunControl::Continue)
    }

    async fn replace_chat_after_project_trust(
        &mut self,
        config: Config,
        thread_manager: Arc<ThreadManager>,
        model: Option<String>,
        frame_requester: tui::FrameRequester,
        continuation: DeferredStartupContinuation,
    ) -> std::result::Result<(), String> {
        let otel_manager = OtelManager::new(
            ThreadId::new(),
            model.as_deref().unwrap_or("No provider configured"),
            model.as_deref().unwrap_or("No provider configured"),
            None,
            None,
            None,
            config.otel.log_user_prompt,
            adam_agent::terminal::user_agent(),
            SessionSource::Cli,
        );

        let initial_user_message = self.deferred_initial_user_message.clone();
        let startup = if config.provider_config_required {
            crate::chatwidget::ChatWidgetStartup::NeedsProviderConfig { auto_open: false }
        } else {
            crate::chatwidget::ChatWidgetStartup::Configured {
                model: model.clone(),
            }
        };
        let make_init =
            |initial_user_message: Option<UserMessage>| crate::chatwidget::ChatWidgetInit {
                config: config.clone(),
                thread_manager: thread_manager.clone(),
                frame_requester: frame_requester.clone(),
                app_event_tx: self.app_event_tx.clone(),
                initial_user_message,
                enhanced_keys_supported: self.enhanced_keys_supported,
                auth_manager: self.auth_manager.clone(),
                feedback: self.feedback.clone(),
                is_first_run: false,
                startup: startup.clone(),
                otel_manager: otel_manager.clone(),
            };
        let chat_widget = match continuation {
            DeferredStartupContinuation::StartFresh => {
                ChatWidget::new(make_init(initial_user_message))
            }
            DeferredStartupContinuation::Resume(path) => {
                let path_display = path.display();
                let resumed = thread_manager
                    .resume_thread_from_rollout(
                        config.clone(),
                        path.clone(),
                        self.auth_manager.clone(),
                    )
                    .await
                    .map_err(|err| {
                        format!("Failed to resume session from {path_display}: {err}")
                    })?;
                ChatWidget::new_from_existing(
                    crate::chatwidget::ChatWidgetInit {
                        startup: crate::chatwidget::ChatWidgetStartup::Configured {
                            model: config.model.clone(),
                        },
                        ..make_init(initial_user_message)
                    },
                    resumed.thread,
                    resumed.session_configured,
                )
            }
            DeferredStartupContinuation::Fork(path) => {
                let path_display = path.display();
                let forked = thread_manager
                    .fork_thread(usize::MAX, config.clone(), path.clone())
                    .await
                    .map_err(|err| format!("Failed to fork session from {path_display}: {err}"))?;
                ChatWidget::new_from_existing(
                    crate::chatwidget::ChatWidgetInit {
                        startup: crate::chatwidget::ChatWidgetStartup::Configured {
                            model: config.model.clone(),
                        },
                        ..make_init(initial_user_message)
                    },
                    forked.thread,
                    forked.session_configured,
                )
            }
        };
        self.server = thread_manager;
        self.thread_created_rx = self.server.subscribe_thread_created();
        self.listen_for_threads = true;
        self.otel_manager = otel_manager;
        self.config = config;
        self.active_profile = self.config.active_profile.clone();
        self.file_search =
            FileSearchManager::new(self.config.cwd.clone(), self.app_event_tx.clone());
        self.chat_widget = chat_widget;
        self.reset_thread_event_state();
        self.runtime_approval_policy_override = None;
        self.runtime_sandbox_policy_override = None;
        self.pending_startup_trust_prompt = false;
        self.deferred_initial_user_message = None;
        self.deferred_startup_continuation = None;
        Ok(())
    }

    async fn apply_project_trust_selection(
        &mut self,
        tui: &mut tui::Tui,
        trust_level: TrustLevel,
    ) -> AppRunControl {
        let target = resolve_root_git_project_for_trust(&self.config.cwd)
            .unwrap_or_else(|| self.config.cwd.clone());
        if let Err(err) = set_project_trust_level(&self.config.adam_home, &target, trust_level) {
            let target_display = target.display();
            tracing::error!(%err, target = %target_display, "failed to persist project trust");
            self.chat_widget
                .add_error_message(format!("Failed to save trust for {target_display}: {err}"));
            self.project_trust_modal = Some(ProjectTrustModal::new(self.config.cwd.clone()));
            return AppRunControl::Continue;
        }

        let cwd = self.config.cwd.clone();
        match self.rebuild_config_for_cwd(cwd).await {
            Ok(config) => {
                match self.restart_chat_after_project_trust(tui, config).await {
                    Ok(AppRunControl::Continue) => {}
                    Ok(exit) => return exit,
                    Err(exit_info) => return AppRunControl::Exit(exit_info.exit_reason),
                }
                let message = match trust_level {
                    TrustLevel::Trusted => "Project trusted.",
                    TrustLevel::Untrusted => {
                        "Project will require approval for edits and commands."
                    }
                };
                self.chat_widget.add_info_message(message.to_string(), None);
            }
            Err(err) => {
                tracing::error!(%err, "failed to reload config after project trust selection");
                self.chat_widget.add_error_message(format!(
                    "Saved project trust, but failed to reload configuration: {err}"
                ));
                self.project_trust_modal = Some(ProjectTrustModal::new(self.config.cwd.clone()));
            }
        }
        AppRunControl::Continue
    }

    async fn persist_buddy_config(&mut self, edit: BuddyConfigEdit) {
        let mut next = self.config.tui_buddy.clone();
        let mut edits: Vec<ConfigEdit> = Vec::new();

        match edit {
            BuddyConfigEdit::Enabled(enabled) => {
                next.enabled = enabled;
                edits.push(set_buddy_path(&["enabled"], toml_edit::value(enabled)));
            }
            BuddyConfigEdit::Muted(muted) => {
                next.muted = muted;
                edits.push(set_buddy_path(&["muted"], toml_edit::value(muted)));
            }
            BuddyConfigEdit::ObserverEnabled(enabled) => {
                next.observer.enabled = enabled;
                edits.push(set_buddy_path(
                    &["observer", "enabled"],
                    toml_edit::value(enabled),
                ));
            }
        }

        match ConfigEditsBuilder::new(&self.config.adam_home)
            .with_edits(edits)
            .apply()
            .await
        {
            Ok(()) => {
                if let Some(thread_id) = self.chat_widget.thread_id() {
                    match self.server.get_thread(thread_id).await {
                        Ok(thread) => thread.update_tui_buddy(next.clone()).await,
                        Err(err) => {
                            tracing::warn!(%err, "failed to update active thread buddy settings");
                        }
                    }
                }
                self.config.tui_buddy = next.clone();
                self.chat_widget.set_buddy_config(next.clone());
                self.chat_widget
                    .add_info_message(buddy_success_message(&next), None);
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to persist buddy config");
                self.chat_widget
                    .add_error_message(format!("Failed to save buddy settings: {err}"));
            }
        }
    }

    fn apply_runtime_policy_overrides(&mut self, config: &mut Config) {
        if let Some(policy) = self.runtime_approval_policy_override.as_ref()
            && let Err(err) = config.approval_policy.set(*policy)
        {
            tracing::warn!(%err, "failed to carry forward approval policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward approval policy override: {err}"
            ));
        }
        if let Some(policy) = self.runtime_sandbox_policy_override.as_ref()
            && let Err(err) = config.sandbox_policy.set(policy.clone())
        {
            tracing::warn!(%err, "failed to carry forward sandbox policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward sandbox policy override: {err}"
            ));
        }
    }

    async fn shutdown_current_thread(&mut self) {
        if let Some(thread_id) = self.chat_widget.thread_id() {
            // Clear any in-flight rollback guard when switching threads.
            self.backtrack.pending_rollback = None;
            self.suppress_shutdown_complete = true;
            self.chat_widget.submit_op(Op::Shutdown);
            self.server.remove_thread(&thread_id).await;
        }
    }

    async fn reload_runtime_provider_config(
        &mut self,
        provider_id: &str,
        model: &str,
    ) -> std::result::Result<(), String> {
        let mut reloaded = self
            .rebuild_config_for_cwd(self.config.cwd.clone())
            .await
            .map_err(|err| err.to_string())?;
        self.apply_runtime_policy_overrides(&mut reloaded);

        let provider = reloaded
            .model_providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| {
                format!("saved provider `{provider_id}` was not found after reloading config")
            })?;

        self.config.config_layer_stack = reloaded.config_layer_stack.clone();
        self.config.config_profiles = reloaded.config_profiles.clone();
        self.config.model_providers = reloaded.model_providers.clone();
        self.config.provider_config_required = reloaded.provider_config_required;
        self.activate_runtime_provider(provider_id, provider, model, true)
            .await;

        Ok(())
    }

    fn resolve_model_provider_for_model(&self, model: &str) -> std::result::Result<String, String> {
        ModelsJson::load_from_adam_home(&self.config.adam_home)
            .map_err(|err| format!("Failed to load models.json for model selection: {err}"))?
            .resolve_model_provider_for_model(model)
            .map(|provider_id| provider_id.unwrap_or_else(|| self.config.model_provider_id.clone()))
            .map_err(|err| err.to_string())
    }

    fn runtime_provider_for_id(
        &self,
        provider_id: &str,
    ) -> std::result::Result<RuntimeEndpoint, String> {
        if provider_id == self.config.model_provider_id {
            return Ok(self.config.model_provider.clone());
        }

        self.config
            .model_providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| format!("model provider `{provider_id}` was not found"))
    }

    async fn activate_runtime_provider(
        &mut self,
        provider_id: &str,
        provider: RuntimeEndpoint,
        model: &str,
        refresh_context_limits: bool,
    ) {
        let provider_changed = self.config.model_provider_id != provider_id;
        let model_changed = self.chat_widget.current_model() != model;
        if let Some(thread_id) = self.chat_widget.thread_id() {
            match self.server.get_thread(thread_id).await {
                Ok(thread) => {
                    thread
                        .switch_provider_and_model(
                            provider_id.to_string(),
                            provider.clone(),
                            model.to_string(),
                        )
                        .await;
                }
                Err(_) => {
                    self.server
                        .switch_model_provider(provider_id, provider.clone())
                        .await;
                }
            }
        } else {
            self.server
                .switch_model_provider(provider_id, provider.clone())
                .await;
        }

        if refresh_context_limits || provider_changed || model_changed {
            match self.config.resolve_model_context_limits(provider_id, model) {
                Ok((model_context_window, model_auto_compact_token_limit)) => {
                    self.config.model_context_window = model_context_window;
                    self.config.model_auto_compact_token_limit = model_auto_compact_token_limit;
                }
                Err(err) => {
                    tracing::warn!(
                        %err,
                        provider_id,
                        model,
                        "failed to resolve target model context limits; clearing stale learned values"
                    );
                    self.config.model_context_window = None;
                    self.config.model_auto_compact_token_limit = None;
                }
            }
        }
        self.config.model_provider_id = provider_id.to_string();
        self.config.model_provider = provider;
        self.config.model = Some(model.to_string());
        self.chat_widget.sync_provider_config(&self.config, true);
        self.chat_widget.set_model(model);
    }

    fn apply_model_selection_to_runtime(
        &mut self,
        model: &str,
        effort: Option<ReasoningEffortConfig>,
    ) {
        self.chat_widget.submit_op(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: Some(model.to_string()),
            effort: Some(effort),
            summary: None,
            identity: None,
            personality: None,
        });
        self.chat_widget.set_model(model);
        self.on_update_reasoning_effort(effort);
    }

    async fn persist_model_selection(
        &mut self,
        model: String,
        provider_id: Option<String>,
        effort: Option<ReasoningEffortConfig>,
    ) -> std::result::Result<String, String> {
        let provider_id = match provider_id {
            Some(provider_id) => {
                self.runtime_provider_for_id(&provider_id)?;
                provider_id
            }
            None => self.resolve_model_provider_for_model(&model)?,
        };
        let provider = self.runtime_provider_for_id(&provider_id)?;

        self.activate_runtime_provider(&provider_id, provider, &model, true)
            .await;
        self.apply_model_selection_to_runtime(&model, effort);
        let model_ref = if provider_id.contains('.') {
            ModelRef::parse(&format!("{provider_id}:{model}"))
                .map_err(|err| format!("Invalid model selection: {err}"))?
        } else {
            ModelRef::new(provider_id.as_str(), "main", model.as_str())
        };
        let save_result = AdamStateStore::new(&self.config.adam_home)
            .set_last_selected_model(&model_ref, effort, None)
            .map_err(anyhow::Error::from);
        if let Err(err) = save_result {
            return Err(format!(
                "Failed to save default model: {err}. Switched the current session to model `{model}` using provider `{provider_id}`."
            ));
        }

        self.reload_runtime_provider_config(&provider_id, &model)
            .await
            .map_err(|err| {
                format!(
                    "Saved model `{model}` with provider `{provider_id}`, but failed to activate it in this session: {err}. Restart Adam to use the updated settings."
                )
            })?;
        self.apply_model_selection_to_runtime(&model, effort);

        Ok(provider_id)
    }

    async fn handle_custom_provider_configured(
        &mut self,
        tui: Option<&mut tui::Tui>,
        config: CustomProviderConfig,
    ) -> AppRunControl {
        let provider_id = custom_provider_ref(&config);
        let provider_label = display_model_provider_ref(&provider_id);
        let model = config.model.clone();

        let was_startup_provider_modal = self
            .provider_config_modal
            .as_ref()
            .is_some_and(|state| state.mode == ProviderConfigModalMode::Startup);
        self.provider_config_modal = None;
        self.chat_widget.dismiss_active_view();

        match persist_custom_provider_files(&self.config.adam_home, &config) {
            Ok(()) => {
                if let Err(err) = self
                    .reload_runtime_provider_config(&provider_id, &model)
                    .await
                {
                    self.chat_widget.add_error_message(format!(
                        "Saved provider `{provider_label}` with model `{model}`, but failed to activate it in this session: {err}. Restart Adam to use the updated settings."
                    ));
                    return AppRunControl::Continue;
                }

                if was_startup_provider_modal && self.pending_startup_trust_prompt {
                    self.project_trust_modal =
                        Some(ProjectTrustModal::new(self.config.cwd.clone()));
                    return AppRunControl::Continue;
                }

                if was_startup_provider_modal && self.deferred_startup_continuation.is_some() {
                    if let Some(tui) = tui {
                        match self
                            .restart_chat_after_project_trust(tui, self.config.clone())
                            .await
                        {
                            Ok(control) => return control,
                            Err(exit_info) => return AppRunControl::Exit(exit_info.exit_reason),
                        }
                    } else {
                        self.chat_widget.add_error_message(
                            "Saved provider, but could not start a session without TUI state."
                                .to_string(),
                        );
                        return AppRunControl::Continue;
                    }
                }

                if self.chat_widget.thread_id().is_none() {
                    match self.server.start_thread(self.config.clone()).await {
                        Ok(new_thread) => {
                            self.chat_widget.attach_started_thread(
                                new_thread.thread,
                                new_thread.session_configured,
                            );
                        }
                        Err(err) => {
                            self.chat_widget.add_error_message(format!(
                                "Saved provider `{provider_label}` with model `{model}`, but failed to start a session: {err}."
                            ));
                            return AppRunControl::Continue;
                        }
                    }
                }
                self.chat_widget.add_info_message(
                    format!(
                        "Switched this session to provider `{provider_label}` with model `{model}`."
                    ),
                    Some(
                        "Future sessions will also use this provider and model by default."
                            .to_string(),
                    ),
                );
            }
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to save provider `{provider_label}` with model `{model}`: {err}."
                ));
            }
        }
        AppRunControl::Continue
    }

    fn ensure_thread_channel(&mut self, thread_id: ThreadId) -> &mut ThreadEventChannel {
        self.thread_event_channels
            .entry(thread_id)
            .or_insert_with(|| ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY))
    }

    async fn set_thread_active(&mut self, thread_id: ThreadId, active: bool) {
        if let Some(channel) = self.thread_event_channels.get_mut(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active = active;
        }
    }

    async fn activate_thread_channel(&mut self, thread_id: ThreadId) {
        if self.active_thread_id.is_some() {
            return;
        }
        self.set_thread_active(thread_id, true).await;
        let receiver = if let Some(channel) = self.thread_event_channels.get_mut(&thread_id) {
            channel.receiver.take()
        } else {
            None
        };
        self.active_thread_id = Some(thread_id);
        self.active_thread_rx = receiver;
    }

    async fn store_active_thread_receiver(&mut self) {
        let Some(active_id) = self.active_thread_id else {
            return;
        };
        let Some(receiver) = self.active_thread_rx.take() else {
            return;
        };
        if let Some(channel) = self.thread_event_channels.get_mut(&active_id) {
            let mut store = channel.store.lock().await;
            store.active = false;
            channel.receiver = Some(receiver);
        }
    }

    async fn activate_thread_for_replay(
        &mut self,
        thread_id: ThreadId,
    ) -> Option<(mpsc::Receiver<Event>, ThreadEventSnapshot)> {
        let channel = self.thread_event_channels.get_mut(&thread_id)?;
        let receiver = channel.receiver.take()?;
        let mut store = channel.store.lock().await;
        store.active = true;
        let snapshot = store.snapshot();
        Some((receiver, snapshot))
    }

    async fn clear_active_thread(&mut self) {
        if let Some(active_id) = self.active_thread_id.take() {
            self.set_thread_active(active_id, false).await;
        }
        self.active_thread_rx = None;
    }

    async fn enqueue_thread_event(&mut self, thread_id: ThreadId, event: Event) -> Result<()> {
        self.sync_agent_picker_thread_lifecycle_event(thread_id, &event.msg);
        if let Some(parent_thread_id) = self.review_parent_by_child.get(&thread_id).copied()
            && should_mirror_detached_review_event(&event.msg)
        {
            self.enqueue_thread_event_without_review_mirror(parent_thread_id, event.clone())
                .await?;
            if is_terminal_detached_review_event(&event.msg) {
                self.review_parent_by_child.remove(&thread_id);
            }
        }
        self.enqueue_thread_event_without_review_mirror(thread_id, event)
            .await
    }

    async fn enqueue_thread_event_without_review_mirror(
        &mut self,
        thread_id: ThreadId,
        event: Event,
    ) -> Result<()> {
        let (sender, store) = {
            let channel = self.ensure_thread_channel(thread_id);
            (channel.sender.clone(), Arc::clone(&channel.store))
        };

        let should_send = {
            let mut guard = store.lock().await;
            guard.push_event(event.clone());
            guard.active
        };

        if should_send {
            // Never await a bounded channel send on the main TUI loop: if the receiver falls behind,
            // `send().await` can block and the UI stops drawing. If the channel is full, wait in a
            // spawned task instead.
            match sender.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    tokio::spawn(async move {
                        if let Err(err) = sender.send(event).await {
                            tracing::warn!("thread {thread_id} event channel closed: {err}");
                        }
                    });
                }
                Err(TrySendError::Closed(_)) => {
                    tracing::warn!("thread {thread_id} event channel closed");
                }
            }
        }
        Ok(())
    }

    async fn enqueue_primary_event(&mut self, event: Event) -> Result<()> {
        if let Some(thread_id) = self.primary_thread_id {
            return self.enqueue_thread_event(thread_id, event).await;
        }

        if let EventMsg::SessionConfigured(session) = &event.msg {
            let thread_id = session.session_id;
            self.primary_thread_id = Some(thread_id);
            self.primary_session_configured = Some(session.clone());
            self.ensure_thread_channel(thread_id);
            self.activate_thread_channel(thread_id).await;

            let pending = std::mem::take(&mut self.pending_primary_events);
            for pending_event in pending {
                self.enqueue_thread_event(thread_id, pending_event).await?;
            }
            self.enqueue_thread_event(thread_id, event).await?;
        } else {
            self.pending_primary_events.push_back(event);
        }
        Ok(())
    }

    async fn start_review(&mut self, review_request: ReviewRequest) -> Result<()> {
        self.review_modal = None;
        if !self.config.features.enabled(Feature::DetachedReview) {
            self.chat_widget.submit_op(Op::Review { review_request });
            return Ok(());
        }

        self.start_detached_review(review_request).await
    }

    async fn start_detached_review(&mut self, review_request: ReviewRequest) -> Result<()> {
        let Some(parent_thread_id) = self.active_thread_id.or(self.primary_thread_id) else {
            self.chat_widget
                .add_error_message("Current session is not ready to review yet.".to_string());
            return Ok(());
        };

        let parent_thread = match self.server.get_thread(parent_thread_id).await {
            Ok(thread) => thread,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to start detached review for thread {parent_thread_id}: {err}"
                ));
                return Ok(());
            }
        };
        let Some(parent_rollout_path) = parent_thread.rollout_path() else {
            self.chat_widget
                .add_error_message("Current session is not ready to review yet.".to_string());
            return Ok(());
        };
        parent_thread.flush_rollout().await;
        let parent_session_source = parent_thread.config_snapshot().await.session_source;
        let session_source =
            detached_review_session_source(parent_thread_id, &parent_session_source);

        let mut review_config = self.chat_widget.config_ref().clone();
        if let Some(review_model) = &review_config.review_model {
            review_config.model = Some(review_model.clone());
        }

        let detached_thread = match self
            .server
            .fork_thread_with_source(
                usize::MAX,
                review_config,
                parent_rollout_path,
                session_source,
            )
            .await
        {
            Ok(thread) => thread,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to create detached review thread: {err}"));
                return Ok(());
            }
        };
        let review_thread_id = detached_thread.thread_id;

        if let Err(err) = self.handle_thread_created(review_thread_id).await {
            self.chat_widget
                .add_error_message(format!("Failed to attach detached review thread: {err}"));
            return Ok(());
        }

        self.review_parent_by_child
            .insert(review_thread_id, parent_thread_id);
        if let Err(err) = detached_thread
            .thread
            .submit(Op::Review { review_request })
            .await
        {
            self.review_parent_by_child.remove(&review_thread_id);
            self.chat_widget
                .add_error_message(format!("Failed to start detached review: {err}"));
        }

        Ok(())
    }

    async fn open_agent_picker(&mut self) {
        let thread_ids: Vec<ThreadId> = self.thread_event_channels.keys().cloned().collect();
        for thread_id in thread_ids {
            match self.server.get_thread(thread_id).await {
                Ok(thread) => {
                    let session_source = thread.config_snapshot().await.session_source;
                    let status = self
                        .agent_picker_threads
                        .get(&thread_id)
                        .map(|entry| entry.status.clone())
                        .unwrap_or(AgentStatus::Running);
                    self.upsert_agent_picker_thread(
                        thread_id,
                        session_source.get_nickname(),
                        session_source.get_agent_role(),
                        status,
                        false,
                    );
                }
                Err(_) => {
                    self.mark_agent_picker_thread_closed(thread_id);
                }
            }
        }

        if self.agent_picker_threads.is_empty() {
            self.chat_widget
                .add_info_message("No agents available yet.".to_string(), None);
            return;
        }

        let mut agent_threads: Vec<(ThreadId, AgentPickerThreadEntry)> = self
            .agent_picker_threads
            .iter()
            .map(|(thread_id, entry)| (*thread_id, entry.clone()))
            .collect();
        sort_agent_picker_threads(&mut agent_threads);

        self.chat_widget.dismiss_active_view();

        let mut initial_selected_idx = None;
        let items: Vec<AgentSelectionModalItem> = agent_threads
            .iter()
            .enumerate()
            .map(|(idx, (thread_id, entry))| {
                if self.active_thread_id == Some(*thread_id) {
                    initial_selected_idx = Some(idx);
                }
                let is_primary = self.primary_thread_id == Some(*thread_id);
                let name = format_agent_picker_item_name(
                    entry.agent_nickname.as_deref(),
                    entry.agent_role.as_deref(),
                    is_primary,
                );
                AgentSelectionModalItem {
                    thread_id: *thread_id,
                    name,
                    status: entry.status.clone(),
                    is_current: self.active_thread_id == Some(*thread_id),
                    is_closed: entry.is_closed,
                }
            })
            .collect();

        self.agent_selection_modal = AgentSelectionModal::new(items, initial_selected_idx);
        self.chat_widget.request_redraw_for_ui();
    }

    fn upsert_agent_picker_thread(
        &mut self,
        thread_id: ThreadId,
        agent_nickname: Option<String>,
        agent_role: Option<String>,
        status: AgentStatus,
        is_closed: bool,
    ) {
        self.agent_picker_threads.insert(
            thread_id,
            AgentPickerThreadEntry {
                agent_nickname,
                agent_role,
                status,
                is_closed,
            },
        );
    }

    fn mark_agent_picker_thread_closed(&mut self, thread_id: ThreadId) {
        if let Some(entry) = self.agent_picker_threads.get_mut(&thread_id) {
            entry.is_closed = true;
        } else {
            self.upsert_agent_picker_thread(thread_id, None, None, AgentStatus::Shutdown, true);
        }
    }

    fn update_agent_picker_thread_status(
        &mut self,
        thread_id: ThreadId,
        agent_nickname: Option<String>,
        agent_role: Option<String>,
        status: AgentStatus,
        is_closed: bool,
    ) {
        if let Some(entry) = self.agent_picker_threads.get_mut(&thread_id) {
            if agent_nickname.is_some() {
                entry.agent_nickname = agent_nickname;
            }
            if agent_role.is_some() {
                entry.agent_role = agent_role;
            }
            entry.status = status;
            entry.is_closed = is_closed;
        } else {
            self.upsert_agent_picker_thread(
                thread_id,
                agent_nickname,
                agent_role,
                status,
                is_closed,
            );
        }
    }

    fn sync_agent_picker_thread_lifecycle_event(&mut self, thread_id: ThreadId, msg: &EventMsg) {
        let Some(status) = agent_status_from_lifecycle_event(msg) else {
            return;
        };
        let is_closed = matches!(status, AgentStatus::Shutdown);
        self.update_agent_picker_thread_status(thread_id, None, None, status, is_closed);
    }

    fn sync_agent_picker_thread_from_event(&mut self, msg: &EventMsg) {
        match msg {
            EventMsg::CollabAgentSpawnEnd(ev) => {
                if let Some(thread_id) = ev.new_thread_id {
                    self.update_agent_picker_thread_status(
                        thread_id,
                        ev.new_agent_nickname.clone(),
                        ev.new_agent_role.clone(),
                        ev.status.clone(),
                        false,
                    );
                }
            }
            EventMsg::CollabAgentInteractionEnd(ev) => {
                self.update_agent_picker_thread_status(
                    ev.receiver_thread_id,
                    ev.receiver_agent_nickname.clone(),
                    ev.receiver_agent_role.clone(),
                    ev.status.clone(),
                    false,
                );
            }
            EventMsg::CollabWaitingEnd(ev) => {
                let mut updated_thread_ids = HashSet::new();
                for entry in &ev.agent_statuses {
                    updated_thread_ids.insert(entry.agent.thread_id);
                    self.update_agent_picker_thread_status(
                        entry.agent.thread_id,
                        entry.agent.agent_nickname.clone(),
                        entry.agent.agent_role.clone(),
                        entry.status.clone(),
                        false,
                    );
                }
                for (thread_id, status) in &ev.statuses {
                    if updated_thread_ids.contains(thread_id) {
                        continue;
                    }
                    self.update_agent_picker_thread_status(
                        *thread_id,
                        None,
                        None,
                        status.clone(),
                        false,
                    );
                }
            }
            EventMsg::CollabResumeBegin(ev) => {
                self.update_agent_picker_thread_status(
                    ev.receiver_thread_id,
                    ev.receiver_agent_nickname.clone(),
                    ev.receiver_agent_role.clone(),
                    AgentStatus::Running,
                    false,
                );
            }
            EventMsg::CollabResumeEnd(ev) => {
                self.update_agent_picker_thread_status(
                    ev.receiver_thread_id,
                    ev.receiver_agent_nickname.clone(),
                    ev.receiver_agent_role.clone(),
                    ev.status.clone(),
                    false,
                );
            }
            EventMsg::CollabCloseEnd(ev) => {
                self.update_agent_picker_thread_status(
                    ev.receiver_thread_id,
                    ev.receiver_agent_nickname.clone(),
                    ev.receiver_agent_role.clone(),
                    ev.status.clone(),
                    true,
                );
            }
            _ => {}
        }
    }

    fn sidebar_agent_entries(&self) -> Vec<AgentPanelEntry> {
        let mut agent_threads: Vec<(ThreadId, AgentPickerThreadEntry)> = self
            .agent_picker_threads
            .iter()
            .filter(|(thread_id, entry)| {
                !entry.is_closed && Some(**thread_id) != self.primary_thread_id
            })
            .map(|(thread_id, entry)| (*thread_id, entry.clone()))
            .collect();
        sort_agent_picker_threads(&mut agent_threads);
        agent_threads
            .into_iter()
            .map(|(thread_id, entry)| AgentPanelEntry {
                thread_id,
                label: format_agent_picker_item_name(
                    entry.agent_nickname.as_deref(),
                    entry.agent_role.as_deref(),
                    false,
                ),
                status: entry.status,
            })
            .collect()
    }

    async fn select_agent_thread(&mut self, tui: &mut tui::Tui, thread_id: ThreadId) -> Result<()> {
        if self.active_thread_id == Some(thread_id) {
            return Ok(());
        }

        let live_thread = match self.server.get_thread(thread_id).await {
            Ok(thread) => Some(thread),
            Err(err) => {
                if self.thread_event_channels.contains_key(&thread_id) {
                    self.mark_agent_picker_thread_closed(thread_id);
                    None
                } else {
                    self.chat_widget.add_error_message(format!(
                        "Failed to attach to agent thread {thread_id}: {err}"
                    ));
                    return Ok(());
                }
            }
        };
        let is_replay_only = live_thread.is_none();

        let previous_thread_id = self.active_thread_id;
        self.store_active_thread_receiver().await;
        self.active_thread_id = None;
        let Some((receiver, snapshot)) = self.activate_thread_for_replay(thread_id).await else {
            self.chat_widget
                .add_error_message(format!("Agent thread {thread_id} is already active."));
            if let Some(previous_thread_id) = previous_thread_id {
                self.activate_thread_channel(previous_thread_id).await;
            }
            return Ok(());
        };

        self.active_thread_id = Some(thread_id);
        self.active_thread_rx = Some(receiver);

        let init = self.chatwidget_init_for_forked_or_resumed_thread(tui, self.config.clone());
        let codex_op_tx = if let Some(thread) = live_thread {
            crate::chatwidget::spawn_op_forwarder(thread)
        } else {
            let (tx, _rx) = unbounded_channel();
            tx
        };
        self.chat_widget = ChatWidget::new_with_op_sender(init, codex_op_tx);

        self.reset_for_thread_switch(tui)?;
        self.replay_thread_snapshot(snapshot);
        if is_replay_only {
            self.chat_widget.add_info_message(
                format!("Agent thread {thread_id} is closed. Replaying saved transcript."),
                None,
            );
        }
        self.drain_active_thread_events(tui).await?;

        Ok(())
    }

    fn reset_for_thread_switch(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.overlay = None;
        self.transcript_cells.clear();
        self.backtrack = BacktrackState::default();
        self.backtrack_render_pending = false;
        tui.terminal.clear_scrollback()?;
        tui.terminal.clear()?;
        Ok(())
    }

    fn reset_thread_event_state(&mut self) {
        self.thread_event_channels.clear();
        self.agent_picker_threads.clear();
        self.active_thread_id = None;
        self.active_thread_rx = None;
        self.primary_thread_id = None;
        self.pending_primary_events.clear();
        self.review_parent_by_child.clear();
    }

    async fn drain_active_thread_events(&mut self, tui: &mut tui::Tui) -> Result<()> {
        let Some(mut rx) = self.active_thread_rx.take() else {
            return Ok(());
        };

        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(event) => self.handle_codex_event_now(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if !disconnected {
            self.active_thread_rx = Some(rx);
        } else {
            self.clear_active_thread().await;
        }

        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    fn replay_thread_snapshot(&mut self, snapshot: ThreadEventSnapshot) {
        if let Some(event) = snapshot.session_configured {
            self.handle_codex_event_replay(event);
        }
        for event in snapshot.events {
            self.handle_codex_event_replay(event);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        auth_manager: Arc<AuthManager>,
        mut config: Config,
        cli_kv_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
        active_profile: Option<String>,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        feedback: adam_feedback::CodexFeedback,
        is_first_run: bool,
        show_provider_popup_on_startup: bool,
        show_trust_popup_on_startup: bool,
    ) -> Result<AppExitInfo> {
        use tokio_stream::StreamExt;
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        emit_project_config_warnings(&app_event_tx, &config);
        tui.set_notification_method(config.tui_notification_method);

        let harness_overrides =
            normalize_harness_overrides_for_cwd(harness_overrides, &config.cwd)?;
        let thread_manager = Arc::new(ThreadManager::new(
            config.adam_home.clone(),
            auth_manager.clone(),
            config.model_provider_id.as_str(),
            config.model_provider.clone(),
            SessionSource::Cli,
        ));
        let mut model = None;
        if !config.provider_config_required {
            let mut resolved_model = thread_manager
                .get_default_model(&config.model, &config, CatalogRefreshStrategy::Offline)
                .await?;
            let available_models = thread_manager
                .list_models(&config, CatalogRefreshStrategy::Offline)
                .await;
            let exit_info = handle_model_migration_prompt_if_needed(
                tui,
                &mut config,
                resolved_model.as_str(),
                &app_event_tx,
                available_models,
            )
            .await;
            if let Some(exit_info) = exit_info {
                return Ok(exit_info);
            }
            if let Some(updated_model) = config.model.clone() {
                resolved_model = updated_model;
            }
            model = Some(resolved_model);
        }

        let otel_manager = OtelManager::new(
            ThreadId::new(),
            model.as_deref().unwrap_or("No provider configured"),
            model.as_deref().unwrap_or("No provider configured"),
            None,
            None,
            None,
            config.otel.log_user_prompt,
            adam_agent::terminal::user_agent(),
            SessionSource::Cli,
        );

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let initial_user_message = crate::chatwidget::create_initial_user_message(
            initial_prompt.clone(),
            initial_images.clone(),
            // CLI prompt args are plain strings, so they don't provide element ranges.
            Vec::new(),
        );
        let defer_startup = show_provider_popup_on_startup || show_trust_popup_on_startup;
        let deferred_initial_user_message = defer_startup
            .then(|| initial_user_message.clone())
            .flatten();
        let deferred_startup_continuation = defer_startup.then(|| match &session_selection {
            SessionSelection::StartFresh | SessionSelection::Exit => {
                DeferredStartupContinuation::StartFresh
            }
            SessionSelection::Resume(path) => DeferredStartupContinuation::Resume(path.clone()),
            SessionSelection::Fork(path) => DeferredStartupContinuation::Fork(path.clone()),
        });
        let deferred_startup = crate::chatwidget::ChatWidgetStartup::Deferred;
        let needs_provider_config = crate::chatwidget::ChatWidgetStartup::NeedsProviderConfig {
            auto_open: !show_provider_popup_on_startup,
        };
        let configured_startup = crate::chatwidget::ChatWidgetStartup::Configured {
            model: model.clone(),
        };
        let startup_for_fresh = if defer_startup {
            deferred_startup.clone()
        } else if config.provider_config_required {
            needs_provider_config.clone()
        } else {
            configured_startup.clone()
        };
        let startup_for_deferred = defer_startup.then_some(deferred_startup.clone());
        let initial_message_for_startup = if defer_startup {
            None
        } else {
            initial_user_message.clone()
        };
        let chat_widget = match session_selection {
            SessionSelection::StartFresh | SessionSelection::Exit => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    thread_manager: thread_manager.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: initial_message_for_startup.clone(),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    startup: startup_for_fresh,
                    otel_manager: otel_manager.clone(),
                };
                ChatWidget::new(init)
            }
            SessionSelection::Resume(_) if startup_for_deferred.is_some() => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    thread_manager: thread_manager.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: None,
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    startup: deferred_startup.clone(),
                    otel_manager: otel_manager.clone(),
                };
                ChatWidget::new(init)
            }
            SessionSelection::Resume(path) => {
                let resumed = thread_manager
                    .resume_thread_from_rollout(config.clone(), path.clone(), auth_manager.clone())
                    .await
                    .wrap_err_with(|| {
                        let path_display = path.display();
                        format!("Failed to resume session from {path_display}")
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    thread_manager: thread_manager.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: initial_user_message.clone(),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    startup: crate::chatwidget::ChatWidgetStartup::Configured {
                        model: config.model.clone(),
                    },
                    otel_manager: otel_manager.clone(),
                };
                ChatWidget::new_from_existing(init, resumed.thread, resumed.session_configured)
            }
            SessionSelection::Fork(_) if startup_for_deferred.is_some() => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    thread_manager: thread_manager.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: None,
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    startup: deferred_startup,
                    otel_manager: otel_manager.clone(),
                };
                ChatWidget::new(init)
            }
            SessionSelection::Fork(path) => {
                let forked = thread_manager
                    .fork_thread(usize::MAX, config.clone(), path.clone())
                    .await
                    .wrap_err_with(|| {
                        let path_display = path.display();
                        format!("Failed to fork session from {path_display}")
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    thread_manager: thread_manager.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: initial_user_message.clone(),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    startup: crate::chatwidget::ChatWidgetStartup::Configured {
                        model: config.model.clone(),
                    },
                    otel_manager: otel_manager.clone(),
                };
                ChatWidget::new_from_existing(init, forked.thread, forked.session_configured)
            }
        };
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        #[cfg(not(debug_assertions))]
        let upgrade_version = crate::updates::get_upgrade_version(&config);
        let provider_config_modal =
            show_provider_popup_on_startup.then(|| ProviderConfigModalState {
                mode: ProviderConfigModalMode::Startup,
                modal: ProviderConfigModal::new(
                    config.adam_home.clone(),
                    app_event_tx.clone(),
                    tui.frame_requester(),
                ),
            });
        let project_trust_modal = (show_trust_popup_on_startup && !show_provider_popup_on_startup)
            .then(|| ProjectTrustModal::new(config.cwd.clone()));

        let mut app = Self {
            server: thread_manager.clone(),
            otel_manager: otel_manager.clone(),
            app_event_tx,
            chat_widget,
            auth_manager: auth_manager.clone(),
            config,
            active_profile,
            cli_kv_overrides,
            harness_overrides,
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search,
            enhanced_keys_supported,
            transcript_cells: Vec::new(),
            overlay: None,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: feedback.clone(),
            pending_update_action: None,
            suppress_shutdown_complete: false,
            windows_sandbox: WindowsSandboxState::default(),
            shift_mouse_bypass_active: false,
            shift_mouse_bypass_restore_at: None,
            thread_event_channels: HashMap::new(),
            agent_picker_threads: HashMap::new(),
            active_thread_id: None,
            active_thread_rx: None,
            thread_created_rx: thread_manager.subscribe_thread_created(),
            listen_for_threads: true,
            primary_thread_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            review_parent_by_child: HashMap::new(),
            non_git_changelog_baselines: HashMap::new(),
            provider_config_modal,
            project_trust_modal,
            identity_modal: None,
            model_selection_modal: None,
            experimental_features_modal: None,
            mcp_tools_modal: None,
            pending_mcp_tools_modal_responses: 0,
            skills_modal: None,
            pending_skills_modal_open: false,
            approval_mode_modal: None,
            agent_selection_modal: None,
            review_modal: None,
            pending_startup_trust_prompt: show_trust_popup_on_startup,
            deferred_initial_user_message,
            deferred_startup_continuation,
        };
        #[cfg(target_os = "windows")]
        app.maybe_prompt_windows_sandbox_enable_modal();

        if let Err(err) = app
            .ensure_non_git_changelog_baseline(app.config.cwd.clone())
            .await
        {
            tracing::warn!(
                cwd = %app.config.cwd.display(),
                %err,
                "failed to prewarm changelog baseline"
            );
        }

        // On startup, if Agent mode (workspace-write) or ReadOnly is active, warn about world-writable dirs on Windows.
        #[cfg(target_os = "windows")]
        {
            let should_check = WindowsSandboxLevel::from_config(&app.config)
                != WindowsSandboxLevel::Disabled
                && matches!(
                    app.config.sandbox_policy.get(),
                    adam_agent::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | adam_agent::protocol::SandboxPolicy::ReadOnly
                )
                && !app
                    .config
                    .notices
                    .hide_world_writable_warning
                    .unwrap_or(false);
            if should_check {
                let cwd = app.config.cwd.clone();
                let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
                let tx = app.app_event_tx.clone();
                let logs_base_dir = app.config.adam_home.clone();
                let sandbox_policy = app.config.sandbox_policy.get().clone();
                Self::spawn_world_writable_scan(cwd, env_map, logs_base_dir, sandbox_policy, tx);
            }
        }

        #[cfg(not(debug_assertions))]
        if let Some(latest_version) = upgrade_version {
            let control = app
                .handle_event(
                    tui,
                    AppEvent::InsertHistoryCell(Box::new(UpdateAvailableHistoryCell::new(
                        latest_version,
                        crate::update_action::get_update_action(),
                    ))),
                )
                .await?;
            if let AppRunControl::Exit(exit_reason) = control {
                return Ok(AppExitInfo {
                    token_usage: app.token_usage(),
                    thread_id: app.chat_widget.thread_id(),
                    thread_name: app.chat_widget.thread_name(),
                    update_action: app.pending_update_action,
                    exit_reason,
                });
            }
        }

        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        tui.frame_requester().schedule_frame();

        let exit_reason = loop {
            let control = select! {
                Some(event) = app_event_rx.recv() => {
                    app.handle_event(tui, event).await?
                }
                active = async {
                    if let Some(rx) = app.active_thread_rx.as_mut() {
                        rx.recv().await
                    } else {
                        None
                    }
                }, if app.active_thread_rx.is_some() => {
                    if let Some(event) = active {
                        app.handle_active_thread_event(tui, event)?;
                    } else {
                        app.clear_active_thread().await;
                    }
                    AppRunControl::Continue
                }
                event = tui_events.next() => {
                    if let Some(event) = event {
                        app.handle_tui_event(tui, event).await?
                    } else {
                        tracing::warn!("terminal input stream closed; shutting down active thread");
                        app.handle_event(tui, AppEvent::Exit(ExitMode::ShutdownFirst))
                            .await?
                    }
                }
                // Listen on new thread creation due to collab tools.
                created = app.thread_created_rx.recv(), if app.listen_for_threads => {
                    match created {
                        Ok(thread_id) => {
                            app.handle_thread_created(thread_id).await?;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            tracing::warn!("thread_created receiver lagged; skipping resync");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            app.listen_for_threads = false;
                        }
                    }
                    AppRunControl::Continue
                }
            };
            match control {
                AppRunControl::Continue => {}
                AppRunControl::Exit(reason) => break reason,
            }
        };
        tui.terminal.clear()?;
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            thread_id: app.chat_widget.thread_id(),
            thread_name: app.chat_widget.thread_name(),
            update_action: app.pending_update_action,
            exit_reason,
        })
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<AppRunControl> {
        if matches!(event, TuiEvent::Draw) {
            self.restore_shift_mouse_bypass_if_due(tui);
        }

        if self.provider_config_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    return self.handle_provider_config_modal_key(tui, key_event).await;
                }
                TuiEvent::Paste(pasted) => {
                    if let Some(state) = self.provider_config_modal.as_mut() {
                        state.modal.handle_paste(pasted.replace("\r", "\n"));
                        tui.frame_requester().schedule_frame();
                    }
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) => {}
            }
        } else if self.project_trust_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    return self.handle_project_trust_modal_key(tui, key_event).await;
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) | TuiEvent::Paste(_) => {}
            }
        } else if self.identity_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_identity_modal_key_event(key_event);
                    tui.frame_requester().schedule_frame();
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) | TuiEvent::Paste(_) => {}
            }
        } else if self.model_selection_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    return self
                        .handle_model_selection_modal_key_event(tui, key_event)
                        .await;
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) | TuiEvent::Paste(_) => {}
            }
        } else if self.experimental_features_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    return self
                        .handle_experimental_features_modal_key_event(tui, key_event)
                        .await;
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) | TuiEvent::Paste(_) => {}
            }
        } else if self.mcp_tools_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_mcp_tools_modal_key_event(tui, key_event);
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) | TuiEvent::Paste(_) => {}
            }
        } else if self.skills_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    return self.handle_skills_modal_key_event(tui, key_event).await;
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) | TuiEvent::Paste(_) => {}
            }
        } else if self.approval_mode_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    return self
                        .handle_approval_mode_modal_key_event(tui, key_event)
                        .await;
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) | TuiEvent::Paste(_) => {}
            }
        } else if self.agent_selection_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    return self
                        .handle_agent_selection_modal_key_event(tui, key_event)
                        .await;
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) | TuiEvent::Paste(_) => {}
            }
        } else if self.review_modal.is_some() {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_review_modal_key(tui, key_event);
                }
                TuiEvent::Paste(pasted) => {
                    if let Some(modal) = self.review_modal.as_mut() {
                        modal.handle_paste(pasted.replace("\r", "\n"));
                        tui.frame_requester().schedule_frame();
                    }
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
                TuiEvent::Mouse(_) => {}
            }
        } else if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, key_event).await;
                }
                TuiEvent::Mouse(mouse_event) => {
                    self.handle_mouse_event(tui, mouse_event).await;
                }
                TuiEvent::Paste(pasted) => {
                    // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                    // but tui-textarea expects \n. Normalize CR to LF.
                    // [tui-textarea]: https://github.com/rhysd/tui-textarea/blob/4d18622eeac13b309e0ff6a55a46ac6706da68cf/src/textarea.rs#L782-L783
                    // [iTerm2]: https://github.com/gnachman/iTerm2/blob/5d0c0d9f68523cbd0494dad5422998964a2ecd8d/sources/iTermPasteHelper.m#L206-L216
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_widget.handle_paste(pasted);
                }
                TuiEvent::Draw => {
                    self.draw_main_ui(tui)?;
                }
            }
        }
        self.maybe_open_pending_skills_modal_with_redraw(tui);
        Ok(AppRunControl::Continue)
    }

    fn draw_main_ui(&mut self, tui: &mut tui::Tui) -> Result<()> {
        if self.backtrack_render_pending {
            self.backtrack_render_pending = false;
            self.chat_widget
                .replace_transcript_cells(self.transcript_cells.clone());
        }
        self.chat_widget.prepare_for_draw();
        self.chat_widget.maybe_post_pending_notification(tui);
        if self
            .chat_widget
            .handle_paste_burst_tick(tui.frame_requester())
        {
            return Ok(());
        }
        self.chat_widget
            .set_sidebar_agents(self.sidebar_agent_entries());
        let size = tui.terminal.size()?;
        tui.draw(size.height, |frame| {
            self.chat_widget.render(frame.area(), frame.buffer);
            if let Some(state) = &self.provider_config_modal {
                state.modal.render(frame.area(), frame.buffer);
                if let Some((x, y)) = state.modal.cursor_pos(frame.area()) {
                    frame.set_cursor_position((x, y));
                }
            } else if let Some(modal) = &self.project_trust_modal {
                modal.render(frame.area(), frame.buffer);
            } else if let Some(modal) = &self.identity_modal {
                modal.render(frame.area(), frame.buffer);
            } else if let Some(modal) = &self.model_selection_modal {
                modal.render(frame.area(), frame.buffer);
            } else if let Some(modal) = &self.experimental_features_modal {
                modal.render(frame.area(), frame.buffer);
            } else if let Some(modal) = &self.mcp_tools_modal {
                modal.render(frame.area(), frame.buffer);
            } else if let Some(modal) = &self.skills_modal {
                modal.render(frame.area(), frame.buffer);
            } else if let Some(modal) = &self.approval_mode_modal {
                modal.render(frame.area(), frame.buffer);
            } else if let Some(modal) = &self.agent_selection_modal {
                modal.render(frame.area(), frame.buffer);
            } else if let Some(modal) = &self.review_modal {
                modal.render(frame.area(), frame.buffer);
                if let Some((x, y)) = modal.cursor_pos(frame.area()) {
                    frame.set_cursor_position((x, y));
                }
            } else if let Some((x, y)) = self.chat_widget.cursor_pos(frame.area()) {
                frame.set_cursor_position((x, y));
            }
        })?;
        if self.chat_widget.external_editor_state() == ExternalEditorState::Requested {
            self.chat_widget
                .set_external_editor_state(ExternalEditorState::Active);
            self.app_event_tx.send(AppEvent::LaunchExternalEditor);
        }
        Ok(())
    }

    async fn handle_provider_config_modal_key(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> Result<AppRunControl> {
        let action = self
            .provider_config_modal
            .as_mut()
            .map(|state| state.modal.handle_key_event(key_event))
            .unwrap_or(ProviderConfigModalAction::None);
        match action {
            ProviderConfigModalAction::None => {
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            ProviderConfigModalAction::Exit => {
                let frame_requester = tui.frame_requester();
                if let Some(exit_mode) =
                    self.close_provider_config_modal_for_exit_with_redraw(&frame_requester)
                {
                    self.handle_event(tui, AppEvent::Exit(exit_mode)).await
                } else {
                    self.maybe_open_pending_skills_modal_with_redraw(tui);
                    Ok(AppRunControl::Continue)
                }
            }
        }
    }

    fn close_provider_config_modal_for_exit_with_redraw(
        &mut self,
        frame_requester: &tui::FrameRequester,
    ) -> Option<ExitMode> {
        let modal_was_open = self.provider_config_modal.is_some();
        let exit_mode = self.close_provider_config_modal_for_exit();
        if modal_was_open && exit_mode.is_none() {
            frame_requester.schedule_frame();
        }
        exit_mode
    }

    fn close_provider_config_modal_for_exit(&mut self) -> Option<ExitMode> {
        let mode = self
            .provider_config_modal
            .as_ref()
            .map(|state| state.mode)
            .unwrap_or(ProviderConfigModalMode::InSession);
        self.provider_config_modal = None;
        match mode {
            ProviderConfigModalMode::Startup => Some(self.provider_config_exit_mode()),
            ProviderConfigModalMode::InSession => None,
        }
    }

    fn handle_review_modal_key(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        let action = self
            .review_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(ReviewModalAction::None);
        if action == ReviewModalAction::Exit {
            self.review_modal = None;
        }
        tui.frame_requester().schedule_frame();
    }

    async fn handle_project_trust_modal_key(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> Result<AppRunControl> {
        let action = self
            .project_trust_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(ProjectTrustModalAction::None);
        match action {
            ProjectTrustModalAction::None => {
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            ProjectTrustModalAction::Exit => {
                self.project_trust_modal = None;
                let exit_mode = self.project_trust_exit_mode();
                self.handle_event(tui, AppEvent::Exit(exit_mode)).await
            }
            ProjectTrustModalAction::Selected(trust_level) => {
                self.project_trust_modal = None;
                let control = self.apply_project_trust_selection(tui, trust_level).await;
                self.maybe_open_pending_skills_modal_with_redraw(tui);
                tui.frame_requester().schedule_frame();
                Ok(control)
            }
        }
    }

    fn handle_identity_modal_key_event(&mut self, key_event: KeyEvent) {
        let action = self
            .identity_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(IdentityModalAction::None);
        match action {
            IdentityModalAction::None => {}
            IdentityModalAction::Selected(mask) => {
                self.identity_modal = None;
                self.update_identity(mask);
            }
            IdentityModalAction::Exit => {
                self.identity_modal = None;
            }
        }
    }

    async fn handle_model_selection_modal_key_event(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> Result<AppRunControl> {
        let action = self
            .model_selection_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(ModelSelectionModalAction::None);
        match action {
            ModelSelectionModalAction::None => {
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            ModelSelectionModalAction::Exit => {
                self.model_selection_modal = None;
                self.maybe_open_pending_skills_modal_with_redraw(tui);
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            ModelSelectionModalAction::PersistModelSelection {
                model,
                provider_id,
                effort,
            } => {
                self.model_selection_modal = None;
                tui.frame_requester().schedule_frame();
                let control = self
                    .handle_event(
                        tui,
                        AppEvent::PersistModelSelection {
                            model,
                            provider_id,
                            effort,
                        },
                    )
                    .await?;
                self.maybe_open_pending_skills_modal_with_redraw(tui);
                Ok(control)
            }
        }
    }

    async fn handle_experimental_features_modal_key_event(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> Result<AppRunControl> {
        let action = self
            .experimental_features_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(ExperimentalFeaturesModalAction::None);
        match action {
            ExperimentalFeaturesModalAction::None => {
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            ExperimentalFeaturesModalAction::SaveAndClose { updates } => {
                self.experimental_features_modal = None;
                tui.frame_requester().schedule_frame();
                if updates.is_empty() {
                    self.maybe_open_pending_skills_modal_with_redraw(tui);
                    Ok(AppRunControl::Continue)
                } else {
                    let control = self
                        .handle_event(tui, AppEvent::UpdateFeatureFlags { updates })
                        .await?;
                    self.maybe_open_pending_skills_modal_with_redraw(tui);
                    Ok(control)
                }
            }
        }
    }

    fn handle_mcp_tools_modal_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        let action = self
            .mcp_tools_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(McpToolsModalAction::None);
        match action {
            McpToolsModalAction::None => {
                tui.frame_requester().schedule_frame();
            }
            McpToolsModalAction::Exit => {
                self.mcp_tools_modal = None;
                tui.frame_requester().schedule_frame();
            }
        }
    }

    async fn handle_skills_modal_key_event(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> Result<AppRunControl> {
        let action = self
            .skills_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(SkillsModalAction::None);
        match action {
            SkillsModalAction::None => {
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            SkillsModalAction::Exit => {
                self.skills_modal = None;
                self.chat_widget.handle_manage_skills_closed();
                self.chat_widget.request_skills_refresh(true);
                self.maybe_open_pending_skills_modal_with_redraw(tui);
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            SkillsModalAction::Toggle { path, enabled } => {
                match self.set_skill_enabled(path.clone(), enabled).await {
                    Ok(()) => {
                        if let Some(modal) = self.skills_modal.as_mut() {
                            modal.set_skill_enabled(&path, enabled);
                        }
                    }
                    Err(message) => {
                        if let Some(modal) = self.skills_modal.as_mut() {
                            modal.set_error_message(message);
                        } else {
                            self.chat_widget.add_error_message(message);
                        }
                    }
                }
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
        }
    }

    async fn handle_approval_mode_modal_key_event(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> Result<AppRunControl> {
        let action = self
            .approval_mode_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(ApprovalModeModalAction::None);
        match action {
            ApprovalModeModalAction::None => {
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            ApprovalModeModalAction::Exit => {
                self.approval_mode_modal = None;
                self.maybe_open_pending_skills_modal_with_redraw(tui);
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            ApprovalModeModalAction::Selected(action) => {
                self.approval_mode_modal = None;
                tui.frame_requester().schedule_frame();
                self.handle_approval_mode_action(action);
                self.maybe_open_pending_skills_modal_with_redraw(tui);
                Ok(AppRunControl::Continue)
            }
        }
    }

    async fn handle_agent_selection_modal_key_event(
        &mut self,
        tui: &mut tui::Tui,
        key_event: KeyEvent,
    ) -> Result<AppRunControl> {
        let action = self
            .agent_selection_modal
            .as_mut()
            .map(|modal| modal.handle_key_event(key_event))
            .unwrap_or(AgentSelectionModalAction::None);
        match action {
            AgentSelectionModalAction::None => {
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            AgentSelectionModalAction::Exit => {
                self.agent_selection_modal = None;
                self.maybe_open_pending_skills_modal_with_redraw(tui);
                tui.frame_requester().schedule_frame();
                Ok(AppRunControl::Continue)
            }
            AgentSelectionModalAction::SelectThread(thread_id) => {
                self.agent_selection_modal = None;
                tui.frame_requester().schedule_frame();
                let control = self
                    .handle_event(tui, AppEvent::SelectAgentThread(thread_id))
                    .await?;
                self.maybe_open_pending_skills_modal_with_redraw(tui);
                Ok(control)
            }
        }
    }

    fn project_trust_exit_mode(&self) -> ExitMode {
        if self.chat_widget.thread_id().is_some() {
            ExitMode::ShutdownFirst
        } else {
            ExitMode::Immediate
        }
    }

    fn provider_config_exit_mode(&self) -> ExitMode {
        if self.chat_widget.thread_id().is_some() {
            ExitMode::ShutdownFirst
        } else {
            ExitMode::Immediate
        }
    }

    async fn handle_mouse_event(&mut self, tui: &mut tui::Tui, mouse_event: MouseEvent) {
        if tui.mouse_capture_enabled() && mouse_event.modifiers.contains(KeyModifiers::SHIFT) {
            let restore_in = SHIFT_MOUSE_BYPASS_DURATION;
            if !self.shift_mouse_bypass_active {
                tui.disable_mouse_capture_temporarily();
                self.shift_mouse_bypass_active = true;
                self.chat_widget
                    .set_status_header("Native selection: release Shift to return".to_string());
            }
            self.shift_mouse_bypass_restore_at = Some(Instant::now() + restore_in);
            tui.frame_requester().schedule_frame_in(restore_in);
            tui.frame_requester().schedule_frame();
            return;
        }

        if self.shift_mouse_bypass_active {
            self.restore_shift_mouse_bypass(tui, Some("Mouse capture restored"));
        }

        self.chat_widget.handle_mouse_event(mouse_event);
        tui.frame_requester().schedule_frame();
    }

    fn restore_shift_mouse_bypass_if_due(&mut self, tui: &mut tui::Tui) {
        self.restore_shift_mouse_bypass_if_due_with(|| tui.restore_mouse_capture_after_bypass());
    }

    fn restore_shift_mouse_bypass_if_due_with(&mut self, restore_mouse_capture: impl FnOnce()) {
        if self
            .shift_mouse_bypass_restore_at
            .is_some_and(|restore_at| Instant::now() >= restore_at)
        {
            self.restore_shift_mouse_bypass_with(
                restore_mouse_capture,
                Some("Mouse capture restored"),
            );
        }
    }

    fn restore_shift_mouse_bypass(&mut self, tui: &mut tui::Tui, status_header: Option<&str>) {
        self.restore_shift_mouse_bypass_with(
            || tui.restore_mouse_capture_after_bypass(),
            status_header,
        );
    }

    fn restore_shift_mouse_bypass_with(
        &mut self,
        restore_mouse_capture: impl FnOnce(),
        status_header: Option<&str>,
    ) {
        if !self.shift_mouse_bypass_active {
            self.shift_mouse_bypass_restore_at = None;
            return;
        }

        restore_mouse_capture();
        self.shift_mouse_bypass_active = false;
        self.shift_mouse_bypass_restore_at = None;

        if let Some(status_header) = status_header {
            self.chat_widget
                .set_status_header(status_header.to_string());
        }
    }

    async fn handle_event(&mut self, tui: &mut tui::Tui, event: AppEvent) -> Result<AppRunControl> {
        match event {
            AppEvent::NewSession => {
                let model = self.chat_widget.current_model().to_string();
                let summary = session_summary(
                    self.chat_widget.token_usage(),
                    self.chat_widget.thread_id(),
                    self.chat_widget.thread_name(),
                );
                self.shutdown_current_thread().await;
                if let Err(err) = self.server.remove_and_close_all_threads().await {
                    tracing::warn!(error = %err, "failed to close all threads");
                }
                let init = crate::chatwidget::ChatWidgetInit {
                    config: self.config.clone(),
                    thread_manager: self.server.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: self.app_event_tx.clone(),
                    // New sessions start without prefilled message content.
                    initial_user_message: None,
                    enhanced_keys_supported: self.enhanced_keys_supported,
                    auth_manager: self.auth_manager.clone(),
                    feedback: self.feedback.clone(),
                    is_first_run: false,
                    startup: if self.config.provider_config_required {
                        crate::chatwidget::ChatWidgetStartup::NeedsProviderConfig {
                            auto_open: true,
                        }
                    } else {
                        crate::chatwidget::ChatWidgetStartup::Configured { model: Some(model) }
                    },
                    otel_manager: self.otel_manager.clone(),
                };
                self.chat_widget = ChatWidget::new(init);
                self.reset_thread_event_state();
                if let Some(summary) = summary {
                    let mut lines: Vec<Line<'static>> = vec![summary.usage_line.clone().into()];
                    if let Some(command) = summary.resume_command {
                        let spans = vec!["To continue this session, run ".into(), command.cyan()];
                        lines.push(spans.into());
                    }
                    self.chat_widget.add_plain_history_lines(lines);
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenResumePicker => {
                match crate::resume_picker::run_resume_picker(
                    tui,
                    &self.config.adam_home,
                    &self.config.model_provider_id,
                    Some(self.config.cwd.as_path()),
                    false,
                )
                .await?
                {
                    SessionSelection::Resume(path) => {
                        let current_cwd = self.config.cwd.clone();
                        let resume_cwd = match crate::resolve_cwd_for_resume_or_fork(
                            tui,
                            &current_cwd,
                            &path,
                            CwdPromptAction::Resume,
                            true,
                        )
                        .await?
                        {
                            Some(cwd) => cwd,
                            None => current_cwd.clone(),
                        };
                        let mut resume_config = if crate::cwds_differ(&current_cwd, &resume_cwd) {
                            match self.rebuild_config_for_cwd(resume_cwd).await {
                                Ok(cfg) => cfg,
                                Err(err) => {
                                    self.chat_widget.add_error_message(format!(
                                        "Failed to rebuild configuration for resume: {err}"
                                    ));
                                    return Ok(AppRunControl::Continue);
                                }
                            }
                        } else {
                            // No rebuild needed: current_cwd comes from self.config.cwd.
                            self.config.clone()
                        };
                        self.apply_runtime_policy_overrides(&mut resume_config);
                        let summary = session_summary(
                            self.chat_widget.token_usage(),
                            self.chat_widget.thread_id(),
                            self.chat_widget.thread_name(),
                        );
                        match self
                            .server
                            .resume_thread_from_rollout(
                                resume_config.clone(),
                                path.clone(),
                                self.auth_manager.clone(),
                            )
                            .await
                        {
                            Ok(resumed) => {
                                self.shutdown_current_thread().await;
                                self.config = resume_config;
                                tui.set_notification_method(self.config.tui_notification_method);
                                self.file_search.update_search_dir(self.config.cwd.clone());
                                if let Err(err) = self
                                    .ensure_non_git_changelog_baseline(self.config.cwd.clone())
                                    .await
                                {
                                    tracing::warn!(
                                        cwd = %self.config.cwd.display(),
                                        %err,
                                        "failed to prewarm changelog baseline after cwd switch"
                                    );
                                }
                                let init = self.chatwidget_init_for_forked_or_resumed_thread(
                                    tui,
                                    self.config.clone(),
                                );
                                self.chat_widget = ChatWidget::new_from_existing(
                                    init,
                                    resumed.thread,
                                    resumed.session_configured,
                                );
                                self.reset_thread_event_state();
                                if let Some(summary) = summary {
                                    let mut lines: Vec<Line<'static>> =
                                        vec![summary.usage_line.clone().into()];
                                    if let Some(command) = summary.resume_command {
                                        let spans = vec![
                                            "To continue this session, run ".into(),
                                            command.cyan(),
                                        ];
                                        lines.push(spans.into());
                                    }
                                    self.chat_widget.add_plain_history_lines(lines);
                                }
                            }
                            Err(err) => {
                                let path_display = path.display();
                                self.chat_widget.add_error_message(format!(
                                    "Failed to resume session from {path_display}: {err}"
                                ));
                            }
                        }
                    }
                    SessionSelection::Exit
                    | SessionSelection::StartFresh
                    | SessionSelection::Fork(_) => {}
                }

                // Re-entering the fullscreen TUI after a restored terminal may blank the viewport;
                // force a redraw either way.
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ForkCurrentSession => {
                let summary = session_summary(
                    self.chat_widget.token_usage(),
                    self.chat_widget.thread_id(),
                    self.chat_widget.thread_name(),
                );
                if let Some(path) = self.chat_widget.rollout_path() {
                    match self
                        .server
                        .fork_thread(usize::MAX, self.config.clone(), path.clone())
                        .await
                    {
                        Ok(forked) => {
                            self.shutdown_current_thread().await;
                            let init = self.chatwidget_init_for_forked_or_resumed_thread(
                                tui,
                                self.config.clone(),
                            );
                            self.chat_widget = ChatWidget::new_from_existing(
                                init,
                                forked.thread,
                                forked.session_configured,
                            );
                            self.reset_thread_event_state();
                            if let Some(summary) = summary {
                                let mut lines: Vec<Line<'static>> =
                                    vec![summary.usage_line.clone().into()];
                                if let Some(command) = summary.resume_command {
                                    let spans = vec![
                                        "To continue this session, run ".into(),
                                        command.cyan(),
                                    ];
                                    lines.push(spans.into());
                                }
                                self.chat_widget.add_plain_history_lines(lines);
                            }
                        }
                        Err(err) => {
                            let path_display = path.display();
                            self.chat_widget.add_error_message(format!(
                                "Failed to fork current session from {path_display}: {err}"
                            ));
                        }
                    }
                } else {
                    self.chat_widget
                        .add_error_message("Current session is not ready to fork yet.".to_string());
                }

                tui.frame_requester().schedule_frame();
            }
            AppEvent::InsertHistoryCell(cell) => self.insert_history_cell(cell, tui),
            AppEvent::InsertThreadHistoryCell { thread_id, cell } => {
                self.insert_history_cell_for_thread(thread_id, cell, tui)
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(50));
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.chat_widget.on_commit_tick();
            }
            AppEvent::CodexEvent(event) => {
                self.enqueue_primary_event(event).await?;
            }
            AppEvent::ThreadEventReceived { thread_id, event } => {
                self.enqueue_thread_event(thread_id, event).await?;
            }
            AppEvent::Exit(mode) => match mode {
                ExitMode::ShutdownFirst => self.chat_widget.submit_op(Op::Shutdown),
                ExitMode::Immediate => {
                    return Ok(AppRunControl::Exit(ExitReason::UserRequested));
                }
            },
            AppEvent::FatalExitRequest(message) => {
                return Ok(AppRunControl::Exit(ExitReason::Fatal(message)));
            }
            AppEvent::CodexOp(op) => {
                self.chat_widget.submit_op(op);
            }
            AppEvent::StartReview { review_request } => {
                self.review_modal = None;
                self.start_review(review_request).await?;
            }
            AppEvent::RequestChangelog => {
                self.request_changelog().await;
            }
            AppEvent::DiffResult(text) => {
                // Clear the in-progress state in the bottom pane
                self.chat_widget.on_diff_complete();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ChangelogResult(result) => match result {
                Ok(ChangelogOutput::Entries {
                    display_root,
                    entries,
                }) => {
                    if entries.is_empty() {
                        self.insert_history_cell(
                            Box::new(history_cell::new_info_event(
                                "No changes detected.".to_string(),
                                None,
                            )),
                            tui,
                        );
                    } else {
                        self.insert_history_cell(
                            Box::new(history_cell::new_changelog_output(
                                entries,
                                &self.config.cwd,
                                &display_root,
                            )),
                            tui,
                        );
                    }
                }
                Err(err) => self.insert_history_cell(
                    Box::new(history_cell::new_error_event(format!(
                        "Failed to collect changelog: {err}"
                    ))),
                    tui,
                ),
            },
            AppEvent::OpenAppLink {
                title,
                description,
                instructions,
                url,
                is_installed,
            } => {
                self.chat_widget.open_app_link_view(
                    title,
                    description,
                    instructions,
                    url,
                    is_installed,
                );
            }
            AppEvent::StartFileSearch(query) => {
                self.file_search.on_user_query(query);
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::ConnectorsLoaded(result) => {
                self.chat_widget.on_connectors_loaded(result);
            }
            AppEvent::OpenIdentityModal => self.open_identity_modal(),
            AppEvent::OpenProviderConfigModal => {
                self.open_provider_config_modal(ProviderConfigModalMode::InSession);
            }
            AppEvent::OpenReviewModal => {
                self.open_review_modal();
            }
            AppEvent::OpenModelSelectionModal { presets } => {
                self.open_model_selection_modal(presets);
            }
            AppEvent::OpenExperimentalFeaturesModal => {
                self.open_experimental_features_modal();
            }
            AppEvent::OpenMcpToolsModal => {
                self.open_mcp_tools_modal();
            }
            AppEvent::UpdatePersonality(personality) => {
                self.on_update_personality(personality);
            }
            AppEvent::CustomProviderConfigured(config) => {
                return Ok(self
                    .handle_custom_provider_configured(Some(tui), config)
                    .await);
            }
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_widget.open_reasoning_popup(model);
            }
            AppEvent::OpenAllModelsPopup { models } => {
                self.chat_widget.open_all_models_popup(models);
            }
            AppEvent::OpenFullAccessConfirmation {
                preset,
                return_to_permissions,
            } => {
                self.open_full_access_confirmation_modal(preset, return_to_permissions);
            }
            AppEvent::OpenWorldWritableWarningConfirmation {
                preset,
                sample_paths,
                extra_count,
                failed_scan,
            } => {
                self.open_world_writable_warning_confirmation_modal(
                    preset,
                    sample_paths,
                    extra_count,
                    failed_scan,
                );
            }
            AppEvent::OpenFeedbackNote {
                category,
                include_logs,
            } => {
                self.chat_widget.open_feedback_note(category, include_logs);
            }
            AppEvent::OpenFeedbackConsent { category } => {
                self.chat_widget.open_feedback_consent(category);
            }
            AppEvent::LaunchExternalEditor => {
                if self.chat_widget.external_editor_state() == ExternalEditorState::Active {
                    self.launch_external_editor(tui).await;
                }
            }
            AppEvent::OpenWindowsSandboxEnablePrompt { preset } => {
                #[cfg(target_os = "windows")]
                self.open_windows_sandbox_enable_prompt_modal(preset);
                #[cfg(not(target_os = "windows"))]
                let _ = preset;
            }
            AppEvent::OpenWindowsSandboxFallbackPrompt { preset, reason } => {
                self.otel_manager
                    .counter("adam.windows_sandbox.fallback_prompt_shown", 1, &[]);
                self.chat_widget.clear_windows_sandbox_setup_status();
                if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                    self.otel_manager.record_duration(
                        "adam.windows_sandbox.elevated_setup_duration_ms",
                        started_at.elapsed(),
                        &[("result", "failure")],
                    );
                }
                #[cfg(target_os = "windows")]
                self.open_windows_sandbox_fallback_prompt_modal(preset, reason);
                #[cfg(not(target_os = "windows"))]
                let _ = (preset, reason);
            }
            AppEvent::BeginWindowsSandboxElevatedSetup { preset } => {
                #[cfg(target_os = "windows")]
                {
                    let policy = preset.sandbox.clone();
                    let policy_cwd = self.config.cwd.clone();
                    let command_cwd = policy_cwd.clone();
                    let env_map: std::collections::HashMap<String, String> =
                        std::env::vars().collect();
                    let adam_home = self.config.adam_home.clone();
                    let tx = self.app_event_tx.clone();

                    // If the elevated setup already ran on this machine, don't prompt for
                    // elevation again - just flip the config to use the elevated path.
                    if adam_agent::windows_sandbox::sandbox_setup_is_complete(adam_home.as_path()) {
                        tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                            preset,
                            mode: WindowsSandboxEnableMode::Elevated,
                        });
                        return Ok(AppRunControl::Continue);
                    }

                    self.chat_widget.show_windows_sandbox_setup_status();
                    self.windows_sandbox.setup_started_at = Some(Instant::now());
                    let otel_manager = self.otel_manager.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = adam_agent::windows_sandbox::run_elevated_setup(
                            &policy,
                            policy_cwd.as_path(),
                            command_cwd.as_path(),
                            &env_map,
                            adam_home.as_path(),
                        );
                        let event = match result {
                            Ok(()) => {
                                otel_manager.counter(
                                    "adam.windows_sandbox.elevated_setup_success",
                                    1,
                                    &[],
                                );
                                AppEvent::EnableWindowsSandboxForAgentMode {
                                    preset: preset.clone(),
                                    mode: WindowsSandboxEnableMode::Elevated,
                                }
                            }
                            Err(err) => {
                                let mut code_tag: Option<String> = None;
                                let mut message_tag: Option<String> = None;
                                if let Some((code, message)) =
                                    adam_agent::windows_sandbox::elevated_setup_failure_details(
                                        &err,
                                    )
                                {
                                    code_tag = Some(code);
                                    message_tag = Some(message);
                                }
                                let mut tags: Vec<(&str, &str)> = Vec::new();
                                if let Some(code) = code_tag.as_deref() {
                                    tags.push(("code", code));
                                }
                                if let Some(message) = message_tag.as_deref() {
                                    tags.push(("message", message));
                                }
                                otel_manager.counter(
                                    "adam.windows_sandbox.elevated_setup_failure",
                                    1,
                                    &tags,
                                );
                                tracing::error!(
                                    error = %err,
                                    "failed to run elevated Windows sandbox setup"
                                );
                                AppEvent::OpenWindowsSandboxFallbackPrompt {
                                    preset,
                                    reason: WindowsSandboxFallbackReason::ElevationFailed,
                                }
                            }
                        };
                        tx.send(event);
                    });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = preset;
                }
            }
            AppEvent::EnableWindowsSandboxForAgentMode { preset, mode } => {
                #[cfg(target_os = "windows")]
                {
                    self.chat_widget.clear_windows_sandbox_setup_status();
                    if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                        self.otel_manager.record_duration(
                            "adam.windows_sandbox.elevated_setup_duration_ms",
                            started_at.elapsed(),
                            &[("result", "success")],
                        );
                    }
                    let profile = self.active_profile.as_deref();
                    let feature_key = Feature::WindowsSandbox.key();
                    let elevated_key = Feature::WindowsSandboxElevated.key();
                    let elevated_enabled = matches!(mode, WindowsSandboxEnableMode::Elevated);
                    let mut builder =
                        ConfigEditsBuilder::new(&self.config.adam_home).with_profile(profile);
                    if elevated_enabled {
                        builder = builder.set_feature_enabled(elevated_key, true);
                    } else {
                        builder = builder
                            .set_feature_enabled(feature_key, true)
                            .set_feature_enabled(elevated_key, false);
                    }
                    match builder.apply().await {
                        Ok(()) => {
                            if elevated_enabled {
                                self.config.set_windows_elevated_sandbox_enabled(true);
                                self.chat_widget
                                    .set_feature_enabled(Feature::WindowsSandboxElevated, true);
                            } else {
                                self.config.set_windows_sandbox_enabled(true);
                                self.config.set_windows_elevated_sandbox_enabled(false);
                                self.chat_widget
                                    .set_feature_enabled(Feature::WindowsSandbox, true);
                                self.chat_widget
                                    .set_feature_enabled(Feature::WindowsSandboxElevated, false);
                            }
                            self.chat_widget.clear_forced_auto_mode_downgrade();
                            let windows_sandbox_level =
                                WindowsSandboxLevel::from_config(&self.config);
                            if let Some((sample_paths, extra_count, failed_scan)) =
                                self.chat_widget.world_writable_warning_details()
                            {
                                self.app_event_tx.send(AppEvent::CodexOp(
                                    Op::OverrideTurnContext {
                                        cwd: None,
                                        approval_policy: None,
                                        sandbox_policy: None,
                                        windows_sandbox_level: Some(windows_sandbox_level),
                                        model: None,
                                        effort: None,
                                        summary: None,
                                        identity: None,
                                        personality: None,
                                    },
                                ));
                                self.app_event_tx.send(
                                    AppEvent::OpenWorldWritableWarningConfirmation {
                                        preset: Some(preset.clone()),
                                        sample_paths,
                                        extra_count,
                                        failed_scan,
                                    },
                                );
                            } else {
                                self.app_event_tx.send(AppEvent::CodexOp(
                                    Op::OverrideTurnContext {
                                        cwd: None,
                                        approval_policy: Some(preset.approval),
                                        sandbox_policy: Some(preset.sandbox.clone()),
                                        windows_sandbox_level: Some(windows_sandbox_level),
                                        model: None,
                                        effort: None,
                                        summary: None,
                                        identity: None,
                                        personality: None,
                                    },
                                ));
                                self.app_event_tx
                                    .send(AppEvent::UpdateAskForApprovalPolicy(preset.approval));
                                self.app_event_tx
                                    .send(AppEvent::UpdateSandboxPolicy(preset.sandbox.clone()));
                                self.chat_widget.add_info_message(
                                    match mode {
                                        WindowsSandboxEnableMode::Elevated => {
                                            "Enabled elevated agent sandbox.".to_string()
                                        }
                                        WindowsSandboxEnableMode::Legacy => {
                                            "Enabled non-elevated agent sandbox.".to_string()
                                        }
                                    },
                                    None,
                                );
                            }
                        }
                        Err(err) => {
                            tracing::error!(
                                error = %err,
                                "failed to enable Windows sandbox feature"
                            );
                            self.chat_widget.add_error_message(format!(
                                "Failed to enable the Windows sandbox feature: {err}"
                            ));
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = (preset, mode);
                }
            }
            AppEvent::PersistModelSelection {
                model,
                provider_id,
                effort,
            } => {
                let previous_provider_id = self.config.model_provider_id.clone();
                match self
                    .persist_model_selection(model.clone(), provider_id, effort)
                    .await
                {
                    Ok(provider_id) => {
                        let mut message = format!("Model changed to {model}");
                        if let Some(label) = Self::reasoning_label_for(&model, effort) {
                            message.push(' ');
                            message.push_str(label);
                        }
                        if provider_id != previous_provider_id {
                            message.push_str(" using provider `");
                            message.push_str(&provider_id);
                            message.push('`');
                        }
                        self.chat_widget.add_info_message(message, None);
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "failed to persist model selection");
                        self.chat_widget.add_error_message(err);
                    }
                }
            }
            AppEvent::PersistPersonalitySelection { personality } => {
                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.adam_home)
                    .with_profile(profile)
                    .set_personality(Some(personality))
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let label = Self::personality_label(personality);
                        let mut message = format!("Personality set to {label}");
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_widget.add_info_message(message, None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist personality selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save personality for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save default personality: {err}"
                            ));
                        }
                    }
                }
            }
            AppEvent::PersistBuddyConfig { edit } => {
                self.persist_buddy_config(edit).await;
            }
            AppEvent::UpdateAskForApprovalPolicy(policy) => {
                self.runtime_approval_policy_override = Some(policy);
                if let Err(err) = self.config.approval_policy.set(policy) {
                    tracing::warn!(%err, "failed to set approval policy on app config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set approval policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.chat_widget.set_approval_policy(policy);
            }
            AppEvent::UpdateSandboxPolicy(policy) => {
                #[cfg(target_os = "windows")]
                let policy_is_workspace_write_or_ro = matches!(
                    &policy,
                    adam_agent::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | adam_agent::protocol::SandboxPolicy::ReadOnly
                );

                if let Err(err) = self.config.sandbox_policy.set(policy.clone()) {
                    tracing::warn!(%err, "failed to set sandbox policy on app config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                #[cfg(target_os = "windows")]
                if !matches!(&policy, adam_agent::protocol::SandboxPolicy::ReadOnly)
                    || WindowsSandboxLevel::from_config(&self.config)
                        != WindowsSandboxLevel::Disabled
                {
                    self.config.forced_auto_mode_downgraded_on_windows = false;
                }
                if let Err(err) = self.chat_widget.set_sandbox_policy(policy) {
                    tracing::warn!(%err, "failed to set sandbox policy on chat config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.runtime_sandbox_policy_override =
                    Some(self.config.sandbox_policy.get().clone());

                // If sandbox policy becomes workspace-write or read-only, run the Windows world-writable scan.
                #[cfg(target_os = "windows")]
                {
                    // One-shot suppression if the user just confirmed continue.
                    if self.windows_sandbox.skip_world_writable_scan_once {
                        self.windows_sandbox.skip_world_writable_scan_once = false;
                        return Ok(AppRunControl::Continue);
                    }

                    let should_check = WindowsSandboxLevel::from_config(&self.config)
                        != WindowsSandboxLevel::Disabled
                        && policy_is_workspace_write_or_ro
                        && !self.chat_widget.world_writable_warning_hidden();
                    if should_check {
                        let cwd = self.config.cwd.clone();
                        let env_map: std::collections::HashMap<String, String> =
                            std::env::vars().collect();
                        let tx = self.app_event_tx.clone();
                        let logs_base_dir = self.config.adam_home.clone();
                        let sandbox_policy = self.config.sandbox_policy.get().clone();
                        Self::spawn_world_writable_scan(
                            cwd,
                            env_map,
                            logs_base_dir,
                            sandbox_policy,
                            tx,
                        );
                    }
                }
            }
            AppEvent::UpdateFeatureFlags { updates } => {
                if updates.is_empty() {
                    return Ok(AppRunControl::Continue);
                }
                let windows_sandbox_changed = updates.iter().any(|(feature, _)| {
                    matches!(
                        feature,
                        Feature::WindowsSandbox | Feature::WindowsSandboxElevated
                    )
                });
                let mut builder = ConfigEditsBuilder::new(&self.config.adam_home)
                    .with_profile(self.active_profile.as_deref());
                for (feature, enabled) in &updates {
                    let feature_key = feature.key();
                    if *enabled {
                        // Update the in-memory configs.
                        self.config.features.enable(*feature);
                        self.chat_widget.set_feature_enabled(*feature, true);
                        builder = builder.set_feature_enabled(feature_key, true);
                    } else {
                        // Update the in-memory configs.
                        self.config.features.disable(*feature);
                        self.chat_widget.set_feature_enabled(*feature, false);
                        if feature.default_enabled() {
                            builder = builder.set_feature_enabled(feature_key, false);
                        } else {
                            // If the feature already default to `false`, we drop the key
                            // in the config file so that the user does not miss the feature
                            // once it gets globally released.
                            builder = builder.with_edits(vec![ConfigEdit::ClearPath {
                                segments: vec!["features".to_string(), feature_key.to_string()],
                            }]);
                        }
                    }
                }
                if windows_sandbox_changed {
                    #[cfg(target_os = "windows")]
                    {
                        let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
                        self.app_event_tx
                            .send(AppEvent::CodexOp(Op::OverrideTurnContext {
                                cwd: None,
                                approval_policy: None,
                                sandbox_policy: None,
                                windows_sandbox_level: Some(windows_sandbox_level),
                                model: None,
                                effort: None,
                                summary: None,
                                identity: None,
                                personality: None,
                            }));
                    }
                }
                if let Err(err) = builder.apply().await {
                    tracing::error!(error = %err, "failed to persist feature flags");
                    self.chat_widget.add_error_message(format!(
                        "Failed to update experimental features: {err}"
                    ));
                }
            }
            AppEvent::SkipNextWorldWritableScan => {
                self.windows_sandbox.skip_world_writable_scan_once = true;
            }
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                self.chat_widget.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::UpdateWorldWritableWarningAcknowledged(ack) => {
                self.chat_widget
                    .set_world_writable_warning_acknowledged(ack);
            }
            AppEvent::PersistFullAccessWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.adam_home)
                    .set_hide_full_access_warning(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist full access warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save full access confirmation preference: {err}"
                    ));
                }
            }
            AppEvent::PersistWorldWritableWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.adam_home)
                    .set_hide_world_writable_warning(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist world-writable warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save Agent mode warning preference: {err}"
                    ));
                }
            }
            AppEvent::PersistModelMigrationPromptAcknowledged {
                from_model,
                to_model,
            } => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.adam_home)
                    .record_model_migration_seen(from_model.as_str(), to_model.as_str())
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist model migration prompt acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save model migration prompt preference: {err}"
                    ));
                }
            }
            AppEvent::OpenApprovalsPopup => {
                self.open_approvals_modal();
            }
            AppEvent::OpenAgentPicker => {
                self.open_agent_picker().await;
            }
            AppEvent::SelectAgentThread(thread_id) => {
                self.select_agent_thread(tui, thread_id).await?;
            }
            AppEvent::OpenSkillsModal => {
                self.open_skills_modal();
            }
            AppEvent::OpenPermissionsPopup => {
                self.open_permissions_modal();
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.ensure_review_modal();
                if let Some(mut modal) = self.review_modal.take() {
                    modal.show_branch_picker(&cwd).await;
                    self.review_modal = Some(modal);
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.ensure_review_modal();
                if let Some(mut modal) = self.review_modal.take() {
                    modal.show_commit_picker(&cwd).await;
                    self.review_modal = Some(modal);
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.ensure_review_modal();
                if let Some(modal) = self.review_modal.as_mut() {
                    modal.show_custom_prompt();
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::SubmitUserMessageWithMode { text, identity } => {
                self.chat_widget
                    .submit_user_message_with_mode(text, identity);
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch { cwd, changes, .. } => {
                    let diff_summary = DiffSummary::new(changes, cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                    ));
                }
                ApprovalRequest::Exec { command, .. } => {
                    let full_cmd = strip_bash_lc_and_escape(&command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                    ));
                }
                ApprovalRequest::McpElicitation {
                    server_name,
                    message,
                    ..
                } => {
                    let paragraph = Paragraph::new(vec![
                        Line::from(vec!["Server: ".into(), server_name.bold()]),
                        Line::from(""),
                        Line::from(message),
                    ])
                    .wrap(Wrap { trim: false });
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(paragraph)],
                        "E L I C I T A T I O N".to_string(),
                    ));
                }
            },
        }
        Ok(AppRunControl::Continue)
    }

    fn handle_codex_event_now(&mut self, event: Event) {
        if self.suppress_shutdown_complete && matches!(event.msg, EventMsg::ShutdownComplete) {
            self.suppress_shutdown_complete = false;
            return;
        }
        if let EventMsg::McpListToolsResponse(response) = &event.msg
            && self.pending_mcp_tools_modal_responses > 0
        {
            self.pending_mcp_tools_modal_responses =
                self.pending_mcp_tools_modal_responses.saturating_sub(1);
            if let Some(modal) = self.mcp_tools_modal.as_mut() {
                modal.set_snapshot(&self.config, response.clone());
                self.chat_widget.request_redraw_for_ui();
            }
            return;
        }
        let is_list_skills_response = matches!(&event.msg, EventMsg::ListSkillsResponse(_));
        if let EventMsg::ListSkillsResponse(response) = &event.msg {
            let cwd = self.chat_widget.config_ref().cwd.clone();
            let errors = errors_for_cwd(&cwd, response);
            emit_skill_load_warnings(&self.app_event_tx, &errors);
        }
        self.sync_agent_picker_thread_from_event(&event.msg);
        self.handle_backtrack_event(&event.msg);
        self.chat_widget.handle_codex_event(event);
        if is_list_skills_response {
            self.maybe_open_pending_skills_modal();
        }
    }

    fn handle_codex_event_replay(&mut self, event: Event) {
        self.sync_agent_picker_thread_from_event(&event.msg);
        self.handle_backtrack_event(&event.msg);
        self.chat_widget.handle_codex_event_replay(event);
    }

    fn handle_active_thread_event(&mut self, tui: &mut tui::Tui, event: Event) -> Result<()> {
        self.handle_codex_event_now(event);
        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }

    async fn handle_thread_created(&mut self, thread_id: ThreadId) -> Result<()> {
        if self.thread_event_channels.contains_key(&thread_id) {
            return Ok(());
        }
        let thread = match self.server.get_thread(thread_id).await {
            Ok(thread) => thread,
            Err(err) => {
                tracing::warn!("failed to attach listener for thread {thread_id}: {err}");
                return Ok(());
            }
        };
        let config_snapshot = thread.config_snapshot().await;
        let status = if let Some(entry) = self.agent_picker_threads.get(&thread_id) {
            entry.status.clone()
        } else {
            thread.agent_status().await
        };
        self.upsert_agent_picker_thread(
            thread_id,
            config_snapshot.session_source.get_nickname(),
            config_snapshot.session_source.get_agent_role(),
            status,
            false,
        );
        let event = Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: thread_id,
                forked_from_id: None,
                thread_name: None,
                model: config_snapshot.model,
                identity_kind: config_snapshot.identity_kind,
                model_provider_id: config_snapshot.model_provider_id,
                approval_policy: config_snapshot.approval_policy,
                sandbox_policy: config_snapshot.sandbox_policy,
                cwd: config_snapshot.cwd,
                reasoning_effort: config_snapshot.reasoning_effort,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: thread.rollout_path(),
            }),
        };
        let channel =
            ThreadEventChannel::new_with_session_configured(THREAD_EVENT_CHANNEL_CAPACITY, event);
        self.thread_event_channels.insert(thread_id, channel);
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            loop {
                let event = match thread.next_event().await {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::debug!("external thread {thread_id} listener stopped: {err}");
                        break;
                    }
                };
                app_event_tx.send(AppEvent::ThreadEventReceived { thread_id, event });
            }
        });
        Ok(())
    }

    fn reasoning_label(reasoning_effort: Option<ReasoningEffortConfig>) -> &'static str {
        match reasoning_effort {
            Some(ReasoningEffortConfig::Minimal) => "minimal",
            Some(ReasoningEffortConfig::Low) => "low",
            Some(ReasoningEffortConfig::Medium) => "medium",
            Some(ReasoningEffortConfig::High) => "high",
            Some(ReasoningEffortConfig::XHigh) => "xhigh",
            None | Some(ReasoningEffortConfig::None) => "default",
        }
    }

    fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<ReasoningEffortConfig>,
    ) -> Option<&'static str> {
        (!model.starts_with("codex-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> adam_agent::protocol::TokenUsage {
        self.chat_widget.token_usage()
    }

    fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        // TODO(aibrahim): Remove this and don't use config as a state object.
        // Instead, explicitly pass the stored identity's effort into new sessions.
        self.config.model_reasoning_effort = effort;
        self.chat_widget.set_reasoning_effort(effort);
    }

    fn on_update_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
        self.chat_widget.set_personality(personality);
    }

    fn update_identity(&mut self, mask: IdentityMask) {
        let selected_kind = mask.kind;
        let selected_name = mask.name.clone();
        self.chat_widget.set_identity_mask(mask);
        if let Some(kind) = selected_kind {
            match AdamStateStore::new(&self.config.adam_home).set_last_selected_identity(kind) {
                Ok(()) => {
                    self.config.last_selected_identity = Some(kind);
                }
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to save default identity: {err}. Switched the current session to identity `{selected_name}`."
                    ));
                }
            }
        }
    }

    fn open_identity_modal(&mut self) {
        if !self.config.features.enabled(Feature::Identities) {
            return;
        }
        let presets = identities::presets_for_tui(self.server.as_ref());
        let Some(modal) = IdentityModal::new(
            presets,
            Some(self.chat_widget.active_identity_kind_for_ui()),
        ) else {
            self.chat_widget
                .add_info_message("No identities are available right now.".to_string(), None);
            return;
        };
        self.identity_modal = Some(modal);
        self.chat_widget.request_redraw_for_ui();
    }

    fn open_model_selection_modal(&mut self, presets: Vec<ModelPreset>) {
        self.chat_widget.dismiss_active_view();
        let context: ModelSelectionModalContext = self.chat_widget.model_selection_context();
        let Some(modal) = ModelSelectionModal::new(presets, context) else {
            self.chat_widget
                .add_info_message("No models are available right now.".to_string(), None);
            return;
        };
        self.model_selection_modal = Some(modal);
        self.chat_widget.request_redraw_for_ui();
    }

    fn open_experimental_features_modal(&mut self) {
        self.chat_widget.dismiss_active_view();
        let features = FEATURES
            .iter()
            .filter_map(|spec| {
                let name = spec.stage.experimental_menu_name()?;
                let description = spec.stage.experimental_menu_description()?;
                Some(ExperimentalFeatureItem {
                    feature: spec.id,
                    name: name.to_string(),
                    description: description.to_string(),
                    enabled: self.config.features.enabled(spec.id),
                })
            })
            .collect();
        self.experimental_features_modal = Some(ExperimentalFeaturesModal::new(features));
        self.chat_widget.request_redraw_for_ui();
    }

    fn open_mcp_tools_modal(&mut self) {
        self.chat_widget.dismiss_active_view();
        if self.config.mcp_servers.is_empty() {
            self.mcp_tools_modal = Some(McpToolsModal::new_empty(&self.config));
        } else {
            self.mcp_tools_modal = Some(McpToolsModal::new_loading(&self.config));
            self.pending_mcp_tools_modal_responses =
                self.pending_mcp_tools_modal_responses.saturating_add(1);
            self.chat_widget.submit_op(Op::ListMcpTools);
        }
        self.chat_widget.request_redraw_for_ui();
    }

    fn open_skills_modal(&mut self) {
        self.chat_widget.dismiss_active_view();
        match self.chat_widget.skills_modal_items() {
            SkillsModalItems::Loading => {
                let was_pending = self.pending_skills_modal_open;
                self.pending_skills_modal_open = true;
                self.chat_widget.request_skills_refresh_if_idle(true);
                if !was_pending {
                    self.chat_widget.add_info_message(
                        "Skills are still loading.".to_string(),
                        Some("The skills manager will open when loading finishes.".to_string()),
                    );
                }
            }
            SkillsModalItems::Empty => {
                self.pending_skills_modal_open = false;
                self.chat_widget
                    .add_info_message("No skills available.".to_string(), None);
            }
            SkillsModalItems::Ready(items) => {
                self.pending_skills_modal_open = false;
                let Some(modal) = SkillsModal::new(items) else {
                    self.chat_widget
                        .add_info_message("No skills available.".to_string(), None);
                    return;
                };
                self.skills_modal = Some(modal);
                self.chat_widget.request_redraw_for_ui();
            }
        }
    }

    fn can_auto_open_skills_modal(&self) -> bool {
        self.overlay.is_none()
            && self.provider_config_modal.is_none()
            && self.project_trust_modal.is_none()
            && self.identity_modal.is_none()
            && self.model_selection_modal.is_none()
            && self.experimental_features_modal.is_none()
            && self.skills_modal.is_none()
            && self.approval_mode_modal.is_none()
            && self.agent_selection_modal.is_none()
            && self.review_modal.is_none()
            && self.chat_widget.no_modal_or_popup_active()
    }

    fn maybe_open_pending_skills_modal(&mut self) -> bool {
        if !self.pending_skills_modal_open
            || self.chat_widget.skills_request_in_flight()
            || !self.can_auto_open_skills_modal()
        {
            return false;
        }

        self.open_skills_modal();
        true
    }

    fn maybe_open_pending_skills_modal_with_redraw(&mut self, tui: &mut tui::Tui) {
        if self.maybe_open_pending_skills_modal() {
            tui.frame_requester().schedule_frame();
        }
    }

    async fn set_skill_enabled(
        &mut self,
        path: PathBuf,
        enabled: bool,
    ) -> std::result::Result<(), String> {
        let edits = [ConfigEdit::SetSkillConfig {
            path: path.clone(),
            enabled,
        }];
        match ConfigEditsBuilder::new(&self.config.adam_home)
            .with_edits(edits)
            .apply()
            .await
        {
            Ok(()) => {
                self.chat_widget.update_skill_enabled(path, enabled);
                Ok(())
            }
            Err(err) => {
                let path_display = path.display();
                Err(format!(
                    "Failed to update skill config for {path_display}: {err}"
                ))
            }
        }
    }

    fn open_approvals_modal(&mut self) {
        self.open_approval_mode_modal(true);
    }

    fn open_permissions_modal(&mut self) {
        let include_read_only = cfg!(target_os = "windows");
        self.open_approval_mode_modal(include_read_only);
    }

    fn open_approval_mode_modal(&mut self, include_read_only: bool) {
        self.chat_widget.dismiss_active_view();
        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let presets = builtin_approval_presets();
        let mut items = Vec::new();

        #[cfg(target_os = "windows")]
        let windows_degraded_sandbox_enabled = {
            let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
            matches!(windows_sandbox_level, WindowsSandboxLevel::RestrictedToken)
        };
        #[cfg(not(target_os = "windows"))]
        let windows_degraded_sandbox_enabled = false;

        let show_elevate_sandbox_hint = adam_agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
            && windows_degraded_sandbox_enabled
            && presets.iter().any(|preset| preset.id == "auto");

        for preset in presets {
            if !include_read_only && preset.id == "read-only" {
                continue;
            }
            let is_current =
                ChatWidget::preset_matches_current(current_approval, current_sandbox, &preset);
            let name = if preset.id == "auto" && windows_degraded_sandbox_enabled {
                "Default (non-elevated sandbox)".to_string()
            } else {
                preset.label.to_string()
            };
            let disabled_reason = match self.config.approval_policy.can_set(&preset.approval) {
                Ok(()) => None,
                Err(err) => Some(err.to_string()),
            };
            let action = self.approval_preset_action(&preset, include_read_only);
            items.push(ApprovalModeItem {
                name,
                description: Some(preset.description.to_string()),
                is_current,
                disabled_reason,
                action,
            });
        }

        let mut header = vec![
            "Update Model Permissions".bold().into(),
            "Choose what Adam can do without approval.".dim().into(),
        ];
        if show_elevate_sandbox_hint {
            header.push("".into());
            header.push(
                vec![
                    "Tip: run ".dim(),
                    "/setup-elevated-sandbox".cyan(),
                    " to upgrade the Windows sandbox.".dim(),
                ]
                .into(),
            );
        }
        header.push("".into());

        self.approval_mode_modal = Some(ApprovalModeModal::new(header, items));
        self.chat_widget.request_redraw_for_ui();
    }

    fn approval_preset_action(
        &self,
        preset: &ApprovalPreset,
        include_read_only: bool,
    ) -> ApprovalModeAction {
        let requires_confirmation = preset.id == "full-access"
            && !self
                .config
                .notices
                .hide_full_access_warning
                .unwrap_or(false);
        if requires_confirmation {
            return ApprovalModeAction::OpenFullAccessConfirmation {
                preset: preset.clone(),
                return_to_permissions: !include_read_only,
            };
        }

        if preset.id == "auto" {
            #[cfg(target_os = "windows")]
            {
                if WindowsSandboxLevel::from_config(&self.config) == WindowsSandboxLevel::Disabled {
                    if adam_agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                        && adam_agent::windows_sandbox::sandbox_setup_is_complete(
                            self.config.adam_home.as_path(),
                        )
                    {
                        return ApprovalModeAction::EnableWindowsSandboxForAgentMode {
                            preset: preset.clone(),
                            mode: WindowsSandboxEnableMode::Elevated,
                            counter: None,
                        };
                    }
                    return ApprovalModeAction::OpenWindowsSandboxEnablePrompt {
                        preset: preset.clone(),
                    };
                }
                if let Some((sample_paths, extra_count, failed_scan)) =
                    self.chat_widget.world_writable_warning_details()
                {
                    return ApprovalModeAction::OpenWorldWritableWarningConfirmation {
                        preset: Some(preset.clone()),
                        sample_paths,
                        extra_count,
                        failed_scan,
                    };
                }
            }
        }

        ApprovalModeAction::ApplyPreset {
            approval: preset.approval,
            sandbox: preset.sandbox.clone(),
        }
    }

    fn open_full_access_confirmation_modal(
        &mut self,
        preset: ApprovalPreset,
        return_to_permissions: bool,
    ) {
        self.chat_widget.dismiss_active_view();
        let approval = preset.approval;
        let sandbox = preset.sandbox;
        let header = vec![
            "Enable full access?".bold().into(),
            vec![
                "When Adam runs with full access, it can edit any file on your computer and run commands with network, without your approval. ".into(),
                "Exercise caution when enabling full access. This significantly increases the risk of data loss, leaks, or unexpected behavior.".red(),
            ]
            .into(),
            "".into(),
        ];
        let items = vec![
            ApprovalModeItem {
                name: "Yes, continue anyway".to_string(),
                description: Some("Apply full access for this session".to_string()),
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::ConfirmFullAccess {
                    approval,
                    sandbox: sandbox.clone(),
                    remember: false,
                },
            },
            ApprovalModeItem {
                name: "Yes, and don't ask again".to_string(),
                description: Some("Enable full access and remember this choice".to_string()),
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::ConfirmFullAccess {
                    approval,
                    sandbox,
                    remember: true,
                },
            },
            ApprovalModeItem {
                name: "Cancel".to_string(),
                description: Some("Go back without enabling full access".to_string()),
                is_current: false,
                disabled_reason: None,
                action: if return_to_permissions {
                    ApprovalModeAction::OpenPermissions
                } else {
                    ApprovalModeAction::OpenApprovals
                },
            },
        ];
        self.approval_mode_modal = Some(ApprovalModeModal::new(header, items));
        self.chat_widget.request_redraw_for_ui();
    }

    fn open_world_writable_warning_confirmation_modal(
        &mut self,
        preset: Option<ApprovalPreset>,
        sample_paths: Vec<String>,
        extra_count: usize,
        failed_scan: bool,
    ) {
        self.chat_widget.dismiss_active_view();
        let describe_policy = |policy: &SandboxPolicy| match policy {
            SandboxPolicy::WorkspaceWrite { .. } => "Agent mode",
            SandboxPolicy::ReadOnly => "Read-Only mode",
            SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. } => "Agent mode",
        };
        let mode_label = preset
            .as_ref()
            .map(|preset| describe_policy(&preset.sandbox))
            .unwrap_or_else(|| describe_policy(self.config.sandbox_policy.get()));
        let mut header = if failed_scan {
            vec![
                "Windows sandbox warning".bold().into(),
                vec![
                    "We couldn't complete the world-writable scan, so protections cannot be verified. ".into(),
                    format!(
                        "The Windows sandbox cannot guarantee protection in {mode_label}."
                    )
                    .red(),
                ]
                .into(),
            ]
        } else {
            vec![
                "Windows sandbox warning".bold().into(),
                "The Windows sandbox cannot protect writes to folders that are writable by Everyone."
                    .dim()
                    .into(),
                "Consider removing write access for Everyone from these folders:"
                    .dim()
                    .into(),
            ]
        };
        if !sample_paths.is_empty() {
            header.push("".into());
            for path in sample_paths {
                header.push(format!("  - {path}").into());
            }
            if extra_count > 0 {
                header.push(format!("and {extra_count} more").into());
            }
        }
        header.push("".into());

        let items = vec![
            ApprovalModeItem {
                name: "Continue".to_string(),
                description: Some(format!("Apply {mode_label} for this session")),
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::ConfirmWorldWritable {
                    preset: preset.clone(),
                    remember: false,
                },
            },
            ApprovalModeItem {
                name: "Continue and don't warn again".to_string(),
                description: Some(format!("Enable {mode_label} and remember this choice")),
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::ConfirmWorldWritable {
                    preset,
                    remember: true,
                },
            },
        ];
        self.approval_mode_modal = Some(ApprovalModeModal::new(header, items));
        self.chat_widget.request_redraw_for_ui();
    }

    #[cfg(target_os = "windows")]
    fn open_windows_sandbox_enable_prompt_modal(&mut self, preset: ApprovalPreset) {
        self.chat_widget.dismiss_active_view();
        if !adam_agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED {
            let header = vec![
                "Agent mode on Windows uses an experimental sandbox to limit network and filesystem access."
                    .bold()
                    .into(),
                "Learn more: https://developers.openai.com/codex/windows"
                    .dim()
                    .into(),
                "".into(),
            ];
            let items = vec![
                ApprovalModeItem {
                    name: "Enable experimental sandbox".to_string(),
                    description: None,
                    is_current: false,
                    disabled_reason: None,
                    action: ApprovalModeAction::EnableWindowsSandboxForAgentMode {
                        preset,
                        mode: WindowsSandboxEnableMode::Legacy,
                        counter: None,
                    },
                },
                ApprovalModeItem {
                    name: "Go back".to_string(),
                    description: None,
                    is_current: false,
                    disabled_reason: None,
                    action: ApprovalModeAction::OpenApprovals,
                },
            ];
            self.approval_mode_modal = Some(ApprovalModeModal::new(header, items));
            self.chat_widget.request_redraw_for_ui();
            return;
        }

        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let presets = builtin_approval_presets();
        let stay_full_access = presets
            .iter()
            .find(|preset| preset.id == "full-access")
            .is_some_and(|preset| {
                ChatWidget::preset_matches_current(current_approval, current_sandbox, preset)
            });
        self.otel_manager
            .counter("adam.windows_sandbox.elevated_prompt_shown", 1, &[]);

        let stay_label = if stay_full_access {
            "Stay in Agent Full Access".to_string()
        } else {
            "Stay in Read-Only".to_string()
        };
        let read_only_preset = (!stay_full_access)
            .then(|| {
                presets
                    .iter()
                    .find(|preset| preset.id == "read-only")
                    .cloned()
            })
            .flatten();

        let header = vec![
            "Set Up Agent Sandbox".bold().into(),
            "".into(),
            "Agent mode uses an experimental Windows sandbox that protects your files and prevents network access by default."
                .into(),
            "Learn more: https://developers.openai.com/codex/windows"
                .dim()
                .into(),
            "".into(),
        ];
        let items = vec![
            ApprovalModeItem {
                name: "Set up agent sandbox (requires elevation)".to_string(),
                description: None,
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::BeginWindowsSandboxElevatedSetup {
                    preset,
                    counter: Some("adam.windows_sandbox.elevated_prompt_accept"),
                },
            },
            ApprovalModeItem {
                name: stay_label,
                description: None,
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::StayInCurrentWindowsMode {
                    read_only_preset,
                    counter: "adam.windows_sandbox.elevated_prompt_decline",
                },
            },
        ];
        self.approval_mode_modal = Some(ApprovalModeModal::new(header, items));
        self.chat_widget.request_redraw_for_ui();
    }

    #[cfg(target_os = "windows")]
    fn open_windows_sandbox_fallback_prompt_modal(
        &mut self,
        preset: ApprovalPreset,
        reason: WindowsSandboxFallbackReason,
    ) {
        let _ = reason;
        self.chat_widget.dismiss_active_view();
        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let presets = builtin_approval_presets();
        let stay_full_access = presets
            .iter()
            .find(|preset| preset.id == "full-access")
            .is_some_and(|preset| {
                ChatWidget::preset_matches_current(current_approval, current_sandbox, preset)
            });
        let stay_label = if stay_full_access {
            "Stay in Agent Full Access".to_string()
        } else {
            "Stay in Read-Only".to_string()
        };
        let read_only_preset = (!stay_full_access)
            .then(|| {
                presets
                    .iter()
                    .find(|preset| preset.id == "read-only")
                    .cloned()
            })
            .flatten();
        let header = vec![
            "Use Non-Elevated Sandbox?".bold().into(),
            "".into(),
            "Elevation failed. You can also use a non-elevated sandbox, which protects your files and prevents network access under most circumstances. However, it carries greater risk if prompt injected."
                .into(),
            "Learn more: https://developers.openai.com/codex/windows"
                .dim()
                .into(),
            "".into(),
        ];
        let items = vec![
            ApprovalModeItem {
                name: "Try elevated agent sandbox setup again".to_string(),
                description: None,
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::BeginWindowsSandboxElevatedSetup {
                    preset: preset.clone(),
                    counter: Some("adam.windows_sandbox.fallback_retry_elevated"),
                },
            },
            ApprovalModeItem {
                name: "Use non-elevated agent sandbox".to_string(),
                description: None,
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::EnableWindowsSandboxForAgentMode {
                    preset,
                    mode: WindowsSandboxEnableMode::Legacy,
                    counter: Some("adam.windows_sandbox.fallback_use_legacy"),
                },
            },
            ApprovalModeItem {
                name: stay_label,
                description: None,
                is_current: false,
                disabled_reason: None,
                action: ApprovalModeAction::StayInCurrentWindowsMode {
                    read_only_preset,
                    counter: "adam.windows_sandbox.fallback_stay_current",
                },
            },
        ];
        self.approval_mode_modal = Some(ApprovalModeModal::new(header, items));
        self.chat_widget.request_redraw_for_ui();
    }

    #[cfg(target_os = "windows")]
    fn maybe_prompt_windows_sandbox_enable_modal(&mut self) {
        if self.config.forced_auto_mode_downgraded_on_windows
            && WindowsSandboxLevel::from_config(&self.config) == WindowsSandboxLevel::Disabled
            && let Some(preset) = builtin_approval_presets()
                .into_iter()
                .find(|preset| preset.id == "auto")
        {
            self.open_windows_sandbox_enable_prompt_modal(preset);
        }
    }

    fn handle_approval_mode_action(&mut self, action: ApprovalModeAction) {
        match action {
            ApprovalModeAction::ApplyPreset { approval, sandbox } => {
                self.send_approval_preset_events(approval, sandbox);
            }
            ApprovalModeAction::OpenApprovals => self.open_approvals_modal(),
            ApprovalModeAction::OpenPermissions => self.open_permissions_modal(),
            ApprovalModeAction::OpenFullAccessConfirmation {
                preset,
                return_to_permissions,
            } => {
                self.open_full_access_confirmation_modal(preset, return_to_permissions);
            }
            ApprovalModeAction::OpenWorldWritableWarningConfirmation {
                preset,
                sample_paths,
                extra_count,
                failed_scan,
            } => {
                self.open_world_writable_warning_confirmation_modal(
                    preset,
                    sample_paths,
                    extra_count,
                    failed_scan,
                );
            }
            #[cfg(target_os = "windows")]
            ApprovalModeAction::OpenWindowsSandboxEnablePrompt { preset } => {
                self.open_windows_sandbox_enable_prompt_modal(preset);
            }
            ApprovalModeAction::ConfirmFullAccess {
                approval,
                sandbox,
                remember,
            } => {
                self.send_approval_preset_events(approval, sandbox);
                self.app_event_tx
                    .send(AppEvent::UpdateFullAccessWarningAcknowledged(true));
                if remember {
                    self.app_event_tx
                        .send(AppEvent::PersistFullAccessWarningAcknowledged);
                }
            }
            ApprovalModeAction::ConfirmWorldWritable { preset, remember } => {
                self.handle_world_writable_confirmation_action(preset, remember);
            }
            #[cfg(target_os = "windows")]
            ApprovalModeAction::BeginWindowsSandboxElevatedSetup { preset, counter } => {
                if let Some(counter) = counter {
                    self.otel_manager.counter(counter, 1, &[]);
                }
                self.app_event_tx
                    .send(AppEvent::BeginWindowsSandboxElevatedSetup { preset });
            }
            #[cfg(target_os = "windows")]
            ApprovalModeAction::EnableWindowsSandboxForAgentMode {
                preset,
                mode,
                counter,
            } => {
                if let Some(counter) = counter {
                    self.otel_manager.counter(counter, 1, &[]);
                }
                self.app_event_tx
                    .send(AppEvent::EnableWindowsSandboxForAgentMode { preset, mode });
            }
            #[cfg(target_os = "windows")]
            ApprovalModeAction::StayInCurrentWindowsMode {
                read_only_preset,
                counter,
            } => {
                self.otel_manager.counter(counter, 1, &[]);
                if let Some(preset) = read_only_preset {
                    self.send_approval_preset_events(preset.approval, preset.sandbox);
                }
            }
        }
    }

    fn handle_world_writable_confirmation_action(
        &mut self,
        preset: Option<ApprovalPreset>,
        remember: bool,
    ) {
        if remember {
            self.app_event_tx
                .send(AppEvent::UpdateWorldWritableWarningAcknowledged(true));
            self.app_event_tx
                .send(AppEvent::PersistWorldWritableWarningAcknowledged);
        } else if preset.is_some() {
            self.app_event_tx.send(AppEvent::SkipNextWorldWritableScan);
        }

        if let Some(preset) = preset {
            self.send_approval_preset_events(preset.approval, preset.sandbox);
        }
    }

    fn send_approval_preset_events(&self, approval: AskForApproval, sandbox: SandboxPolicy) {
        self.app_event_tx
            .send(AppEvent::CodexOp(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(approval),
                sandbox_policy: Some(sandbox.clone()),
                windows_sandbox_level: None,
                model: None,
                effort: None,
                summary: None,
                identity: None,
                personality: None,
            }));
        self.app_event_tx
            .send(AppEvent::UpdateAskForApprovalPolicy(approval));
        self.app_event_tx
            .send(AppEvent::UpdateSandboxPolicy(sandbox));
    }

    fn open_provider_config_modal(&mut self, mode: ProviderConfigModalMode) {
        self.chat_widget.dismiss_active_view();
        self.provider_config_modal = Some(ProviderConfigModalState {
            mode,
            modal: ProviderConfigModal::new(
                self.config.adam_home.clone(),
                self.app_event_tx.clone(),
                self.chat_widget.frame_requester(),
            ),
        });
        self.chat_widget.request_redraw_for_ui();
    }

    fn open_review_modal(&mut self) {
        self.chat_widget.dismiss_active_view();
        self.review_modal = Some(ReviewModal::new(
            self.config.cwd.clone(),
            self.app_event_tx.clone(),
        ));
        self.chat_widget.request_redraw_for_ui();
    }

    fn ensure_review_modal(&mut self) {
        if self.review_modal.is_none() {
            self.open_review_modal();
        }
    }

    fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    async fn launch_external_editor(&mut self, tui: &mut tui::Tui) {
        let editor_cmd = match external_editor::resolve_editor_command() {
            Ok(cmd) => cmd,
            Err(external_editor::EditorError::MissingEditor) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(
                        "Cannot open external editor: set $VISUAL or $EDITOR before starting Adam."
                            .to_string(),
                    ));
                self.reset_external_editor_state(tui);
                return;
            }
            Err(err) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
                self.reset_external_editor_state(tui);
                return;
            }
        };

        let seed = self.chat_widget.composer_text_with_pending();
        let editor_result = tui
            .with_restored(tui::RestoreMode::KeepRaw, || async {
                external_editor::run_editor(&seed, &editor_cmd).await
            })
            .await;
        self.reset_external_editor_state(tui);

        match editor_result {
            Ok(new_text) => {
                // Trim trailing whitespace
                let cleaned = new_text.trim_end().to_string();
                self.chat_widget.apply_external_edit(cleaned);
            }
            Err(err) => {
                self.chat_widget
                    .add_to_history(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )));
            }
        }
        tui.frame_requester().schedule_frame();
    }

    fn request_external_editor_launch(&mut self, tui: &mut tui::Tui) {
        self.chat_widget
            .set_external_editor_state(ExternalEditorState::Requested);
        self.chat_widget.set_footer_hint_override(Some(vec![(
            EXTERNAL_EDITOR_HINT.to_string(),
            String::new(),
        )]));
        tui.frame_requester().schedule_frame();
    }

    fn reset_external_editor_state(&mut self, tui: &mut tui::Tui) {
        self.chat_widget
            .set_external_editor_state(ExternalEditorState::Closed);
        self.chat_widget.set_footer_hint_override(None);
        tui.frame_requester().schedule_frame();
    }

    async fn handle_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                self.overlay = Some(Overlay::new_transcript(
                    self.transcript_cells.clone(),
                    self.chat_widget.clipboard_text_config(),
                ));
                tui.frame_requester().schedule_frame();
            }
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                // Only launch the external editor if there is no overlay and the bottom pane is not in use.
                // Note that it can be launched while a task is running to enable editing while the previous turn is ongoing.
                if self.overlay.is_none()
                    && self.chat_widget.can_launch_external_editor()
                    && self.chat_widget.external_editor_state() == ExternalEditorState::Closed
                {
                    self.request_external_editor_launch(tui);
                }
            }
            // Esc primes/advances backtracking only in normal (not working) mode
            // with the composer focused and empty. In any other state, forward
            // Esc so the active UI (e.g. status indicator, modals, popups)
            // handles it.
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if self.chat_widget.is_normal_backtrack_mode()
                    && self.chat_widget.composer_is_empty()
                {
                    self.handle_backtrack_esc_key(tui);
                } else {
                    self.chat_widget.handle_key_event(key_event);
                }
            }
            // Enter confirms backtrack when primed + count > 0. Otherwise pass to widget.
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.nth_user_message != usize::MAX
                && self.chat_widget.composer_is_empty() =>
            {
                if let Some(selection) = self.confirm_backtrack_from_main() {
                    self.apply_backtrack_selection(tui, selection);
                }
            }
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                // Any non-Esc key press should cancel a primed backtrack.
                // This avoids stale "Esc-primed" state after the user starts typing
                // (even if they later backspace to empty).
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_widget.handle_key_event(key_event);
            }
            _ => {
                // Ignore Release key events.
            }
        };
    }

    #[cfg(target_os = "windows")]
    fn spawn_world_writable_scan(
        cwd: PathBuf,
        env_map: std::collections::HashMap<String, String>,
        logs_base_dir: PathBuf,
        sandbox_policy: adam_agent::protocol::SandboxPolicy,
        tx: AppEventSender,
    ) {
        tokio::task::spawn_blocking(move || {
            let result = adam_windows_sandbox::apply_world_writable_scan_and_denies(
                &logs_base_dir,
                &cwd,
                &env_map,
                &sandbox_policy,
                Some(logs_base_dir.as_path()),
            );
            if result.is_err() {
                // Scan failed: warn without examples.
                tx.send(AppEvent::OpenWorldWritableWarningConfirmation {
                    preset: None,
                    sample_paths: Vec::new(),
                    extra_count: 0usize,
                    failed_scan: true,
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_backtrack::BacktrackSelection;
    use crate::app_backtrack::BacktrackState;
    use crate::app_backtrack::PendingBacktrackRollback;
    use crate::app_backtrack::user_count;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
    use crate::file_search::FileSearchManager;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use crate::history_cell::PlainHistoryCell;
    use crate::history_cell::UserHistoryCell;
    use crate::history_cell::new_session_info;
    use crate::provider_config::ApiProviderDialect;
    use crate::provider_config::CustomProviderConfig;
    use crate::provider_config::persist_custom_provider_files;
    use adam_agent::AuthManager;
    use adam_agent::CodexAuth;
    use adam_agent::ThreadManager;
    use adam_agent::config::CONFIG_TOML_FILE;
    use adam_agent::config::ConfigBuilder;
    use adam_agent::config::ConfigOverrides;
    use adam_agent::config::models_json::ModelsDialect;
    use adam_agent::config::models_json::ModelsEndpoint;
    use adam_agent::config::models_json::ModelsJson;
    use adam_agent::config::types::McpServerConfig;
    use adam_agent::config::types::McpServerTransportConfig;

    use adam_agent::features::Feature;
    use adam_agent::models_manager::manager::ModelsManager;
    use adam_agent::protocol::AskForApproval;
    use adam_agent::protocol::CollabAgentRef;
    use adam_agent::protocol::CollabAgentStatusEntry;
    use adam_agent::protocol::CollabCloseEndEvent;
    use adam_agent::protocol::CollabWaitingEndEvent;
    use adam_agent::protocol::ErrorEvent;
    use adam_agent::protocol::Event;
    use adam_agent::protocol::EventMsg;
    use adam_agent::protocol::McpListToolsResponseEvent;
    use adam_agent::protocol::ReviewRequest;
    use adam_agent::protocol::ReviewTarget;
    use adam_agent::protocol::SandboxPolicy;
    use adam_agent::protocol::SessionConfiguredEvent;
    use adam_agent::protocol::SessionSource;
    use adam_agent::protocol::ThreadRolledBackEvent;
    use adam_agent::protocol::TurnAbortedEvent;
    use adam_agent::protocol::TurnCompleteEvent;
    use adam_agent::protocol::TurnStartedEvent;
    use adam_otel::OtelManager;
    use adam_protocol::ThreadId;
    use adam_protocol::config_types::IdentityKind;
    use adam_protocol::config_types::TrustLevel;
    use adam_protocol::user_input::TextElement;
    use crossterm::event::KeyModifiers;
    use insta::assert_snapshot;
    use mcp_types::Tool;
    use mcp_types::ToolInputSchema;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tempfile::tempdir;
    use tokio::time;

    #[test]
    fn normalize_harness_overrides_resolves_relative_add_dirs() -> Result<()> {
        let temp_dir = tempdir()?;
        let base_cwd = temp_dir.path().join("base");
        std::fs::create_dir_all(&base_cwd)?;

        let overrides = ConfigOverrides {
            additional_writable_roots: vec![PathBuf::from("rel")],
            ..Default::default()
        };
        let normalized = normalize_harness_overrides_for_cwd(overrides, &base_cwd)?;

        assert_eq!(
            normalized.additional_writable_roots,
            vec![base_cwd.join("rel")]
        );
        Ok(())
    }

    #[tokio::test]
    async fn enqueue_thread_event_does_not_block_when_channel_full() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(1));
        app.set_thread_active(thread_id, true).await;

        let event = Event {
            id: String::new(),
            msg: EventMsg::ShutdownComplete,
        };

        app.enqueue_thread_event(thread_id, event.clone()).await?;
        time::timeout(
            Duration::from_millis(50),
            app.enqueue_thread_event(thread_id, event),
        )
        .await
        .expect("enqueue_thread_event blocked on a full channel")?;

        let mut rx = app
            .thread_event_channels
            .get_mut(&thread_id)
            .expect("missing thread channel")
            .receiver
            .take()
            .expect("missing receiver");

        time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for first event")
            .expect("channel closed unexpectedly");
        time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for second event")
            .expect("channel closed unexpectedly");

        Ok(())
    }

    #[tokio::test]
    async fn rollback_confirmation_requests_transcript_replay() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: thread_id,
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                identity_kind: IdentityKind::Nobody,
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: Some(PathBuf::new()),
            }),
        });
        app.transcript_cells = vec![
            Arc::new(UserHistoryCell {
                message: "question".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
            }),
            Arc::new(PlainHistoryCell::new(vec!["answer".into()])),
        ];
        app.backtrack.pending_rollback = Some(PendingBacktrackRollback {
            selection: BacktrackSelection {
                nth_user_message: 0,
                prefill: String::new(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
            },
            thread_id: Some(thread_id),
        });

        app.handle_backtrack_event(&EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        }));

        assert!(app.backtrack_render_pending);
        assert!(app.transcript_cells.is_empty());
    }

    #[tokio::test]
    async fn non_git_baseline_tracker_wait_ready_returns_stored_result() {
        let tracker = NonGitBaselineTracker::default();
        tracker.store(Err("boom".to_string()));

        assert_eq!(tracker.wait_ready().await, Err("boom".to_string()));
    }

    #[tokio::test]
    async fn non_git_baseline_tracker_wait_ready_wakes_after_store() {
        let tracker = Arc::new(NonGitBaselineTracker::default());
        let waiting_tracker = tracker.clone();
        let wait_task = tokio::spawn(async move { waiting_tracker.wait_ready().await });

        tokio::task::yield_now().await;
        tracker.store(Err("later".to_string()));

        let result = time::timeout(Duration::from_secs(1), wait_task)
            .await
            .expect("wait task should complete")
            .expect("wait task should not panic");
        assert_eq!(result, Err("later".to_string()));
    }

    #[tokio::test]
    async fn open_identity_modal_event_creates_centered_modal() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_feature_enabled(Feature::Identities, true);
        app.config = app.chat_widget.config_ref().clone();

        app.open_identity_modal();

        assert!(app.identity_modal.is_some());
    }

    #[tokio::test]
    async fn identity_modal_enter_updates_identity() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_feature_enabled(Feature::Identities, true);
        app.config = app.chat_widget.config_ref().clone();

        app.open_identity_modal();
        app.handle_identity_modal_key_event(KeyEvent::from(KeyCode::Down));
        app.handle_identity_modal_key_event(KeyEvent::from(KeyCode::Enter));

        assert!(app.identity_modal.is_none());
        assert_eq!(
            app.chat_widget.active_identity_kind_for_ui(),
            IdentityKind::Planner
        );
        assert_eq!(
            app.config.last_selected_identity,
            Some(IdentityKind::Planner)
        );
    }

    #[tokio::test]
    async fn open_provider_config_modal_event_creates_centered_modal() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;

        app.open_provider_config_modal(ProviderConfigModalMode::InSession);

        assert!(app.provider_config_modal.is_some());
        assert_eq!(
            app.provider_config_modal.as_ref().map(|state| state.mode),
            Some(ProviderConfigModalMode::InSession)
        );
    }

    #[tokio::test]
    async fn open_review_modal_creates_centered_modal() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;

        app.open_review_modal();

        assert!(app.review_modal.is_some());
    }

    #[tokio::test]
    async fn start_review_closes_open_review_modal() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let review_request = ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        };
        app.open_review_modal();

        app.start_review(review_request.clone())
            .await
            .expect("start review");

        assert!(app.review_modal.is_none());
        match op_rx.try_recv() {
            Ok(Op::Review {
                review_request: got,
            }) => {
                assert_eq!(got, review_request);
            }
            other => panic!("expected inline review op, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn provider_in_session_modal_exit_only_closes_modal() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        app.open_provider_config_modal(ProviderConfigModalMode::InSession);

        let exit_mode = app.close_provider_config_modal_for_exit();

        assert_eq!(exit_mode, None);
        assert!(app.provider_config_modal.is_none());
    }

    #[tokio::test]
    async fn provider_in_session_modal_exit_requests_redraw() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        app.open_provider_config_modal(ProviderConfigModalMode::InSession);
        let (frame_requester, mut frame_rx) = tui::FrameRequester::test_with_receiver();

        let exit_mode = app.close_provider_config_modal_for_exit_with_redraw(&frame_requester);

        assert_eq!(exit_mode, None);
        assert!(app.provider_config_modal.is_none());
        time::timeout(Duration::from_millis(50), frame_rx.recv())
            .await
            .expect("timed out waiting for redraw")
            .expect("frame requester closed");
    }

    #[tokio::test]
    async fn provider_startup_modal_exit_returns_exit_mode() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        app.provider_config_modal = Some(ProviderConfigModalState {
            mode: ProviderConfigModalMode::Startup,
            modal: ProviderConfigModal::new(
                app.config.adam_home.clone(),
                app.app_event_tx.clone(),
                tui::FrameRequester::test_dummy(),
            ),
        });

        let exit_mode = app.close_provider_config_modal_for_exit();

        assert_eq!(exit_mode, Some(ExitMode::Immediate));
        assert!(app.provider_config_modal.is_none());
    }

    #[tokio::test]
    async fn shift_mouse_bypass_records_restore_deadline() {
        let mut app = make_test_app().await;
        assert_eq!(app.shift_mouse_bypass_restore_at, None);

        let restore_at = Instant::now() + SHIFT_MOUSE_BYPASS_DURATION;
        app.shift_mouse_bypass_active = true;
        app.shift_mouse_bypass_restore_at = Some(restore_at);

        assert!(app.shift_mouse_bypass_active);
        assert_eq!(app.shift_mouse_bypass_restore_at, Some(restore_at));
    }

    #[tokio::test]
    async fn draw_before_shift_mouse_bypass_deadline_keeps_bypass_active() {
        let mut app = make_test_app().await;
        let restore_at = Instant::now() + Duration::from_secs(60);
        app.shift_mouse_bypass_active = true;
        app.shift_mouse_bypass_restore_at = Some(restore_at);

        let should_restore = app
            .shift_mouse_bypass_restore_at
            .is_some_and(|restore_at| Instant::now() >= restore_at);

        assert!(!should_restore);
        assert!(app.shift_mouse_bypass_active);
        assert_eq!(app.shift_mouse_bypass_restore_at, Some(restore_at));
    }

    #[tokio::test]
    async fn draw_restores_shift_mouse_bypass_after_deadline() {
        let mut app = make_test_app().await;
        app.shift_mouse_bypass_active = true;
        app.shift_mouse_bypass_restore_at = Some(Instant::now() - Duration::from_millis(1));

        let mut restored = false;
        if app
            .shift_mouse_bypass_restore_at
            .is_some_and(|restore_at| Instant::now() >= restore_at)
        {
            app.restore_shift_mouse_bypass_with(|| restored = true, Some("Mouse capture restored"));
        }

        assert!(restored);
        assert!(!app.shift_mouse_bypass_active);
        assert_eq!(app.shift_mouse_bypass_restore_at, None);
    }

    #[tokio::test]
    async fn restore_shift_mouse_bypass_if_due_restores_after_deadline() {
        let mut app = make_test_app().await;
        app.shift_mouse_bypass_active = true;
        app.shift_mouse_bypass_restore_at = Some(Instant::now() - Duration::from_millis(1));

        let mut restored = false;
        app.restore_shift_mouse_bypass_if_due_with(|| restored = true);

        assert!(restored);
        assert!(!app.shift_mouse_bypass_active);
        assert_eq!(app.shift_mouse_bypass_restore_at, None);
    }

    #[tokio::test]
    async fn restore_shift_mouse_bypass_if_due_waits_until_deadline() {
        let mut app = make_test_app().await;
        let restore_at = Instant::now() + Duration::from_secs(60);
        app.shift_mouse_bypass_active = true;
        app.shift_mouse_bypass_restore_at = Some(restore_at);

        let mut restored = false;
        app.restore_shift_mouse_bypass_if_due_with(|| restored = true);

        assert!(!restored);
        assert!(app.shift_mouse_bypass_active);
        assert_eq!(app.shift_mouse_bypass_restore_at, Some(restore_at));
    }

    #[tokio::test]
    async fn restore_shift_mouse_bypass_clears_orphan_deadline_when_inactive() {
        let mut app = make_test_app().await;
        app.shift_mouse_bypass_active = false;
        app.shift_mouse_bypass_restore_at = Some(Instant::now());

        let mut restored = false;
        app.restore_shift_mouse_bypass_with(|| restored = true, Some("Mouse capture restored"));

        assert!(!restored);
        assert!(!app.shift_mouse_bypass_active);
        assert_eq!(app.shift_mouse_bypass_restore_at, None);
    }

    #[tokio::test]
    async fn request_changelog_uses_session_baseline_outside_git() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let temp_dir = tempdir().expect("tempdir");
        let cwd = temp_dir.path().to_path_buf();
        let path = cwd.join("tracked.txt");
        std::fs::write(&path, "before").expect("write initial file");
        app.config.cwd = cwd.clone();

        while app_event_rx.try_recv().is_ok() {}

        app.ensure_non_git_changelog_baseline(cwd.clone())
            .await
            .expect("prepare baseline");
        let tracker = app
            .non_git_changelog_baselines
            .get(&cwd)
            .cloned()
            .expect("baseline tracker");
        tracker.wait_ready().await.expect("baseline ready");
        std::fs::write(&path, "after").expect("update file");

        app.request_changelog().await;

        let event = time::timeout(std::time::Duration::from_secs(2), app_event_rx.recv())
            .await
            .expect("wait for changelog result")
            .expect("result event");

        match event {
            AppEvent::ChangelogResult(Ok(ChangelogOutput::Entries {
                display_root,
                entries,
            })) => {
                assert_eq!(display_root, cwd);
                assert_eq!(
                    entries,
                    vec![crate::changelog::ChangelogEntry {
                        kind: crate::changelog::ChangelogKind::Modified,
                        path,
                    }]
                );
            }
            other => panic!("expected changelog result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn request_changelog_missing_cwd_reports_error_event() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let temp_dir = tempdir().expect("tempdir");
        let missing_cwd = temp_dir.path().join("missing");
        app.config.cwd = missing_cwd.clone();

        while app_event_rx.try_recv().is_ok() {}

        app.request_changelog().await;

        let event = time::timeout(Duration::from_secs(2), app_event_rx.recv())
            .await
            .expect("wait for changelog error")
            .expect("result event");

        match event {
            AppEvent::ChangelogResult(Err(err)) => {
                assert!(
                    !err.is_empty(),
                    "expected changelog setup failure to produce a message"
                );
            }
            other => panic!("expected changelog error result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_agent_picker_shows_no_agents_message_when_empty() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

        app.open_agent_picker().await;

        assert!(app.agent_selection_modal.is_none());
        assert!(app.chat_widget.no_modal_or_popup_active());
        while let Ok(event) = app_event_rx.try_recv() {
            assert!(
                !matches!(event, AppEvent::UpdateFeatureFlags { .. }),
                "did not expect feature flag update event: {event:?}"
            );
        }
    }

    #[tokio::test]
    async fn open_agent_picker_allows_existing_agent_threads() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

        let thread_id = ThreadId::new();
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(4));

        app.open_agent_picker().await;

        let action = app
            .agent_selection_modal
            .as_mut()
            .expect("agent selection modal")
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(action, AgentSelectionModalAction::SelectThread(thread_id));
        assert!(app_event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn open_agent_picker_sets_centered_modal() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;

        let thread_id = ThreadId::new();
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(4));

        app.open_agent_picker().await;

        assert!(
            app.agent_selection_modal.is_some(),
            "expected App to own the /agent modal"
        );
        assert!(app.chat_widget.no_modal_or_popup_active());
    }

    #[tokio::test]
    async fn sidebar_agent_entries_match_active_agent_picker_threads() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let primary_id =
            ThreadId::from_string("00000000-0000-7000-8000-000000000001").expect("primary id");
        let active_id =
            ThreadId::from_string("00000000-0000-7000-8000-000000000002").expect("active id");
        let closed_id =
            ThreadId::from_string("00000000-0000-7000-8000-000000000003").expect("closed id");

        app.primary_thread_id = Some(primary_id);
        app.upsert_agent_picker_thread(
            primary_id,
            Some("Main".to_string()),
            Some("default".to_string()),
            AgentStatus::Running,
            false,
        );
        app.upsert_agent_picker_thread(
            active_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            AgentStatus::Completed(Some("done".to_string())),
            false,
        );
        app.upsert_agent_picker_thread(
            closed_id,
            Some("Closed".to_string()),
            Some("worker".to_string()),
            AgentStatus::Errored("tool timeout".to_string()),
            true,
        );

        assert_eq!(
            app.sidebar_agent_entries(),
            vec![AgentPanelEntry {
                thread_id: active_id,
                label: "Robie [explorer]".to_string(),
                status: AgentStatus::Completed(Some("done".to_string())),
            }]
        );
    }

    #[tokio::test]
    async fn collab_waiting_end_updates_sidebar_agent_status() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        app.handle_codex_event_now(Event {
            id: String::new(),
            msg: EventMsg::CollabWaitingEnd(CollabWaitingEndEvent {
                sender_thread_id: ThreadId::new(),
                call_id: "wait-1".to_string(),
                statuses: HashMap::new(),
                agent_statuses: vec![CollabAgentStatusEntry {
                    agent: CollabAgentRef {
                        thread_id,
                        agent_nickname: Some("Robie".to_string()),
                        agent_role: Some("explorer".to_string()),
                    },
                    status: AgentStatus::Completed(Some("done".to_string())),
                }],
            }),
        });

        assert_eq!(
            app.agent_picker_threads.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                status: AgentStatus::Completed(Some("done".to_string())),
                is_closed: false,
            })
        );
        assert_eq!(
            app.sidebar_agent_entries(),
            vec![AgentPanelEntry {
                thread_id,
                label: "Robie [explorer]".to_string(),
                status: AgentStatus::Completed(Some("done".to_string())),
            }]
        );
    }

    #[tokio::test]
    async fn collab_close_end_marks_sidebar_agent_closed() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        app.upsert_agent_picker_thread(
            thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            AgentStatus::Running,
            false,
        );
        app.handle_codex_event_now(Event {
            id: String::new(),
            msg: EventMsg::CollabCloseEnd(CollabCloseEndEvent {
                sender_thread_id: ThreadId::new(),
                receiver_thread_id: thread_id,
                receiver_agent_nickname: Some("Robie".to_string()),
                receiver_agent_role: Some("explorer".to_string()),
                call_id: "close-1".to_string(),
                status: AgentStatus::Errored("tool timeout".to_string()),
            }),
        });

        assert_eq!(
            app.agent_picker_threads.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                status: AgentStatus::Errored("tool timeout".to_string()),
                is_closed: true,
            })
        );
        assert_eq!(app.sidebar_agent_entries(), Vec::new());
    }

    #[tokio::test]
    async fn handle_thread_created_preserves_cached_agent_status() {
        let mut app = make_test_app().await;
        let thread = app
            .server
            .start_thread(app.config.clone())
            .await
            .expect("start thread");

        app.upsert_agent_picker_thread(
            thread.thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            AgentStatus::Completed(Some("done".to_string())),
            false,
        );

        app.handle_thread_created(thread.thread_id)
            .await
            .expect("attach thread listener");

        assert_eq!(
            app.agent_picker_threads.get(&thread.thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: None,
                agent_role: None,
                status: AgentStatus::Completed(Some("done".to_string())),
                is_closed: false,
            })
        );
    }

    #[tokio::test]
    async fn thread_lifecycle_turn_complete_updates_sidebar_agent_status() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        app.upsert_agent_picker_thread(
            thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            AgentStatus::Running,
            false,
        );
        app.enqueue_thread_event(
            thread_id,
            Event {
                id: "complete".to_string(),
                msg: EventMsg::TurnComplete(TurnCompleteEvent {
                    last_agent_message: Some("done".to_string()),
                }),
            },
        )
        .await
        .expect("enqueue turn complete");

        assert_eq!(
            app.sidebar_agent_entries(),
            vec![AgentPanelEntry {
                thread_id,
                label: "Robie [explorer]".to_string(),
                status: AgentStatus::Completed(Some("done".to_string())),
            }]
        );
    }

    #[tokio::test]
    async fn thread_lifecycle_interrupted_updates_sidebar_agent_status() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        app.upsert_agent_picker_thread(
            thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            AgentStatus::Running,
            false,
        );
        app.enqueue_thread_event(
            thread_id,
            Event {
                id: "interrupted".to_string(),
                msg: EventMsg::TurnAborted(TurnAbortedEvent {
                    reason: TurnAbortReason::Interrupted,
                }),
            },
        )
        .await
        .expect("enqueue turn aborted");

        assert_eq!(
            app.agent_picker_threads.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                status: AgentStatus::Interrupted,
                is_closed: false,
            })
        );
    }

    #[tokio::test]
    async fn thread_lifecycle_error_updates_sidebar_agent_status() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        app.upsert_agent_picker_thread(
            thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            AgentStatus::Running,
            false,
        );
        app.enqueue_thread_event(
            thread_id,
            Event {
                id: "error".to_string(),
                msg: EventMsg::Error(ErrorEvent {
                    message: "boom".to_string(),
                    codex_error_info: None,
                }),
            },
        )
        .await
        .expect("enqueue error");

        assert_eq!(
            app.agent_picker_threads.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                status: AgentStatus::Errored("boom".to_string()),
                is_closed: false,
            })
        );
    }

    #[tokio::test]
    async fn thread_lifecycle_shutdown_marks_agent_closed() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        app.upsert_agent_picker_thread(
            thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            AgentStatus::Running,
            false,
        );
        app.enqueue_thread_event(
            thread_id,
            Event {
                id: "shutdown".to_string(),
                msg: EventMsg::ShutdownComplete,
            },
        )
        .await
        .expect("enqueue shutdown");

        assert_eq!(
            app.agent_picker_threads.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                status: AgentStatus::Shutdown,
                is_closed: true,
            })
        );
        assert_eq!(app.sidebar_agent_entries(), Vec::new());
    }

    #[tokio::test]
    async fn mirrored_detached_review_lifecycle_updates_child_not_parent() {
        let mut app = make_test_app().await;
        let parent_thread_id = ThreadId::new();
        let child_thread_id = ThreadId::new();

        app.upsert_agent_picker_thread(
            parent_thread_id,
            Some("Parent".to_string()),
            Some("default".to_string()),
            AgentStatus::Running,
            false,
        );
        app.upsert_agent_picker_thread(
            child_thread_id,
            Some("Review".to_string()),
            Some("review".to_string()),
            AgentStatus::Running,
            false,
        );
        app.review_parent_by_child
            .insert(child_thread_id, parent_thread_id);

        app.enqueue_thread_event(
            child_thread_id,
            Event {
                id: "review-complete".to_string(),
                msg: EventMsg::TurnComplete(TurnCompleteEvent {
                    last_agent_message: Some("done".to_string()),
                }),
            },
        )
        .await
        .expect("enqueue child turn complete");

        assert_eq!(
            app.agent_picker_threads.get(&parent_thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Parent".to_string()),
                agent_role: Some("default".to_string()),
                status: AgentStatus::Running,
                is_closed: false,
            })
        );
        assert_eq!(
            app.agent_picker_threads.get(&child_thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Review".to_string()),
                agent_role: Some("review".to_string()),
                status: AgentStatus::Completed(Some("done".to_string())),
                is_closed: false,
            })
        );
    }

    #[tokio::test]
    async fn thread_scoped_history_is_dropped_after_thread_switch() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;

        let old_thread_id = ThreadId::new();
        let new_thread_id = ThreadId::new();
        let configure = |thread_id| Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: thread_id,
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                identity_kind: IdentityKind::Nobody,
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: Some(PathBuf::new()),
            }),
        };

        app.chat_widget.handle_codex_event(configure(old_thread_id));
        app.chat_widget.handle_codex_event(configure(new_thread_id));

        let inserted = app.insert_history_cell_arc_for_thread(
            old_thread_id,
            Arc::new(PlainHistoryCell::new(vec!["stale".into()])),
        );

        assert!(!inserted);
        assert!(app.transcript_cells.is_empty());
    }

    #[tokio::test]
    async fn open_agent_picker_keeps_missing_threads_for_replay() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(1));

        app.open_agent_picker().await;

        assert_eq!(app.thread_event_channels.contains_key(&thread_id), true);
        assert_eq!(
            app.agent_picker_threads.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: None,
                agent_role: None,
                status: AgentStatus::Shutdown,
                is_closed: true,
            })
        );
    }

    #[tokio::test]
    async fn open_agent_picker_keeps_cached_closed_threads() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(1));
        app.agent_picker_threads.insert(
            thread_id,
            AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                status: AgentStatus::Interrupted,
                is_closed: false,
            },
        );

        app.open_agent_picker().await;

        assert_eq!(app.thread_event_channels.contains_key(&thread_id), true);
        assert_eq!(
            app.agent_picker_threads.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                status: AgentStatus::Interrupted,
                is_closed: true,
            })
        );
    }

    #[tokio::test]
    async fn handle_thread_created_routes_live_events_back_through_app() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread = app
            .server
            .start_thread(app.config.clone())
            .await
            .expect("start thread");

        app.handle_thread_created(thread.thread_id)
            .await
            .expect("attach thread listener");
        thread
            .thread
            .submit(Op::SetThreadName {
                name: "review-child".to_string(),
            })
            .await
            .expect("rename thread");

        let observed_thread_id = time::timeout(Duration::from_secs(2), async {
            loop {
                let event = app_event_rx
                    .recv()
                    .await
                    .expect("thread event should be present");
                if let AppEvent::ThreadEventReceived {
                    thread_id,
                    event:
                        Event {
                            msg: EventMsg::ThreadNameUpdated(_),
                            ..
                        },
                } = event
                {
                    break thread_id;
                }
            }
        })
        .await
        .expect("thread rename event should arrive in time");
        assert_eq!(observed_thread_id, thread.thread_id);
    }

    #[tokio::test]
    async fn start_review_submits_inline_review_when_detached_feature_disabled() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let review_request = ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        };

        app.start_review(review_request.clone())
            .await
            .expect("start review");

        match op_rx.try_recv() {
            Ok(Op::Review {
                review_request: got,
            }) => {
                assert_eq!(got, review_request);
            }
            other => panic!("expected inline review op, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_review_creates_detached_review_thread_when_feature_enabled() {
        let mut app = make_test_app().await;
        app.config.features.enable(Feature::DetachedReview);
        app.chat_widget
            .set_feature_enabled(Feature::DetachedReview, true);

        let parent = app
            .server
            .start_thread(app.config.clone())
            .await
            .expect("start parent thread");
        app.enqueue_primary_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(parent.session_configured.clone()),
        })
        .await
        .expect("enqueue primary event");

        app.start_review(ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        })
        .await
        .expect("start detached review");

        let child_ids: Vec<_> = app
            .agent_picker_threads
            .keys()
            .copied()
            .filter(|thread_id| *thread_id != parent.thread_id)
            .collect();
        assert_eq!(child_ids.len(), 1, "expected one detached review thread");

        let child_id = child_ids[0];
        assert_eq!(app.active_thread_id, Some(parent.thread_id));
        assert!(app.thread_event_channels.contains_key(&child_id));
        assert_eq!(
            app.review_parent_by_child.get(&child_id),
            Some(&parent.thread_id)
        );
        assert_eq!(
            app.agent_picker_threads.get(&child_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: None,
                agent_role: Some("review".to_string()),
                status: AgentStatus::PendingInit,
                is_closed: false,
            })
        );
    }

    #[tokio::test]
    async fn detached_review_events_are_mirrored_to_parent_thread() {
        let mut app = make_test_app().await;
        let parent_thread_id = ThreadId::new();
        let child_thread_id = ThreadId::new();
        app.ensure_thread_channel(parent_thread_id);
        app.activate_thread_channel(parent_thread_id).await;
        app.ensure_thread_channel(child_thread_id);
        app.review_parent_by_child
            .insert(child_thread_id, parent_thread_id);

        let review_request = ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        };
        app.enqueue_thread_event(
            child_thread_id,
            Event {
                id: "entered-review".to_string(),
                msg: EventMsg::TurnStarted(TurnStartedEvent {
                    model_context_window: None,
                    identity_kind: IdentityKind::default(),
                }),
            },
        )
        .await
        .expect("enqueue turn started");
        app.enqueue_thread_event(
            child_thread_id,
            Event {
                id: "review-begin".to_string(),
                msg: EventMsg::EnteredReviewMode(review_request.clone()),
            },
        )
        .await
        .expect("enqueue entered review");

        let parent_snapshot = app
            .thread_event_channels
            .get(&parent_thread_id)
            .expect("parent channel")
            .store
            .lock()
            .await
            .snapshot();
        assert!(
            parent_snapshot
                .events
                .iter()
                .any(|event| matches!(&event.msg, EventMsg::TurnStarted(_)))
        );
        assert!(parent_snapshot.events.iter().any(|event| matches!(
            &event.msg,
            EventMsg::EnteredReviewMode(got) if got == &review_request
        )));

        app.enqueue_thread_event(
            child_thread_id,
            Event {
                id: "review-complete".to_string(),
                msg: EventMsg::TurnComplete(TurnCompleteEvent {
                    last_agent_message: None,
                }),
            },
        )
        .await
        .expect("enqueue turn complete");

        assert!(!app.review_parent_by_child.contains_key(&child_thread_id));
    }

    #[test]
    fn agent_picker_item_name_snapshot() {
        let thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");
        let snapshot = [
            format!(
                "{} | {}",
                format_agent_picker_item_name(Some("Robie"), Some("explorer"), true),
                thread_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(Some("Robie"), Some("explorer"), false),
                thread_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(Some("Robie"), None, false),
                thread_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(None, Some("explorer"), false),
                thread_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(None, None, false),
                thread_id
            ),
        ]
        .join("\n");

        assert_snapshot!("agent_picker_item_name", snapshot);
    }

    async fn make_test_app() -> App {
        let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let server = Arc::new(ThreadManager::with_models_provider(
            CodexAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ));
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let otel_manager = test_otel_manager(&config, model.as_str());

        App {
            server: server.clone(),
            otel_manager,
            app_event_tx,
            chat_widget,
            auth_manager,
            config,
            active_profile: None,
            cli_kv_overrides: Vec::new(),
            harness_overrides: ConfigOverrides::default(),
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search,
            transcript_cells: Vec::new(),
            overlay: None,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: adam_feedback::CodexFeedback::new(),
            pending_update_action: None,
            suppress_shutdown_complete: false,
            windows_sandbox: WindowsSandboxState::default(),
            shift_mouse_bypass_active: false,
            shift_mouse_bypass_restore_at: None,
            thread_event_channels: HashMap::new(),
            agent_picker_threads: HashMap::new(),
            active_thread_id: None,
            active_thread_rx: None,
            thread_created_rx: server.subscribe_thread_created(),
            listen_for_threads: true,
            primary_thread_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            review_parent_by_child: HashMap::new(),
            non_git_changelog_baselines: HashMap::new(),
            provider_config_modal: None,
            project_trust_modal: None,
            identity_modal: None,
            model_selection_modal: None,
            experimental_features_modal: None,
            mcp_tools_modal: None,
            pending_mcp_tools_modal_responses: 0,
            skills_modal: None,
            pending_skills_modal_open: false,
            approval_mode_modal: None,
            agent_selection_modal: None,
            review_modal: None,
            pending_startup_trust_prompt: false,
            deferred_initial_user_message: None,
            deferred_startup_continuation: None,
        }
    }

    async fn make_test_app_with_channels() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        tokio::sync::mpsc::UnboundedReceiver<Op>,
    ) {
        let (chat_widget, app_event_tx, rx, op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let server = Arc::new(ThreadManager::with_models_provider(
            CodexAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ));
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let otel_manager = test_otel_manager(&config, model.as_str());

        (
            App {
                server: server.clone(),
                otel_manager,
                app_event_tx,
                chat_widget,
                auth_manager,
                config,
                active_profile: None,
                cli_kv_overrides: Vec::new(),
                harness_overrides: ConfigOverrides::default(),
                runtime_approval_policy_override: None,
                runtime_sandbox_policy_override: None,
                file_search,
                transcript_cells: Vec::new(),
                overlay: None,
                enhanced_keys_supported: false,
                commit_anim_running: Arc::new(AtomicBool::new(false)),
                backtrack: BacktrackState::default(),
                backtrack_render_pending: false,
                feedback: adam_feedback::CodexFeedback::new(),
                pending_update_action: None,
                suppress_shutdown_complete: false,
                windows_sandbox: WindowsSandboxState::default(),
                shift_mouse_bypass_active: false,
                shift_mouse_bypass_restore_at: None,
                thread_event_channels: HashMap::new(),
                agent_picker_threads: HashMap::new(),
                active_thread_id: None,
                active_thread_rx: None,
                thread_created_rx: server.subscribe_thread_created(),
                listen_for_threads: true,
                primary_thread_id: None,
                primary_session_configured: None,
                pending_primary_events: VecDeque::new(),
                review_parent_by_child: HashMap::new(),
                non_git_changelog_baselines: HashMap::new(),
                provider_config_modal: None,
                project_trust_modal: None,
                identity_modal: None,
                model_selection_modal: None,
                experimental_features_modal: None,
                mcp_tools_modal: None,
                pending_mcp_tools_modal_responses: 0,
                skills_modal: None,
                pending_skills_modal_open: false,
                approval_mode_modal: None,
                agent_selection_modal: None,
                review_modal: None,
                pending_startup_trust_prompt: false,
                deferred_initial_user_message: None,
                deferred_startup_continuation: None,
            },
            rx,
            op_rx,
        )
    }

    fn test_session_configured_event() -> SessionConfiguredEvent {
        SessionConfiguredEvent {
            session_id: ThreadId::new(),
            forked_from_id: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            identity_kind: IdentityKind::Nobody,
            model_provider_id: "test-provider".to_string(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::ReadOnly,
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            rollout_path: Some(PathBuf::new()),
        }
    }

    #[tokio::test]
    async fn project_trust_rebuilds_full_config_for_deferred_startup() -> std::io::Result<()> {
        let adam_home = tempdir()?;
        let workspace = tempdir()?;
        let workspace_key = workspace.path().to_string_lossy().replace('\\', "\\\\");
        std::fs::write(
            adam_home.path().join(CONFIG_TOML_FILE),
            format!(
                r#"
profile = "saved"

[profiles.saved]
approval_policy = "on-request"

[profiles.project]
approval_policy = "never"

[projects."{workspace_key}"]
trust_level = "untrusted"
"#,
            ),
        )?;
        let project_config_dir = workspace.path().join(".adam");
        std::fs::create_dir_all(&project_config_dir)?;
        std::fs::write(
            project_config_dir.join(CONFIG_TOML_FILE),
            r#"
profile = "project"
developer_instructions = "project-only developer instructions"
show_raw_agent_reasoning = true
"#,
        )?;

        let initial_config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .provider_config_required(false)
            .harness_overrides(ConfigOverrides {
                cwd: Some(workspace.path().to_path_buf()),
                ..Default::default()
            })
            .build()
            .await?;
        assert_eq!(
            initial_config.active_project.trust_level,
            Some(TrustLevel::Untrusted)
        );
        assert_eq!(initial_config.active_profile.as_deref(), Some("saved"));
        assert_eq!(initial_config.developer_instructions, None);
        assert!(!initial_config.show_raw_agent_reasoning);

        let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let server = Arc::new(ThreadManager::with_models_provider_and_home(
            CodexAuth::from_api_key("Test API Key"),
            initial_config.model_provider_id.as_str(),
            initial_config.model_provider.clone(),
            adam_home.path().to_path_buf(),
        ));
        let mut app = App {
            server: server.clone(),
            otel_manager: test_otel_manager(&initial_config, "gpt-5.3-codex"),
            app_event_tx: app_event_tx.clone(),
            chat_widget,
            auth_manager,
            config: initial_config,
            active_profile: Some("saved".to_string()),
            cli_kv_overrides: Vec::new(),
            harness_overrides: ConfigOverrides {
                cwd: Some(workspace.path().to_path_buf()),
                ..Default::default()
            },
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search: FileSearchManager::new(
                workspace.path().to_path_buf(),
                app_event_tx.clone(),
            ),
            transcript_cells: Vec::new(),
            overlay: None,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: adam_feedback::CodexFeedback::new(),
            pending_update_action: None,
            suppress_shutdown_complete: false,
            windows_sandbox: WindowsSandboxState::default(),
            shift_mouse_bypass_active: false,
            shift_mouse_bypass_restore_at: None,
            thread_event_channels: HashMap::new(),
            agent_picker_threads: HashMap::new(),
            active_thread_id: None,
            active_thread_rx: None,
            thread_created_rx: server.subscribe_thread_created(),
            listen_for_threads: true,
            primary_thread_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            review_parent_by_child: HashMap::new(),
            non_git_changelog_baselines: HashMap::new(),
            provider_config_modal: None,
            project_trust_modal: None,
            identity_modal: None,
            model_selection_modal: None,
            experimental_features_modal: None,
            mcp_tools_modal: None,
            pending_mcp_tools_modal_responses: 0,
            skills_modal: None,
            pending_skills_modal_open: false,
            approval_mode_modal: None,
            agent_selection_modal: None,
            review_modal: None,
            pending_startup_trust_prompt: true,
            deferred_initial_user_message: None,
            deferred_startup_continuation: Some(DeferredStartupContinuation::StartFresh),
        };

        set_project_trust_level(adam_home.path(), workspace.path(), TrustLevel::Trusted)
            .expect("set project trusted");
        let rebuilt = app
            .rebuild_config_for_cwd(workspace.path().to_path_buf())
            .await
            .expect("rebuild config");
        assert_eq!(rebuilt.active_profile.as_deref(), Some("project"));
        assert_eq!(rebuilt.approval_policy.value(), AskForApproval::Never);
        assert_eq!(
            rebuilt.developer_instructions.as_deref(),
            Some("project-only developer instructions")
        );
        assert!(rebuilt.show_raw_agent_reasoning);

        let thread_manager = Arc::new(ThreadManager::with_models_provider_and_home(
            CodexAuth::from_api_key("Test API Key"),
            rebuilt.model_provider_id.as_str(),
            rebuilt.model_provider.clone(),
            adam_home.path().to_path_buf(),
        ));
        app.replace_chat_after_project_trust(
            rebuilt,
            thread_manager,
            Some("gpt-5.3-codex".to_string()),
            tui::FrameRequester::test_dummy(),
            DeferredStartupContinuation::StartFresh,
        )
        .await
        .expect("replace chat");

        assert_eq!(app.config.approval_policy.value(), AskForApproval::Never);
        assert_eq!(app.config.active_profile.as_deref(), Some("project"));
        assert_eq!(app.active_profile.as_deref(), Some("project"));
        assert_eq!(
            app.chat_widget.config_ref().active_profile.as_deref(),
            Some("project")
        );
        assert_eq!(
            app.chat_widget.config_ref().approval_policy.value(),
            AskForApproval::Never
        );
        assert_eq!(
            app.config.developer_instructions.as_deref(),
            Some("project-only developer instructions")
        );
        assert_eq!(
            app.chat_widget
                .config_ref()
                .developer_instructions
                .as_deref(),
            Some("project-only developer instructions")
        );
        assert!(app.config.show_raw_agent_reasoning);
        assert!(app.chat_widget.config_ref().show_raw_agent_reasoning);
        assert!(!app.pending_startup_trust_prompt);
        Ok(())
    }

    #[tokio::test]
    async fn project_trust_deferred_prompt_waits_for_session_configured() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let config = app.config.clone();
        let server = app.server.clone();
        app.pending_startup_trust_prompt = true;
        app.deferred_initial_user_message = Some(UserMessage::from("hello after trust"));
        app.deferred_startup_continuation = Some(DeferredStartupContinuation::StartFresh);

        app.replace_chat_after_project_trust(
            config,
            server,
            Some("gpt-test".to_string()),
            tui::FrameRequester::test_dummy(),
            DeferredStartupContinuation::StartFresh,
        )
        .await
        .expect("replace chat");

        assert_eq!(app.deferred_initial_user_message.is_none(), true);
        assert_eq!(app.deferred_startup_continuation.is_none(), true);
        assert_eq!(app.chat_widget.has_initial_user_message(), true);

        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(test_session_configured_event()),
        });

        assert_eq!(app.chat_widget.has_initial_user_message(), false);
    }

    #[tokio::test]
    async fn project_trust_exit_before_startup_uses_immediate_exit() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;

        app.project_trust_modal = Some(ProjectTrustModal::new(app.config.cwd.clone()));
        app.pending_startup_trust_prompt = true;

        assert_eq!(app.project_trust_exit_mode(), ExitMode::Immediate);
    }

    #[tokio::test]
    async fn provider_startup_modal_precedes_trust_modal() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let saved = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Responses,
            base_url: "https://example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: None,
        };
        persist_provider_fixture(&app.config.adam_home, &saved);
        app.provider_config_modal = Some(ProviderConfigModalState {
            mode: ProviderConfigModalMode::Startup,
            modal: ProviderConfigModal::new(
                app.config.adam_home.clone(),
                app.app_event_tx.clone(),
                tui::FrameRequester::test_dummy(),
            ),
        });
        app.project_trust_modal = None;
        app.pending_startup_trust_prompt = true;
        app.deferred_startup_continuation = Some(DeferredStartupContinuation::StartFresh);

        let control = app.handle_custom_provider_configured(None, saved).await;

        assert!(matches!(control, AppRunControl::Continue));
        assert!(app.provider_config_modal.is_none());
        assert!(app.project_trust_modal.is_some());
        assert!(app.pending_startup_trust_prompt);
        assert!(app.deferred_startup_continuation.is_some());
    }

    fn persist_provider_fixture(adam_home: &Path, config: &CustomProviderConfig) {
        persist_custom_provider_files(adam_home, config).expect("persist provider fixture");
    }

    fn persist_selected_model_fixture(adam_home: &Path, provider_id: &str, model: &str) {
        persist_selected_model_with_effort_fixture(adam_home, provider_id, model, None);
    }

    fn persist_selected_model_with_effort_fixture(
        adam_home: &Path,
        provider_id: &str,
        model: &str,
        effort: Option<ReasoningEffortConfig>,
    ) {
        let model_ref = if provider_id.contains('.') {
            ModelRef::parse(&format!("{provider_id}:{model}")).expect("model ref")
        } else {
            ModelRef::new(provider_id, "main", model)
        };
        AdamStateStore::new(adam_home)
            .set_last_selected_model(&model_ref, effort, None)
            .expect("persist state fixture");
    }

    fn persist_main_provider_fixture(
        adam_home: &Path,
        provider_id: &str,
        model: &str,
        model_context_window: Option<i64>,
    ) {
        let mut models_json = ModelsJson::load_from_adam_home(adam_home).expect("load models");
        let provider = models_json
            .providers
            .entry(provider_id.to_string())
            .or_default();
        provider.name = Some(provider_id.to_string());
        let endpoint = provider
            .endpoints
            .entry("main".to_string())
            .or_insert_with(|| ModelsEndpoint {
                name: Some(provider_id.to_string()),
                base_url: Some(format!("https://example.com/{provider_id}")),
                env_key: None,
                env_key_instructions: None,
                experimental_bearer_token: Some(format!("sk-{provider_id}")),
                dialect: ModelsDialect::Chat,
                query_params: None,
                http_headers: None,
                env_http_headers: None,
                request_max_retries: None,
                stream_max_retries: None,
                stream_idle_timeout_ms: None,
                supports_realtime_streaming: false,
                models: Default::default(),
            });
        endpoint
            .models
            .entry(model.to_string())
            .or_default()
            .context_window = model_context_window;
        models_json
            .save_to_adam_home(adam_home)
            .expect("save models");
    }

    fn write_profile_config(adam_home: &Path, contents: &str) {
        std::fs::write(adam_home.join("config.toml"), contents).expect("write config");
    }

    fn persist_provider_mapping_fixture(adam_home: &Path) {
        persist_main_provider_fixture(adam_home, "provider_a", "model-a", None);
        persist_main_provider_fixture(adam_home, "provider_b", "model-b", Some(64_000));
        persist_selected_model_fixture(adam_home, "provider_a", "model-a");
    }

    fn test_otel_manager(config: &Config, model: &str) -> OtelManager {
        let model_info = ModelsManager::construct_model_info_offline(model, config);
        OtelManager::new(
            ThreadId::new(),
            model,
            model_info.slug.as_str(),
            None,
            None,
            None,
            false,
            "test".to_string(),
            SessionSource::Cli,
        )
    }

    fn all_model_presets() -> Vec<ModelPreset> {
        adam_agent::models_manager::model_presets::all_model_presets().clone()
    }

    #[tokio::test]
    async fn open_model_selection_modal_sets_centered_modal() {
        let mut app = make_test_app().await;
        app.open_model_selection_modal(all_model_presets());

        assert!(
            app.model_selection_modal.is_some(),
            "expected App to own the /model modal"
        );
        assert!(app.chat_widget.no_modal_or_popup_active());
    }

    #[tokio::test]
    async fn open_experimental_features_modal_sets_centered_modal() {
        let mut app = make_test_app().await;
        app.open_experimental_features_modal();

        assert!(
            app.experimental_features_modal.is_some(),
            "expected App to own the /experimental modal"
        );
        assert!(app.chat_widget.no_modal_or_popup_active());
    }

    fn test_mcp_server() -> McpServerConfig {
        McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "docs-server".to_string(),
                args: Vec::new(),
                env: None,
                env_vars: Vec::new(),
                cwd: None,
            },
            enabled: true,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
        }
    }

    fn add_test_mcp_server(app: &mut App) {
        let mut servers = app.config.mcp_servers.get().clone();
        servers.insert("docs".to_string(), test_mcp_server());
        app.config.mcp_servers.set(servers).expect("mcp servers");
    }

    fn mcp_tools_event() -> Event {
        let mut tools = HashMap::new();
        tools.insert(
            "mcp__docs__list".to_string(),
            Tool {
                annotations: None,
                description: None,
                input_schema: ToolInputSchema {
                    properties: None,
                    required: None,
                    r#type: "object".to_string(),
                },
                name: "list".to_string(),
                output_schema: None,
                title: None,
            },
        );

        Event {
            id: String::new(),
            msg: EventMsg::McpListToolsResponse(McpListToolsResponseEvent {
                tools,
                resources: HashMap::new(),
                resource_templates: HashMap::new(),
                auth_statuses: HashMap::new(),
            }),
        }
    }

    #[tokio::test]
    async fn open_mcp_tools_modal_sets_centered_modal_for_empty_config() {
        let (mut app, _rx, mut op_rx) = make_test_app_with_channels().await;
        app.open_mcp_tools_modal();

        assert!(
            app.mcp_tools_modal.is_some(),
            "expected App to own the /mcp modal"
        );
        assert_eq!(app.pending_mcp_tools_modal_responses, 0);
        assert!(op_rx.try_recv().is_err());
        assert!(app.chat_widget.no_modal_or_popup_active());
    }

    #[tokio::test]
    async fn open_mcp_tools_modal_submits_list_op_for_configured_servers() {
        let (mut app, _rx, mut op_rx) = make_test_app_with_channels().await;
        add_test_mcp_server(&mut app);

        app.open_mcp_tools_modal();

        assert!(
            app.mcp_tools_modal.is_some(),
            "expected App to own the /mcp modal"
        );
        assert_eq!(app.pending_mcp_tools_modal_responses, 1);
        assert!(matches!(op_rx.try_recv(), Ok(Op::ListMcpTools)));
        assert!(app.chat_widget.no_modal_or_popup_active());
    }

    #[tokio::test]
    async fn mcp_list_tools_response_updates_open_modal_without_history() {
        let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
        add_test_mcp_server(&mut app);
        app.open_mcp_tools_modal();

        app.handle_codex_event_now(mcp_tools_event());

        assert_eq!(app.pending_mcp_tools_modal_responses, 0);
        assert!(
            app.mcp_tools_modal.is_some(),
            "expected response to keep modal open"
        );
        assert!(
            rx.try_recv().is_err(),
            "expected modal response to avoid writing history"
        );
    }

    #[tokio::test]
    async fn mcp_list_tools_response_after_modal_closed_is_swallowed() {
        let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
        add_test_mcp_server(&mut app);
        app.open_mcp_tools_modal();
        app.mcp_tools_modal = None;

        app.handle_codex_event_now(mcp_tools_event());

        assert_eq!(app.pending_mcp_tools_modal_responses, 0);
        assert!(
            rx.try_recv().is_err(),
            "expected closed modal response to avoid writing history"
        );
    }

    #[tokio::test]
    async fn mcp_list_tools_response_without_pending_modal_uses_history_fallback() {
        let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
        add_test_mcp_server(&mut app);

        app.handle_codex_event_now(mcp_tools_event());

        assert!(matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(_))));
    }

    fn test_skill(name: &str) -> adam_agent::protocol::SkillMetadata {
        adam_agent::protocol::SkillMetadata {
            name: name.to_string(),
            description: format!("Description for {name}"),
            short_description: None,
            interface: None,
            dependencies: None,
            path: PathBuf::from(format!("/tmp/skills/{name}.toml")),
            scope: adam_agent::protocol::SkillScope::User,
            enabled: true,
        }
    }

    fn list_skills_event(cwd: PathBuf, skills: Vec<adam_agent::protocol::SkillMetadata>) -> Event {
        Event {
            id: String::new(),
            msg: EventMsg::ListSkillsResponse(ListSkillsResponseEvent {
                skills: vec![adam_agent::protocol::SkillsListEntry {
                    cwd,
                    skills,
                    errors: Vec::new(),
                }],
            }),
        }
    }

    #[tokio::test]
    async fn open_skills_modal_sets_centered_modal() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_skills_from_response(&ListSkillsResponseEvent {
                skills: vec![adam_agent::protocol::SkillsListEntry {
                    cwd: app.config.cwd.clone(),
                    skills: vec![adam_agent::protocol::SkillMetadata {
                        name: "repo_scout".to_string(),
                        description: "Summarize the repo layout".to_string(),
                        short_description: None,
                        interface: None,
                        dependencies: None,
                        path: PathBuf::from("/tmp/skills/repo_scout.toml"),
                        scope: adam_agent::protocol::SkillScope::User,
                        enabled: true,
                    }],
                    errors: Vec::new(),
                }],
            });

        app.open_skills_modal();

        assert!(
            app.skills_modal.is_some(),
            "expected App to own the /skills modal"
        );
        assert!(app.chat_widget.no_modal_or_popup_active());
    }

    #[tokio::test]
    async fn open_skills_modal_before_skills_response_requests_loading_refresh() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        app.open_skills_modal();

        assert!(
            app.skills_modal.is_none(),
            "expected /skills to wait for the first skills response"
        );
        match op_rx.try_recv() {
            Ok(Op::ListSkills { cwds, force_reload }) => {
                assert!(cwds.is_empty());
                assert!(force_reload);
            }
            other => panic!("expected skills refresh op, got {other:?}"),
        }
        assert!(app.pending_skills_modal_open);
    }

    #[tokio::test]
    async fn open_skills_modal_while_initial_request_in_flight_opens_after_response() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        app.chat_widget.request_skills_refresh(true);
        match op_rx.try_recv() {
            Ok(Op::ListSkills { cwds, force_reload }) => {
                assert!(cwds.is_empty());
                assert!(force_reload);
            }
            other => panic!("expected initial skills refresh op, got {other:?}"),
        }

        app.open_skills_modal();

        assert!(matches!(op_rx.try_recv(), Err(TryRecvError::Empty)));
        assert!(app.pending_skills_modal_open);

        app.handle_codex_event_now(Event {
            id: String::new(),
            msg: EventMsg::ListSkillsResponse(ListSkillsResponseEvent {
                skills: vec![adam_agent::protocol::SkillsListEntry {
                    cwd: app.config.cwd.clone(),
                    skills: vec![adam_agent::protocol::SkillMetadata {
                        name: "repo_scout".to_string(),
                        description: "Summarize the repo layout".to_string(),
                        short_description: None,
                        interface: None,
                        dependencies: None,
                        path: PathBuf::from("/tmp/skills/repo_scout.toml"),
                        scope: adam_agent::protocol::SkillScope::User,
                        enabled: true,
                    }],
                    errors: Vec::new(),
                }],
            }),
        });

        assert!(
            app.skills_modal.is_some(),
            "expected pending /skills intent to open the modal"
        );
        assert!(!app.pending_skills_modal_open);
        assert!(matches!(op_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn pending_skills_modal_does_not_steal_approval_modal() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        app.chat_widget.request_skills_refresh(true);
        assert!(matches!(op_rx.try_recv(), Ok(Op::ListSkills { .. })));
        app.open_skills_modal();
        assert!(app.pending_skills_modal_open);

        app.open_approvals_modal();
        app.handle_codex_event_now(list_skills_event(
            app.config.cwd.clone(),
            vec![test_skill("repo_scout")],
        ));

        assert!(
            app.approval_mode_modal.is_some(),
            "expected approvals modal to keep focus"
        );
        assert!(
            app.skills_modal.is_none(),
            "expected pending /skills not to steal focus"
        );
        assert!(app.pending_skills_modal_open);

        app.approval_mode_modal = None;

        assert!(app.maybe_open_pending_skills_modal());
        assert!(
            app.skills_modal.is_some(),
            "expected pending /skills to open after approvals closes"
        );
        assert!(!app.pending_skills_modal_open);
    }

    #[tokio::test]
    async fn pending_skills_modal_does_not_steal_review_modal() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        app.chat_widget.request_skills_refresh(true);
        assert!(matches!(op_rx.try_recv(), Ok(Op::ListSkills { .. })));
        app.open_skills_modal();
        assert!(app.pending_skills_modal_open);

        app.open_review_modal();
        app.handle_codex_event_now(list_skills_event(
            app.config.cwd.clone(),
            vec![test_skill("repo_scout")],
        ));

        assert!(
            app.review_modal.is_some(),
            "expected review modal to keep focus"
        );
        assert!(
            app.skills_modal.is_none(),
            "expected pending /skills not to steal focus"
        );
        assert!(app.pending_skills_modal_open);

        app.review_modal = None;

        assert!(app.maybe_open_pending_skills_modal());
        assert!(
            app.skills_modal.is_some(),
            "expected pending /skills to open after review closes"
        );
        assert!(!app.pending_skills_modal_open);
    }

    #[tokio::test]
    async fn pending_skills_modal_does_not_steal_overlay() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        app.chat_widget.request_skills_refresh(true);
        assert!(matches!(op_rx.try_recv(), Ok(Op::ListSkills { .. })));
        app.open_skills_modal();
        assert!(app.pending_skills_modal_open);

        app.overlay = Some(Overlay::new_static_with_lines(
            vec![Line::from("Pending approval")],
            "T E S T".to_string(),
        ));
        app.handle_codex_event_now(list_skills_event(
            app.config.cwd.clone(),
            vec![test_skill("repo_scout")],
        ));

        assert!(app.overlay.is_some(), "expected overlay to stay visible");
        assert!(
            app.skills_modal.is_none(),
            "expected pending /skills not to steal focus"
        );
        assert!(app.pending_skills_modal_open);

        app.overlay = None;

        assert!(app.maybe_open_pending_skills_modal());
        assert!(
            app.skills_modal.is_some(),
            "expected pending /skills to open after overlay closes"
        );
        assert!(!app.pending_skills_modal_open);
    }

    #[tokio::test]
    async fn pending_skills_modal_waits_for_queued_refresh() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        app.chat_widget.request_skills_refresh(true);
        assert!(matches!(op_rx.try_recv(), Ok(Op::ListSkills { .. })));
        app.open_skills_modal();
        assert!(app.pending_skills_modal_open);

        app.chat_widget.request_skills_refresh(true);
        assert!(matches!(op_rx.try_recv(), Err(TryRecvError::Empty)));

        app.handle_codex_event_now(list_skills_event(
            app.config.cwd.clone(),
            vec![test_skill("old_skill")],
        ));

        assert!(
            app.skills_modal.is_none(),
            "expected stale response not to open skills modal"
        );
        assert!(app.pending_skills_modal_open);
        assert!(app.chat_widget.skills_request_in_flight());
        match op_rx.try_recv() {
            Ok(Op::ListSkills { cwds, force_reload }) => {
                assert!(cwds.is_empty());
                assert!(force_reload);
            }
            other => panic!("expected queued skills refresh op, got {other:?}"),
        }

        app.handle_codex_event_now(list_skills_event(
            app.config.cwd.clone(),
            vec![test_skill("fresh_skill")],
        ));

        assert!(
            app.skills_modal.is_some(),
            "expected fresh response to open skills modal"
        );
        assert!(!app.pending_skills_modal_open);
        assert!(!app.chat_widget.skills_request_in_flight());
    }

    #[tokio::test]
    async fn open_skills_modal_after_empty_response_does_not_refresh_again() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        app.chat_widget
            .set_skills_from_response(&ListSkillsResponseEvent {
                skills: vec![adam_agent::protocol::SkillsListEntry {
                    cwd: app.config.cwd.clone(),
                    skills: Vec::new(),
                    errors: Vec::new(),
                }],
            });

        app.open_skills_modal();

        assert!(
            app.skills_modal.is_none(),
            "expected empty skills response to avoid opening the modal"
        );
        assert!(matches!(op_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn pending_skills_modal_open_clears_after_empty_response() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        app.chat_widget.request_skills_refresh(true);
        match op_rx.try_recv() {
            Ok(Op::ListSkills { cwds, force_reload }) => {
                assert!(cwds.is_empty());
                assert!(force_reload);
            }
            other => panic!("expected initial skills refresh op, got {other:?}"),
        }
        app.open_skills_modal();

        app.handle_codex_event_now(Event {
            id: String::new(),
            msg: EventMsg::ListSkillsResponse(ListSkillsResponseEvent {
                skills: vec![adam_agent::protocol::SkillsListEntry {
                    cwd: app.config.cwd.clone(),
                    skills: Vec::new(),
                    errors: Vec::new(),
                }],
            }),
        });

        assert!(
            app.skills_modal.is_none(),
            "expected empty skills response to avoid opening the modal"
        );
        assert!(!app.pending_skills_modal_open);
        assert!(matches!(op_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn set_skill_enabled_failure_leaves_cached_skill_unchanged() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_skills_from_response(&ListSkillsResponseEvent {
                skills: vec![adam_agent::protocol::SkillsListEntry {
                    cwd: app.config.cwd.clone(),
                    skills: vec![test_skill("repo_scout")],
                    errors: Vec::new(),
                }],
            });
        let adam_home = tempdir().expect("temp adam home");
        let not_a_dir = adam_home.path().join("not-a-dir");
        std::fs::write(&not_a_dir, "not a directory").expect("write file");
        app.config.adam_home = not_a_dir;

        let result = app
            .set_skill_enabled(PathBuf::from("/tmp/skills/repo_scout.toml"), false)
            .await;

        assert!(result.is_err());
        let items = match app.chat_widget.skills_modal_items() {
            SkillsModalItems::Ready(items) => items,
            other => panic!("expected ready skills, got {other:?}"),
        };
        assert_eq!(
            items,
            vec![crate::skills_modal::SkillsModalItem {
                name: "repo_scout".to_string(),
                skill_name: "repo_scout".to_string(),
                description: "Description for repo_scout".to_string(),
                enabled: true,
                path: PathBuf::from("/tmp/skills/repo_scout.toml"),
            }]
        );
    }

    #[tokio::test]
    async fn open_approvals_modal_sets_centered_modal() {
        let mut app = make_test_app().await;
        app.open_approvals_modal();

        assert!(
            app.approval_mode_modal.is_some(),
            "expected App to own the /approvals modal"
        );
        assert!(app.chat_widget.no_modal_or_popup_active());
    }

    #[tokio::test]
    async fn open_permissions_modal_sets_centered_modal() {
        let mut app = make_test_app().await;
        app.open_permissions_modal();

        assert!(
            app.approval_mode_modal.is_some(),
            "expected App to own the /permissions modal"
        );
        assert!(app.chat_widget.no_modal_or_popup_active());
    }

    fn model_migration_copy_to_plain_text(
        copy: &crate::model_migration::ModelMigrationCopy,
    ) -> String {
        if let Some(markdown) = copy.markdown.as_ref() {
            return markdown.clone();
        }
        let mut s = String::new();
        for span in &copy.heading {
            s.push_str(&span.content);
        }
        s.push('\n');
        s.push('\n');
        for line in &copy.content {
            for span in &line.spans {
                s.push_str(&span.content);
            }
            s.push('\n');
        }
        s
    }

    #[tokio::test]
    async fn model_migration_prompt_only_shows_for_deprecated_models() {
        let seen = BTreeMap::new();
        assert!(should_show_model_migration_prompt(
            "gpt-5",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5-codex",
            "gpt-5.1-codex",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5-codex-mini",
            "gpt-5.1-codex-mini",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5.1-codex",
            "gpt-5.1-codex-max",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.1-codex",
            "gpt-5.1-codex",
            &seen,
            &all_model_presets()
        ));
    }

    #[tokio::test]
    async fn model_migration_prompt_respects_hide_flag_and_self_target() {
        let mut seen = BTreeMap::new();
        seen.insert("gpt-5".to_string(), "gpt-5.1".to_string());
        assert!(!should_show_model_migration_prompt(
            "gpt-5",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.1",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
    }

    #[tokio::test]
    async fn model_migration_prompt_skips_when_target_missing() {
        let mut available = all_model_presets();
        let mut current = available
            .iter()
            .find(|preset| preset.model == "gpt-5-codex")
            .cloned()
            .expect("preset present");
        current.upgrade = Some(ModelUpgrade {
            id: "missing-target".to_string(),
            reasoning_effort_mapping: None,
            migration_config_key: HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG.to_string(),
            model_link: None,
            upgrade_copy: None,
            migration_markdown: None,
        });
        available.retain(|preset| preset.model != "gpt-5-codex");
        available.push(current.clone());

        assert!(should_show_model_migration_prompt(
            &current.model,
            "missing-target",
            &BTreeMap::new(),
            &available,
        ));

        assert!(target_preset_for_upgrade(&available, "missing-target").is_none());
    }

    #[tokio::test]
    async fn model_migration_prompt_shows_for_hidden_model() {
        let adam_home = tempdir().expect("temp codex home");
        let config = ConfigBuilder::default()
            .adam_home(adam_home.path().to_path_buf())
            .build()
            .await
            .expect("config");

        let available_models = all_model_presets();
        let current = available_models
            .iter()
            .find(|preset| preset.model == "gpt-5.1-codex")
            .cloned()
            .expect("gpt-5.1-codex preset present");
        assert!(
            !current.show_in_picker,
            "expected gpt-5.1-codex to be hidden from picker for this test"
        );

        let upgrade = current.upgrade.as_ref().expect("upgrade configured");
        assert!(
            should_show_model_migration_prompt(
                &current.model,
                &upgrade.id,
                &config.notices.model_migrations,
                &available_models,
            ),
            "expected migration prompt to be eligible for hidden model"
        );

        let target = target_preset_for_upgrade(&available_models, &upgrade.id)
            .expect("upgrade target present");
        let target_description =
            (!target.description.is_empty()).then(|| target.description.clone());
        let can_opt_out = true;
        let copy = migration_copy_for_models(
            &current.model,
            &upgrade.id,
            upgrade.model_link.clone(),
            upgrade.upgrade_copy.clone(),
            upgrade.migration_markdown.clone(),
            target.display_name.clone(),
            target_description,
            can_opt_out,
        );

        // Snapshot the copy we would show; rendering is covered by model_migration snapshots.
        assert_snapshot!(
            "model_migration_prompt_shows_for_hidden_model",
            model_migration_copy_to_plain_text(&copy)
        );
    }

    #[tokio::test]
    async fn update_reasoning_effort_updates_identity() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let user_cell = |text: &str,
                         text_elements: Vec<TextElement>,
                         local_image_paths: Vec<PathBuf>|
         -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
                text_elements,
                local_image_paths,
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                true,
            )) as Arc<dyn HistoryCell>
        };

        let make_header = |is_first| {
            let event = SessionConfiguredEvent {
                session_id: ThreadId::new(),
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                identity_kind: IdentityKind::Nobody,
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: Some(PathBuf::new()),
            };
            Arc::new(new_session_info(
                app.chat_widget.config_ref(),
                app.chat_widget.current_model(),
                event,
                is_first,
            )) as Arc<dyn HistoryCell>
        };

        let placeholder = "[Image #1]";
        let edited_text = format!("follow-up (edited) {placeholder}");
        let edited_range = edited_text.len().saturating_sub(placeholder.len())..edited_text.len();
        let edited_text_elements = vec![TextElement::new(edited_range.into(), None)];
        let edited_local_image_paths = vec![PathBuf::from("/tmp/fake-image.png")];

        // Simulate a transcript with duplicated history (e.g., from prior backtracks)
        // and an edited turn appended after a session header boundary.
        app.transcript_cells = vec![
            make_header(true),
            user_cell("first question", Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell("follow-up", Vec::new(), Vec::new()),
            agent_cell("answer follow-up"),
            make_header(false),
            user_cell("first question", Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell(
                &edited_text,
                edited_text_elements.clone(),
                edited_local_image_paths.clone(),
            ),
            agent_cell("answer edited"),
        ];

        assert_eq!(user_count(&app.transcript_cells), 2);

        let base_id = ThreadId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: base_id,
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                identity_kind: IdentityKind::Nobody,
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: Some(PathBuf::new()),
            }),
        });

        app.backtrack.base_id = Some(base_id);
        app.backtrack.primed = true;
        app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

        let selection = app
            .confirm_backtrack_from_main()
            .expect("backtrack selection");
        assert_eq!(selection.nth_user_message, 1);
        assert_eq!(selection.prefill, edited_text);
        assert_eq!(selection.text_elements, edited_text_elements);
        assert_eq!(selection.local_image_paths, edited_local_image_paths);

        app.apply_backtrack_rollback(selection);

        let mut rollback_turns = None;
        while let Ok(op) = op_rx.try_recv() {
            if let Op::ThreadRollback { num_turns } = op {
                rollback_turns = Some(num_turns);
            }
        }

        assert_eq!(rollback_turns, Some(1));
    }

    #[tokio::test]
    async fn new_session_requests_shutdown_for_previous_conversation() {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let thread_id = ThreadId::new();
        let event = SessionConfiguredEvent {
            session_id: thread_id,
            forked_from_id: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            identity_kind: IdentityKind::Nobody,
            model_provider_id: "test-provider".to_string(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::ReadOnly,
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            rollout_path: Some(PathBuf::new()),
        };

        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(event),
        });

        while app_event_rx.try_recv().is_ok() {}
        while op_rx.try_recv().is_ok() {}

        app.shutdown_current_thread().await;

        match op_rx.try_recv() {
            Ok(Op::Shutdown) => {}
            Ok(other) => panic!("expected Op::Shutdown, got {other:?}"),
            Err(_) => panic!("expected shutdown op to be sent"),
        }
    }

    #[tokio::test]
    async fn session_summary_skip_zero_usage() {
        assert!(session_summary(TokenUsage::default(), None, None).is_none());
    }

    #[tokio::test]
    async fn session_summary_includes_resume_hint() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(usage, Some(conversation), None).expect("summary");
        assert_eq!(
            summary.usage_line,
            "Token usage: total=12 input=10 output=2"
        );
        assert_eq!(
            summary.resume_command,
            Some("adam resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }

    #[tokio::test]
    async fn session_summary_prefers_name_over_id() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(usage, Some(conversation), Some("my-session".to_string()))
            .expect("summary");
        assert_eq!(
            summary.resume_command,
            Some("adam resume my-session".to_string())
        );
    }

    #[tokio::test]
    async fn custom_provider_save_refreshes_active_provider_from_reloaded_config() {
        let mut app = make_test_app().await;
        let existing = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Responses,
            base_url: "https://example.com/v1".to_string(),
            api_key: "sk-old".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: None,
        };
        let saved = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Chat,
            base_url: "https://example.com/chat".to_string(),
            api_key: "sk-new".to_string(),
            model: "gpt-other".to_string(),
            model_context_window: None,
        };
        persist_provider_fixture(&app.config.adam_home, &existing);
        persist_provider_fixture(&app.config.adam_home, &saved);

        app.config = ConfigBuilder::default()
            .adam_home(app.config.adam_home.clone())
            .build()
            .await
            .expect("reload config");
        app.chat_widget.sync_provider_config(&app.config, true);
        app.chat_widget.set_model("model-a");

        app.handle_custom_provider_configured(None, saved).await;

        assert_eq!(app.config.model_provider.env_key.as_deref(), None);
        assert_eq!(app.config.model_provider.query_params.as_ref(), None);
        assert_eq!(app.config.model_provider.http_headers.as_ref(), None);
        assert_eq!(
            app.chat_widget.config_ref().model_provider,
            app.config.model_provider
        );
        assert_eq!(app.config.model_provider_id, "custom_1.chat");
        assert_eq!(app.config.model.as_deref(), Some("gpt-other"));
        assert_eq!(app.chat_widget.current_model(), "gpt-other");
        assert_eq!(
            app.chat_widget.config_ref().model.as_deref(),
            Some("gpt-other")
        );
        assert_eq!(
            app.chat_widget.config_ref().model_provider_id,
            "custom_1.chat"
        );

        assert_eq!(
            AdamStateStore::new(&app.config.adam_home)
                .load()
                .expect("state")
                .last_selected_model
                .as_ref()
                .map(|model| model.model_ref.as_str()),
            Some("custom_1.chat:gpt-other")
        );
    }

    #[tokio::test]
    async fn custom_provider_save_switches_runtime_provider_for_new_provider() {
        let mut app = make_test_app().await;
        let saved = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Responses,
            base_url: "https://example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: None,
        };
        persist_provider_fixture(&app.config.adam_home, &saved);

        app.handle_custom_provider_configured(None, saved).await;

        assert_eq!(app.config.model_provider_id, "custom_1.responses");
        assert_eq!(app.config.model.as_deref(), Some("gpt-test"));
        assert_eq!(app.chat_widget.current_model(), "gpt-test");
        assert_eq!(
            app.chat_widget.config_ref().model_provider_id,
            "custom_1.responses"
        );
        assert_eq!(
            app.chat_widget.config_ref().model.as_deref(),
            Some("gpt-test")
        );

        assert_eq!(
            AdamStateStore::new(&app.config.adam_home)
                .load()
                .expect("state")
                .last_selected_model
                .as_ref()
                .map(|model| model.model_ref.as_str()),
            Some("custom_1.responses:gpt-test")
        );
    }

    #[tokio::test]
    async fn custom_provider_save_applies_context_window_to_runtime_and_profile() {
        let mut app = make_test_app().await;
        let saved = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Responses,
            base_url: "https://example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: Some(128_000),
        };
        persist_provider_fixture(&app.config.adam_home, &saved);

        app.handle_custom_provider_configured(None, saved).await;

        assert_eq!(app.config.model_provider_id, "custom_1.responses");
        assert_eq!(app.config.model.as_deref(), Some("gpt-test"));
        assert_eq!(app.config.model_context_window, Some(128_000));
        assert_eq!(app.chat_widget.current_model(), "gpt-test");
        assert_eq!(
            app.chat_widget.config_ref().model_context_window,
            Some(128_000)
        );
    }

    #[tokio::test]
    async fn custom_provider_save_preserves_existing_context_window_when_left_blank() {
        let mut app = make_test_app().await;
        let initial = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Responses,
            base_url: "https://example.com/v1".to_string(),
            api_key: "sk-old".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: Some(64_000),
        };
        let saved = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Chat,
            base_url: "https://example.com/chat".to_string(),
            api_key: "sk-new".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: None,
        };
        persist_provider_fixture(&app.config.adam_home, &initial);
        persist_provider_fixture(&app.config.adam_home, &saved);

        app.handle_custom_provider_configured(None, saved).await;

        assert_eq!(app.config.model_provider_id, "custom_1.chat");
        assert_eq!(app.config.model.as_deref(), Some("gpt-test"));
        assert_eq!(app.config.model_context_window, None);
        assert_eq!(app.chat_widget.config_ref().model_context_window, None);
    }

    #[tokio::test]
    async fn custom_provider_save_clears_stale_global_context_window_for_new_blank_model() {
        let mut app = make_test_app().await;
        let initial = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Responses,
            base_url: "https://example.com/v1".to_string(),
            api_key: "sk-old".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: Some(64_000),
        };
        let saved = CustomProviderConfig {
            provider_id: "custom_2".to_string(),
            dialect: ApiProviderDialect::Chat,
            base_url: "https://example.com/chat".to_string(),
            api_key: "sk-new".to_string(),
            model: "gpt-other".to_string(),
            model_context_window: None,
        };
        persist_provider_fixture(&app.config.adam_home, &initial);
        persist_provider_fixture(&app.config.adam_home, &saved);

        app.handle_custom_provider_configured(None, saved).await;

        assert_eq!(app.config.model_provider_id, "custom_2.chat");
        assert_eq!(app.config.model.as_deref(), Some("gpt-other"));
        assert_eq!(app.config.model_context_window, None);
        assert_eq!(app.chat_widget.config_ref().model_context_window, None);
    }

    #[tokio::test]
    async fn custom_provider_save_switches_active_thread_provider_and_model() {
        let mut app = make_test_app().await;
        let new_thread = app
            .server
            .start_thread(app.config.clone())
            .await
            .expect("start thread");
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(new_thread.session_configured.clone()),
        });

        let saved = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            dialect: ApiProviderDialect::Responses,
            base_url: "https://example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: None,
        };
        persist_provider_fixture(&app.config.adam_home, &saved);

        app.handle_custom_provider_configured(None, saved).await;

        let thread = app
            .server
            .get_thread(new_thread.thread_id)
            .await
            .expect("get thread");
        let snapshot = thread.config_snapshot().await;
        assert_eq!(snapshot.model_provider_id, "custom_1.responses");
        assert_eq!(snapshot.model, "gpt-test");
    }

    #[tokio::test]
    async fn persist_model_selection_switches_runtime_provider_from_models_json() {
        let mut app = make_test_app().await;
        persist_provider_mapping_fixture(&app.config.adam_home);
        write_profile_config(
            &app.config.adam_home,
            r#"profile = "saved"

[profiles.saved]
"#,
        );

        app.config = ConfigBuilder::default()
            .adam_home(app.config.adam_home.clone())
            .build()
            .await
            .expect("reload config");
        app.active_profile = Some("saved".to_string());
        app.config.active_profile = Some("saved".to_string());
        app.chat_widget.sync_provider_config(&app.config, true);
        app.chat_widget.set_model("model-a");

        let provider_id = app
            .persist_model_selection(
                "model-b".to_string(),
                None,
                Some(ReasoningEffortConfig::High),
            )
            .await
            .expect("persist model selection");

        assert_eq!(provider_id, "provider_b");
        assert_eq!(app.config.model_provider_id, "provider_b");
        assert_eq!(app.config.model.as_deref(), Some("model-b"));
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(app.config.model_context_window, Some(64_000));
        assert_eq!(app.config.model_auto_compact_token_limit, Some(60_800));
        assert_eq!(app.chat_widget.current_model(), "model-b");
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.chat_widget.config_ref().model_context_window,
            Some(64_000)
        );
        assert_eq!(
            app.chat_widget.config_ref().model_auto_compact_token_limit,
            Some(60_800)
        );

        let state = AdamStateStore::new(&app.config.adam_home)
            .load()
            .expect("state");
        assert_eq!(
            state
                .last_selected_model
                .as_ref()
                .map(|model| model.model_ref.as_str()),
            Some("provider_b.main:model-b")
        );
        assert_eq!(
            state.last_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn persist_model_selection_reloads_context_limits_after_state_save() {
        let mut app = make_test_app().await;
        persist_provider_mapping_fixture(&app.config.adam_home);
        write_profile_config(
            &app.config.adam_home,
            r#"profile = "saved"

[profiles.saved]
"#,
        );

        app.config = ConfigBuilder::default()
            .adam_home(app.config.adam_home.clone())
            .build()
            .await
            .expect("reload config");
        app.active_profile = Some("saved".to_string());
        app.config.active_profile = Some("saved".to_string());
        app.chat_widget.sync_provider_config(&app.config, true);
        app.chat_widget.set_model("model-a");

        let provider_id = app
            .persist_model_selection(
                "model-b".to_string(),
                None,
                Some(ReasoningEffortConfig::High),
            )
            .await
            .expect("persist model selection");

        assert_eq!(provider_id, "provider_b");
        assert_eq!(app.config.model_provider_id, "provider_b");
        assert_eq!(app.config.model.as_deref(), Some("model-b"));
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(app.config.model_context_window, Some(64_000));
        assert_eq!(app.config.model_auto_compact_token_limit, Some(60_800));
        assert_eq!(app.chat_widget.current_model(), "model-b");
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.chat_widget.config_ref().model_context_window,
            Some(64_000)
        );
        assert_eq!(
            app.chat_widget.config_ref().model_auto_compact_token_limit,
            Some(60_800)
        );

        let state = AdamStateStore::new(&app.config.adam_home)
            .load()
            .expect("state");
        assert_eq!(
            state
                .last_selected_model
                .as_ref()
                .map(|model| model.model_ref.as_str()),
            Some("provider_b.main:model-b")
        );
        assert_eq!(
            state.last_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn persist_model_selection_clears_reasoning_effort_when_none_passed() {
        let mut app = make_test_app().await;
        persist_provider_mapping_fixture(&app.config.adam_home);
        persist_selected_model_with_effort_fixture(
            &app.config.adam_home,
            "provider_a",
            "model-a",
            Some(ReasoningEffortConfig::High),
        );
        write_profile_config(
            &app.config.adam_home,
            r#"profile = "saved"

[profiles.saved]
"#,
        );

        app.config = ConfigBuilder::default()
            .adam_home(app.config.adam_home.clone())
            .build()
            .await
            .expect("reload config");
        app.active_profile = Some("saved".to_string());
        app.config.active_profile = Some("saved".to_string());
        app.chat_widget.sync_provider_config(&app.config, true);
        app.chat_widget.set_model("model-a");
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::High));

        let provider_id = app
            .persist_model_selection("model-b".to_string(), None, None)
            .await
            .expect("persist model selection");

        assert_eq!(provider_id, "provider_b");
        assert_eq!(app.config.model_provider_id, "provider_b");
        assert_eq!(app.config.model.as_deref(), Some("model-b"));
        assert_eq!(app.config.model_reasoning_effort, None);
        assert_eq!(app.chat_widget.current_model(), "model-b");
        assert_eq!(app.chat_widget.current_reasoning_effort(), None);

        let state = AdamStateStore::new(&app.config.adam_home)
            .load()
            .expect("state");
        assert_eq!(
            state
                .last_selected_model
                .as_ref()
                .map(|model| model.model_ref.as_str()),
            Some("provider_b.main:model-b")
        );
        assert_eq!(state.last_reasoning_effort, None);
    }

    #[tokio::test]
    async fn persist_model_selection_rejects_ambiguous_provider_mapping() {
        let mut app = make_test_app().await;
        persist_main_provider_fixture(&app.config.adam_home, "provider_a", "shared-model", None);
        persist_main_provider_fixture(&app.config.adam_home, "provider_b", "shared-model", None);
        persist_selected_model_fixture(&app.config.adam_home, "provider_a", "model-a");

        app.config = ConfigBuilder::default()
            .adam_home(app.config.adam_home.clone())
            .build()
            .await
            .expect("reload config");
        app.chat_widget.sync_provider_config(&app.config, true);
        app.chat_widget.set_model("model-a");

        let err = app
            .persist_model_selection("shared-model".to_string(), None, None)
            .await
            .expect_err("expected ambiguous model selection to fail");

        assert_eq!(
            err,
            "model `shared-model` is configured for multiple providers: provider_a, provider_b"
        );
        assert_eq!(app.config.model_provider_id, "provider_a");
        assert_eq!(app.config.model.as_deref(), Some("model-a"));
        assert_eq!(app.chat_widget.current_model(), "model-a");
    }

    #[tokio::test]
    async fn persist_model_selection_uses_explicit_provider_for_ambiguous_model() {
        let mut app = make_test_app().await;
        persist_main_provider_fixture(&app.config.adam_home, "provider_a", "shared-model", None);
        persist_main_provider_fixture(&app.config.adam_home, "provider_b", "shared-model", None);
        persist_selected_model_fixture(&app.config.adam_home, "provider_a", "model-a");

        app.config = ConfigBuilder::default()
            .adam_home(app.config.adam_home.clone())
            .build()
            .await
            .expect("reload config");
        app.chat_widget.sync_provider_config(&app.config, true);
        app.chat_widget.set_model("model-a");

        let provider_id = app
            .persist_model_selection(
                "shared-model".to_string(),
                Some("provider_b".to_string()),
                None,
            )
            .await
            .expect("persist model selection with explicit provider");

        assert_eq!(provider_id, "provider_b");
        assert_eq!(app.config.model_provider_id, "provider_b");
        assert_eq!(app.config.model.as_deref(), Some("shared-model"));
        assert_eq!(app.chat_widget.current_model(), "shared-model");
    }

    #[tokio::test]
    async fn persist_model_selection_uses_explicit_openai_provider_for_builtin_same_slug() {
        let mut app = make_test_app().await;
        persist_main_provider_fixture(&app.config.adam_home, "provider_a", "gpt-5.2", None);
        persist_selected_model_fixture(&app.config.adam_home, "provider_a", "gpt-5.2");

        app.config = ConfigBuilder::default()
            .adam_home(app.config.adam_home.clone())
            .build()
            .await
            .expect("reload config");
        app.chat_widget.sync_provider_config(&app.config, true);
        app.chat_widget.set_model("gpt-5.2");

        let provider_id = app
            .persist_model_selection("gpt-5.2".to_string(), Some("openai".to_string()), None)
            .await
            .expect("persist model selection with explicit openai provider");

        assert_eq!(provider_id, "openai");
        assert_eq!(app.config.model_provider_id, "openai");
        assert_eq!(app.config.model.as_deref(), Some("gpt-5.2"));
        assert_eq!(app.chat_widget.current_model(), "gpt-5.2");
    }

    #[tokio::test]
    async fn persist_model_selection_switches_runtime_when_save_fails() {
        let mut app = make_test_app().await;
        let config_path = app.config.adam_home.join("config.toml");
        persist_provider_mapping_fixture(&app.config.adam_home);
        write_profile_config(
            &app.config.adam_home,
            r#"profile = "saved"

[profiles.saved]
"#,
        );

        app.config = ConfigBuilder::default()
            .adam_home(app.config.adam_home.clone())
            .build()
            .await
            .expect("reload config");
        app.active_profile = Some("saved".to_string());
        app.config.active_profile = Some("saved".to_string());
        app.chat_widget.sync_provider_config(&app.config, true);
        app.chat_widget.set_model("model-a");
        let provider_id = "provider_b".to_string();
        app.config.adam_home = config_path;

        let err = app
            .persist_model_selection(
                "model-b".to_string(),
                Some(provider_id),
                Some(ReasoningEffortConfig::High),
            )
            .await
            .expect_err("expected model persistence to fail");

        assert!(
            err.contains(
                "Switched the current session to model `model-b` using provider `provider_b`."
            ),
            "unexpected error: {err}"
        );
        assert_eq!(app.config.model_provider_id, "provider_b");
        assert_eq!(app.config.model.as_deref(), Some("model-b"));
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(app.config.model_context_window, None);
        assert_eq!(app.config.model_auto_compact_token_limit, None);
        assert_eq!(app.chat_widget.current_model(), "model-b");
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(app.chat_widget.config_ref().model_context_window, None);
        assert_eq!(
            app.chat_widget.config_ref().model_auto_compact_token_limit,
            None
        );
    }
}
