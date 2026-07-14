//! The main LHA TUI chat surface.
//!
//! `ChatWidget` consumes protocol events, builds and updates history cells, and drives rendering
//! for both the main viewport and overlay UIs.
//!
//! The UI has both committed transcript cells (finalized `HistoryCell`s) and an in-flight active
//! cell (`ChatWidget.active_cell`) that can mutate in place while streaming (often representing a
//! coalesced exec/tool group). The transcript overlay (`Ctrl+T`) renders committed cells plus a
//! cached, render-only live tail derived from the current active cell and pending plan streams so
//! in-flight tool calls are visible immediately.
//!
//! The transcript overlay is kept in sync by `App::overlay_forward_event`, which syncs a live tail
//! during draws using `transcript_live_tail_key()` and `transcript_live_tail_for_mode()`. The cache
//! key is designed to change when the active cell or markdown stream mutates, or when transcript
//! output is time-dependent, so the overlay can refresh its cached tail without rebuilding it on
//! every draw.
//!
//! The bottom pane exposes a single "task running" indicator that drives the spinner and interrupt
//! hints. This module treats that indicator as derived UI-busy state: it is set while an agent turn
//! is in progress and while MCP server startup is in progress. Those lifecycles are tracked
//! independently (`agent_turn_running` and `mcp_startup_status`) and synchronized via
//! `update_task_running_state`.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::product::agent::config::Config;
use crate::product::agent::config::ConstraintResult;
use crate::product::agent::config::types::Notifications;
use crate::product::agent::config::types::TuiBuddy;
use crate::product::agent::connectors;
use crate::product::agent::features::Feature;
use crate::product::agent::project_doc::DEFAULT_PROJECT_DOC_FILENAME;
use crate::product::agent::protocol::AgentMessageDeltaEvent;
use crate::product::agent::protocol::AgentMessageEvent;
use crate::product::agent::protocol::AgentReasoningDeltaEvent;
use crate::product::agent::protocol::AgentReasoningEvent;
use crate::product::agent::protocol::AgentReasoningRawContentDeltaEvent;
use crate::product::agent::protocol::AgentReasoningRawContentEvent;
use crate::product::agent::protocol::ApplyPatchApprovalRequestEvent;
use crate::product::agent::protocol::BackgroundEventEvent;
use crate::product::agent::protocol::BuddyTurnSnapshot;
use crate::product::agent::protocol::CodexErrorInfo;
use crate::product::agent::protocol::DeprecationNoticeEvent;
use crate::product::agent::protocol::ErrorEvent;
use crate::product::agent::protocol::Event;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::ExecApprovalRequestEvent;
use crate::product::agent::protocol::ExecCommandBeginEvent;
use crate::product::agent::protocol::ExecCommandEndEvent;
use crate::product::agent::protocol::ExecCommandOutputDeltaEvent;
use crate::product::agent::protocol::ExecCommandSource;
use crate::product::agent::protocol::ExitedReviewModeEvent;
use crate::product::agent::protocol::InputSlimmingEvent;
use crate::product::agent::protocol::ListCustomPromptsResponseEvent;
use crate::product::agent::protocol::ListSkillsResponseEvent;
use crate::product::agent::protocol::McpListToolsResponseEvent;
use crate::product::agent::protocol::McpStartupCompleteEvent;
use crate::product::agent::protocol::McpStartupStatus;
use crate::product::agent::protocol::McpStartupUpdateEvent;
use crate::product::agent::protocol::McpToolCallBeginEvent;
use crate::product::agent::protocol::McpToolCallEndEvent;
use crate::product::agent::protocol::Op;
use crate::product::agent::protocol::PatchApplyBeginEvent;
use crate::product::agent::protocol::ReasoningContentDeltaEvent;
use crate::product::agent::protocol::ReasoningRawContentDeltaEvent;
use crate::product::agent::protocol::ReviewRequest;
use crate::product::agent::protocol::ReviewTarget;
use crate::product::agent::protocol::SkillMetadata as ProtocolSkillMetadata;
use crate::product::agent::protocol::StreamErrorEvent;
use crate::product::agent::protocol::TerminalInteractionEvent;
use crate::product::agent::protocol::ThreadGoal;
use crate::product::agent::protocol::ThreadGoalClearedEvent;
use crate::product::agent::protocol::ThreadGoalReplaceConfirmationRequiredEvent;
use crate::product::agent::protocol::ThreadGoalSetMode;
use crate::product::agent::protocol::ThreadGoalSnapshotEvent;
use crate::product::agent::protocol::ThreadGoalStatus;
use crate::product::agent::protocol::ThreadGoalUpdatedEvent;
use crate::product::agent::protocol::TokenUsage;
use crate::product::agent::protocol::TokenUsageInfo;
use crate::product::agent::protocol::TurnAbortReason;
use crate::product::agent::protocol::TurnCompleteEvent;
use crate::product::agent::protocol::TurnDiffEvent;
use crate::product::agent::protocol::UndoCompletedEvent;
use crate::product::agent::protocol::UndoStartedEvent;
use crate::product::agent::protocol::UserMessageEvent;
use crate::product::agent::protocol::ViewImageToolCallEvent;
use crate::product::agent::protocol::WarningEvent;
use crate::product::agent::protocol::WebSearchBeginEvent;
use crate::product::agent::protocol::WebSearchEndEvent;
use crate::product::agent::skills::model::SkillMetadata;
#[cfg(target_os = "windows")]
use crate::product::agent::windows_sandbox::WindowsSandboxLevelExt;
use crate::product::otel::OtelManager;
use crate::product::protocol::ThreadId;
use crate::product::protocol::approvals::ElicitationRequestEvent;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::IdentityMask;
use crate::product::protocol::config_types::Personality;
use crate::product::protocol::config_types::Settings;
#[cfg(target_os = "windows")]
use crate::product::protocol::config_types::WindowsSandboxLevel;
use crate::product::protocol::items::ReasoningItem;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::models::local_image_label_text;
use crate::product::protocol::parse_command::ParsedCommand;
use crate::product::protocol::request_user_input::RequestUserInputEvent;
use crate::product::protocol::user_input::TextElement;
use crate::product::protocol::user_input::UserInput;
use crate::product::tui_app::status::format_model_provider_name;
use crate::product::tui_app::version::CODEX_CLI_VERSION;
use crate::product::utils_sleep_inhibitor::SleepInhibitor;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use rand::Rng;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tracing::debug;

const DEFAULT_MODEL_DISPLAY_NAME: &str = "loading";
const PLAN_IMPLEMENTATION_TITLE: &str = "Implement this plan?";
const PLAN_IMPLEMENTATION_YES: &str = "Yes, implement this plan";
const PLAN_IMPLEMENTATION_CLEAR_UNFINISHED_GOAL: &str =
    "Clear unfinished goal and implement this plan";
const PLAN_IMPLEMENTATION_NO: &str = "No, stay in planner identity";
const PLAN_IMPLEMENTATION_CODING_MESSAGE: &str = "Implement the plan.";
pub(crate) const DRAG_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

use crate::product::tui_app::InputSlimmingExitSummary;
use crate::product::tui_app::app_event::AppEvent;
use crate::product::tui_app::app_event::BuddyConfigEdit;
use crate::product::tui_app::app_event::ConnectorsSnapshot;
use crate::product::tui_app::app_event::ExitMode;
#[cfg(target_os = "windows")]
use crate::product::tui_app::app_event::WindowsSandboxEnableMode;
use crate::product::tui_app::app_event::WindowsSandboxFallbackReason;
use crate::product::tui_app::app_event_sender::AppEventSender;
use crate::product::tui_app::bottom_pane::ApprovalRequest;
use crate::product::tui_app::bottom_pane::BottomPane;
use crate::product::tui_app::bottom_pane::BottomPaneParams;
use crate::product::tui_app::bottom_pane::CancellationEvent;
use crate::product::tui_app::bottom_pane::DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED;
use crate::product::tui_app::bottom_pane::IdentityIndicator;
use crate::product::tui_app::bottom_pane::InputResult;
use crate::product::tui_app::bottom_pane::LocalImageAttachment;
use crate::product::tui_app::bottom_pane::QUIT_SHORTCUT_TIMEOUT;
use crate::product::tui_app::bottom_pane::SelectionAction;
use crate::product::tui_app::bottom_pane::SelectionItem;
use crate::product::tui_app::bottom_pane::SelectionViewParams;
use crate::product::tui_app::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::product::tui_app::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::product::tui_app::clipboard_paste::paste_image_to_temp_png;
use crate::product::tui_app::clipboard_text::ClipboardTextConfig;
use crate::product::tui_app::clipboard_text::write_text_to_clipboard;
use crate::product::tui_app::diff_render::display_path_for;
use crate::product::tui_app::exec_cell::CommandOutput;
use crate::product::tui_app::exec_cell::ExecCell;
use crate::product::tui_app::exec_cell::new_active_exec_command;
use crate::product::tui_app::exec_command::strip_bash_lc_and_escape;
use crate::product::tui_app::get_git_diff::get_git_diff;
use crate::product::tui_app::history_cell;
use crate::product::tui_app::history_cell::AgentMessageCell;
use crate::product::tui_app::history_cell::HistoryCell;
use crate::product::tui_app::history_cell::McpToolCallCell;
use crate::product::tui_app::history_cell::PlainHistoryCell;
use crate::product::tui_app::history_cell::ProposedPlanStreamCell;
use crate::product::tui_app::history_cell::WebSearchCell;
use crate::product::tui_app::identities;
use crate::product::tui_app::key_hint;
use crate::product::tui_app::key_hint::KeyBinding;
use crate::product::tui_app::markdown::append_markdown;
use crate::product::tui_app::mouse::MouseScrollState;
use crate::product::tui_app::render::renderable::ColumnRenderable;
use crate::product::tui_app::render::renderable::Renderable;
use crate::product::tui_app::sidebar::AgentPanelEntry;
use crate::product::tui_app::sidebar::InputSlimmingPanelSnapshot;
use crate::product::tui_app::sidebar::McpPanelSnapshot;
use crate::product::tui_app::sidebar::SIDEBAR_VISIBLE_FILES_LIMIT;
use crate::product::tui_app::sidebar::SidebarSnapshot;
use crate::product::tui_app::sidebar::SidebarWidget;
use crate::product::tui_app::sidebar::SkillPanelEntry;
use crate::product::tui_app::sidebar::StatusPanelSnapshot;
use crate::product::tui_app::sidebar::TaskPanelSnapshot;
use crate::product::tui_app::sidebar::TodoPanelItem;
use crate::product::tui_app::sidebar::TodoPanelSnapshot;
use crate::product::tui_app::slash_command::SlashCommand;
use crate::product::tui_app::status::cache_hit_percent;
use crate::product::tui_app::status::format_directory_display;
use crate::product::tui_app::status_indicator_widget::STATUS_DETAILS_DEFAULT_MAX_LINES;
use crate::product::tui_app::status_indicator_widget::StatusDetailsCapitalization;
use crate::product::tui_app::text_formatting::capitalize_first;
use crate::product::tui_app::text_formatting::truncate_text;
use crate::product::tui_app::transcript_view::TranscriptLiveTail;
use crate::product::tui_app::transcript_view::TranscriptLiveTailKey;
use crate::product::tui_app::transcript_view::TranscriptLiveTailSource;
use crate::product::tui_app::transcript_view::TranscriptMouseOutcome;
use crate::product::tui_app::transcript_view::TranscriptRenderMode;
use crate::product::tui_app::transcript_view::TranscriptScroll;
use crate::product::tui_app::transcript_view::TranscriptView;
use crate::product::tui_app::tui::FrameRequester;
mod interrupts;
use self::interrupts::InterruptManager;
mod agent;
use self::agent::attach_existing_thread;
use self::agent::spawn_agent;
mod session_header;
use self::session_header::SessionHeader;
mod skills;
pub(crate) use self::skills::SkillsModalItems;
use self::skills::collect_tool_mentions;
use self::skills::find_app_mentions;
use self::skills::find_skill_mentions_with_tool_mentions;
use crate::product::tui_app::streaming::controller::AgentMarkdownStreamController;
use crate::product::tui_app::streaming::controller::PlanStreamController;

use crate::product::agent::AuthManager;
use crate::product::agent::ThreadManager;
use crate::product::agent::protocol::AskForApproval;
use crate::product::agent::protocol::SandboxPolicy;
use crate::product::common::approval_presets::ApprovalPreset;
use crate::product::common::approval_presets::builtin_approval_presets;
use crate::product::file_search::FileMatch;
use crate::product::protocol::openai_models::ModelPreset;
use crate::product::protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crate::product::protocol::plan_tool::UpdatePlanArgs;
use crate::product::protocol::protocol::AgentJobDisplayStatus;
use crate::product::protocol::protocol::AgentJobKind;
use crate::product::protocol::protocol::AgentJobStatusEvent;
use strum::IntoEnumIterator;

const USER_SHELL_COMMAND_HELP_TITLE: &str = "Prefix a command with ! to run it locally";
const USER_SHELL_COMMAND_HELP_HINT: &str = "Example: !ls";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const UNIFIED_EXEC_VISIBILITY_DELAY: Duration = Duration::from_millis(250);
// Track information about an in-flight exec command.
struct RunningCommand {
    command: Vec<String>,
    parsed_cmd: Vec<ParsedCommand>,
    source: ExecCommandSource,
}

struct UnifiedExecProcessSummary {
    key: String,
    call_id: String,
    command_display: String,
    started_at: Instant,
    visible: bool,
    recent_chunks: Vec<String>,
}

struct UnifiedExecWaitState {
    command_display: String,
}

impl UnifiedExecWaitState {
    fn new(command_display: String) -> Self {
        Self { command_display }
    }

    fn is_duplicate(&self, command_display: &str) -> bool {
        self.command_display == command_display
    }
}

#[derive(Clone, Debug)]
struct UnifiedExecWaitStreak {
    process_id: String,
    command_display: Option<String>,
}

impl UnifiedExecWaitStreak {
    fn new(process_id: String, command_display: Option<String>) -> Self {
        Self {
            process_id,
            command_display: command_display.filter(|display| !display.is_empty()),
        }
    }

    fn update_command_display(&mut self, command_display: Option<String>) {
        if self.command_display.is_some() {
            return;
        }
        self.command_display = command_display.filter(|display| !display.is_empty());
    }
}

fn is_unified_exec_source(source: ExecCommandSource) -> bool {
    matches!(
        source,
        ExecCommandSource::UnifiedExecStartup | ExecCommandSource::UnifiedExecInteraction
    )
}

fn is_standard_tool_call(parsed_cmd: &[ParsedCommand]) -> bool {
    !parsed_cmd.is_empty()
        && parsed_cmd
            .iter()
            .all(|parsed| !matches!(parsed, ParsedCommand::Unknown { .. }))
}

fn extract_first_markdown_heading(markdown: &str) -> Option<String> {
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&level) {
            continue;
        }
        let rest = &trimmed[level..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let title = rest.trim().trim_end_matches('#').trim().to_string();
        if !title.is_empty() {
            return Some(title);
        }
    }
    None
}

#[derive(Clone, Debug)]
struct LoadedSkillSummary {
    name: String,
    path: PathBuf,
}

#[derive(Debug)]
enum RateLimitErrorKind {
    ModelCap {
        model: String,
        reset_after_seconds: Option<u64>,
    },
}

fn rate_limit_error_kind(info: &CodexErrorInfo) -> Option<RateLimitErrorKind> {
    match info {
        CodexErrorInfo::ModelCap {
            model,
            reset_after_seconds,
        } => Some(RateLimitErrorKind::ModelCap {
            model: model.clone(),
            reset_after_seconds: *reset_after_seconds,
        }),
        CodexErrorInfo::UsageLimitExceeded
        | CodexErrorInfo::ResponseTooManyFailedAttempts { .. }
        | CodexErrorInfo::ContextWindowExceeded
        | CodexErrorInfo::HttpConnectionFailed { .. }
        | CodexErrorInfo::ResponseStreamConnectionFailed { .. }
        | CodexErrorInfo::InternalServerError
        | CodexErrorInfo::Unauthorized
        | CodexErrorInfo::BadRequest
        | CodexErrorInfo::SandboxError
        | CodexErrorInfo::ResponseStreamDisconnected { .. }
        | CodexErrorInfo::ThreadRollbackFailed
        | CodexErrorInfo::Other => None,
    }
}

fn goal_status_label(status: ThreadGoalStatus) -> &'static str {
    match status {
        ThreadGoalStatus::Active => "active",
        ThreadGoalStatus::Paused => "paused",
        ThreadGoalStatus::Blocked => "blocked",
        ThreadGoalStatus::UsageLimited => "usage limited",
        ThreadGoalStatus::BudgetLimited => "limited by budget",
        ThreadGoalStatus::Complete => "complete",
    }
}

fn goal_usage_summary(goal: &ThreadGoal) -> String {
    let budget = goal
        .token_budget
        .map(|budget| format!(" / {budget} token budget"))
        .unwrap_or_default();
    format!(
        "Used {} tokens{budget}; elapsed {}.",
        goal.tokens_used,
        format_goal_elapsed_seconds(goal.time_used_seconds)
    )
}

fn format_goal_summary_title(goal: &ThreadGoal) -> String {
    let mut objective_lines = goal.objective.split('\n');
    let first_line = objective_lines.next().unwrap_or_default();
    let mut title = format!("Goal ({}) - {first_line}", goal_status_label(goal.status));
    for line in objective_lines {
        if line.trim().is_empty() {
            continue;
        }
        title.push('\n');
        title.push_str("  ");
        title.push_str(line);
    }
    title
}

fn format_goal_elapsed_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn edited_goal_status(status: ThreadGoalStatus) -> ThreadGoalStatus {
    match status {
        ThreadGoalStatus::Active => ThreadGoalStatus::Active,
        ThreadGoalStatus::Paused | ThreadGoalStatus::Blocked | ThreadGoalStatus::UsageLimited => {
            status
        }
        ThreadGoalStatus::BudgetLimited | ThreadGoalStatus::Complete => ThreadGoalStatus::Active,
    }
}

/// Common initialization parameters shared by all `ChatWidget` constructors.
pub(crate) struct ChatWidgetInit {
    pub(crate) config: Config,
    pub(crate) thread_manager: Arc<ThreadManager>,
    pub(crate) frame_requester: FrameRequester,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) initial_user_message: Option<UserMessage>,
    pub(crate) enhanced_keys_supported: bool,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) feedback: crate::product::feedback::CodexFeedback,
    pub(crate) is_first_run: bool,
    pub(crate) startup: ChatWidgetStartup,
    pub(crate) otel_manager: OtelManager,
}

#[derive(Clone)]
pub(crate) enum ChatWidgetStartup {
    Configured { model: Option<String> },
    NeedsProviderConfig { auto_open: bool },
    Deferred,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
enum ConnectorsCacheState {
    #[default]
    Uninitialized,
    Loading,
    Ready(ConnectorsSnapshot),
    Failed(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ExternalEditorState {
    #[default]
    Closed,
    Requested,
    Active,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StatusIndicatorState {
    header: String,
    details: Option<String>,
    details_max_lines: usize,
}

impl StatusIndicatorState {
    fn working() -> Self {
        Self {
            header: String::from("Working"),
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CliAgentJobEntry {
    agent_type: AgentJobKind,
    name: Option<String>,
    status: AgentJobDisplayStatus,
    message: Option<String>,
    display_order: usize,
}

#[derive(Default)]
struct ReasoningItemStreamState {
    summary_deltas: BTreeMap<i64, String>,
    raw_deltas: BTreeMap<i64, String>,
    raw_section_started: bool,
    last_raw_content_index: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReasoningContentSource {
    Summary,
    Raw,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReasoningBufferEntry {
    Content {
        item_id: Option<String>,
        source: ReasoningContentSource,
        markdown: String,
    },
    SectionBreak {
        item_id: Option<String>,
    },
}

impl ReasoningBufferEntry {
    fn item_id(&self) -> Option<&str> {
        match self {
            Self::Content { item_id, .. } | Self::SectionBreak { item_id } => item_id.as_deref(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LegacyReasoningCompletion {
    Summary(String),
    Raw(String),
}

/// Maintains the per-session UI state and interaction state machines for the chat screen.
///
/// `ChatWidget` owns the state derived from the protocol event stream (history cells, streaming
/// buffers, bottom-pane overlays, and transient status text) and turns key presses into user
/// intent (`Op` submissions and `AppEvent` requests).
///
/// It is not responsible for running the agent itself; it reflects progress by updating UI state
/// and by sending requests back to the LHA product runtime.
///
/// Quit/interrupt behavior intentionally spans layers: the bottom pane owns local input routing
/// (which view gets Ctrl+C), while `ChatWidget` owns process-level decisions such as interrupting
/// active work, arming the double-press quit shortcut, and requesting shutdown-first exit.
pub(crate) struct ChatWidget {
    app_event_tx: AppEventSender,
    codex_op_tx: UnboundedSender<Op>,
    bottom_pane: BottomPane,
    transcript: RefCell<TranscriptView>,
    mouse_scroll: MouseScrollState,
    active_cell: Option<Box<dyn HistoryCell>>,
    /// Monotonic-ish counter used to invalidate transcript overlay caching.
    ///
    /// The transcript overlay appends a cached "live tail" for the current active cell. Most
    /// active-cell updates are mutations of the *existing* cell (not a replacement), so pointer
    /// identity alone is not a good cache key.
    ///
    /// Callers bump this whenever the active cell's transcript output could change without
    /// flushing. It is intentionally allowed to wrap, which implies a rare one-time cache collision
    /// where the overlay may briefly treat new tail content as already cached.
    active_cell_revision: u64,
    config: Config,
    /// The unmasked identity settings.
    ///
    /// Masks are applied on top of this base identity to derive the effective identity.
    current_identity: Identity,
    /// The currently active identity mask, if any.
    active_identity_mask: Option<IdentityMask>,
    /// Fresh sessions start runtime as Nobody, so the TUI's persisted identity
    /// should be pushed once after the runtime session exists.
    pending_initial_identity_sync: bool,
    /// Startup model override for a resumed/forked thread, kept separate from
    /// identity masks so restored custom identity instructions are preserved.
    pending_existing_thread_model_override: Option<String>,
    /// User-selected reasoning effort overrides keyed by identity.
    ///
    /// A missing key means "use the preset default for this identity".
    reasoning_effort_overrides: HashMap<IdentityKind, Option<ReasoningEffortConfig>>,
    auth_manager: Arc<AuthManager>,
    thread_manager: Arc<ThreadManager>,
    otel_manager: OtelManager,
    session_header: SessionHeader,
    initial_user_message: Option<UserMessage>,
    token_info: Option<TokenUsageInfo>,
    // Stream lifecycle controller
    stream_controller: Option<AgentMarkdownStreamController>,
    // Final legacy AgentMessage echo expected after a blocking prompt forced an answer stream flush.
    pending_streamed_agent_message_echo: Option<String>,
    // Stream lifecycle controller for proposed plan output.
    plan_stream_controller: Option<PlanStreamController>,
    // Cache-busting counter for the render-only proposed plan live tail.
    plan_stream_revision: u64,
    // Whether the current turn has started streaming visible assistant answer content.
    answer_stream_started_this_turn: bool,
    running_commands: HashMap<String, RunningCommand>,
    suppressed_exec_calls: HashSet<String>,
    skills_all: Vec<ProtocolSkillMetadata>,
    skills_have_loaded: bool,
    skills_request_in_flight: bool,
    skills_refresh_pending: Option<bool>,
    skills_initial_state: Option<HashMap<PathBuf, bool>>,
    loaded_skills: Vec<LoadedSkillSummary>,
    last_unified_wait: Option<UnifiedExecWaitState>,
    unified_exec_wait_streak: Option<UnifiedExecWaitStreak>,
    turn_sleep_inhibitor: SleepInhibitor,
    task_complete_pending: bool,
    unified_exec_processes: Vec<UnifiedExecProcessSummary>,
    changed_files: VecDeque<String>,
    cli_agent_jobs: HashMap<String, CliAgentJobEntry>,
    /// Tracks whether the LHA product runtime currently considers an agent turn to be in progress.
    ///
    /// This is kept separate from `mcp_startup_status` so that MCP startup progress (or completion)
    /// can update the status header without accidentally clearing the spinner for an active turn.
    agent_turn_running: bool,
    /// Tracks per-server MCP startup state while startup is in progress.
    ///
    /// The map is `Some(_)` from the first `McpStartupUpdate` until `McpStartupComplete`, and the
    /// bottom pane is treated as "running" while this is populated, even if no agent turn is
    /// currently executing.
    mcp_startup_status: Option<HashMap<String, McpStartupStatus>>,
    connectors_cache: ConnectorsCacheState,
    // Queue of interruptive UI events deferred during an active write cycle
    interrupts: InterruptManager,
    // Accumulates reasoning sections with their source so raw content is never interpreted as a
    // summary title.
    reasoning_buffer: Vec<ReasoningBufferEntry>,
    // Structured deltas arrive before their completed reasoning item. Keep the per-item text so
    // completion can fill only a missing suffix instead of replaying the entire item.
    reasoning_item_states: HashMap<String, ReasoningItemStreamState>,
    // Live legacy completion events follow the structured ItemCompleted event with the same outer
    // id. Track the exact expected sequence so those compatibility echoes stay out of the TUI.
    pending_live_legacy_reasoning: HashMap<String, VecDeque<LegacyReasoningCompletion>>,
    // Legacy-only streams can send a complete raw item after a summary completion that already
    // contained the same raw text. Retain the last source until the next legacy delta to avoid a
    // duplicate cell in that old event shape.
    last_legacy_reasoning_finalized: Option<String>,
    // Full status snapshot shown in the status indicator.
    current_status: StatusIndicatorState,
    // Previous status snapshot to restore after a transient stream retry.
    retry_status: Option<StatusIndicatorState>,
    thread_id: Option<ThreadId>,
    agent_shutdown_complete: bool,
    thread_name: Option<String>,
    forked_from: Option<ThreadId>,
    context_compact_count: usize,
    input_slimming: Option<InputSlimmingPanelSnapshot>,
    // Structured compactions are counted by item id so multiple compactions in the same turn
    // are all reflected in `/status`.
    counted_context_compaction_item_ids: HashSet<String>,
    // Live legacy `ContextCompacted` events are emitted immediately after their structured
    // counterpart and reuse the same outer event id, so we track how many should be suppressed.
    pending_live_legacy_context_compactions: HashMap<String, usize>,
    // Replay does not provide outer event ids; we therefore suppress only the next legacy compact
    // event(s) that correspond to replayed structured compactions, including parent-thread events
    // during fork replay.
    pending_replay_legacy_context_compactions: usize,
    frame_requester: FrameRequester,
    // Whether to include the initial welcome banner on session configured
    show_welcome_banner: bool,
    // When resuming an existing session (selected via resume picker), avoid an
    // immediate redraw on SessionConfigured to prevent a gratuitous UI flicker.
    suppress_session_configured_redraw: bool,
    // User messages queued while a turn is in progress
    queued_user_messages: VecDeque<UserMessage>,
    // Pending notification to show when unfocused on next Draw
    pending_notification: Option<Notification>,
    /// When `Some`, the user has pressed a quit shortcut and the second press
    /// must occur before `quit_shortcut_expires_at`.
    quit_shortcut_expires_at: Option<Instant>,
    /// Tracks which quit shortcut key was pressed first.
    ///
    /// We require the second press to match this key so `Ctrl+C` followed by
    /// `Ctrl+D` (or vice versa) doesn't quit accidentally.
    quit_shortcut_key: Option<KeyBinding>,
    // Simple review mode flag; used to adjust layout and banners.
    is_review_mode: bool,
    // Snapshot of token usage to restore after review mode exits.
    pre_review_token_info: Option<Option<TokenUsageInfo>>,
    // True after the user submits /review and before the review banner is inserted.
    pending_review_start_transition: bool,
    // Status elapsed time preserved when review progress is hidden before TurnComplete.
    pending_review_elapsed_secs: Option<u64>,
    // Whether the next streamed assistant content should be preceded by a final message separator.
    //
    // This is set whenever we insert a visible history cell that conceptually belongs to a turn.
    // The separator itself is only rendered if the turn recorded "work" activity (see
    // `had_work_activity`).
    needs_final_message_separator: bool,
    // Whether the current turn performed "work" (exec commands, MCP tool calls, patch applications).
    //
    // This gates rendering of the "Worked for …" separator so purely conversational turns don't
    // show an empty divider. It is reset when the separator is emitted.
    had_work_activity: bool,
    // Whether the current turn emitted a plan update.
    saw_plan_update_this_turn: bool,
    // Whether the current turn emitted a proposed plan item.
    saw_plan_item_this_turn: bool,
    // Whether the current turn's proposed plan has already been rendered in history.
    pending_proposed_plan_rendered_this_turn: bool,
    // Incremental buffer for streamed plan content.
    plan_delta_buffer: String,
    // True while a plan item is streaming.
    plan_item_active: bool,
    // Heading from the latest completed proposed plan, retained across turn boundaries. A later
    // completed plan replaces it or clears it when that plan has no heading.
    latest_proposed_plan_title: Option<String>,
    // Full text of the latest completed proposed plan, retained across turn boundaries and
    // replaced by the next completed plan.
    latest_proposed_plan_text: Option<String>,
    latest_update_plan: Option<UpdatePlanArgs>,
    // Status-indicator elapsed seconds captured at the last emitted final-message separator.
    //
    // This lets the separator show per-chunk work time (since the previous separator) rather than
    // the total task-running time reported by the status indicator.
    last_separator_elapsed_secs: Option<u64>,

    last_rendered_width: std::cell::Cell<Option<usize>>,
    last_transcript_area: std::cell::Cell<Option<Rect>>,
    last_bottom_area: std::cell::Cell<Option<Rect>>,
    // Feedback sink for /feedback
    feedback: crate::product::feedback::CodexFeedback,
    // Current session rollout path (if known)
    current_rollout_path: Option<PathBuf>,
    current_goal: Option<ThreadGoal>,
    current_goal_state_known: bool,
    // A planner-created plan waits for this read-only snapshot before deciding which action to show.
    pending_plan_implementation_goal_state_refresh: bool,
    // A resolved snapshot can wait behind a temporary picker before showing its prompt.
    pending_plan_implementation_prompt: bool,
    external_editor_state: ExternalEditorState,
    git_branch: Option<String>,
}

/// Snapshot of active-cell state that affects transcript overlay rendering.
///
/// The overlay keeps a cached "live tail" for the in-flight cell; this key lets
/// it cheaply decide when to recompute that tail as the active cell evolves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActiveCellTranscriptKey {
    /// Cache-busting revision for in-place updates.
    ///
    /// Many active cells are updated incrementally while streaming (for example when exec groups
    /// add output or change status), and the transcript overlay caches its live tail, so this
    /// revision gives a cheap way to say "same active cell, but its transcript output is different
    /// now". Callers bump it on any mutation that can affect `HistoryCell::transcript_lines`.
    pub(crate) revision: u64,
    /// Whether the active cell continues the prior stream, which affects
    /// spacing between transcript blocks.
    pub(crate) is_stream_continuation: bool,
    /// Optional animation tick for time-dependent transcript output.
    ///
    /// When this changes, the overlay recomputes the cached tail even if the revision and width
    /// are unchanged, which is how shimmer/spinner visuals can animate in the overlay without any
    /// underlying data change.
    pub(crate) animation_tick: Option<u64>,
}

#[derive(Clone)]
pub(crate) struct UserMessage {
    text: String,
    local_images: Vec<LocalImageAttachment>,
    text_elements: Vec<TextElement>,
    mention_paths: HashMap<String, String>,
}

impl From<String> for UserMessage {
    fn from(text: String) -> Self {
        Self {
            text,
            local_images: Vec::new(),
            // Plain text conversion has no UI element ranges.
            text_elements: Vec::new(),
            mention_paths: HashMap::new(),
        }
    }
}

impl From<&str> for UserMessage {
    fn from(text: &str) -> Self {
        Self {
            text: text.to_string(),
            local_images: Vec::new(),
            // Plain text conversion has no UI element ranges.
            text_elements: Vec::new(),
            mention_paths: HashMap::new(),
        }
    }
}

pub(crate) fn create_initial_user_message(
    text: Option<String>,
    local_image_paths: Vec<PathBuf>,
    text_elements: Vec<TextElement>,
) -> Option<UserMessage> {
    let text = text.unwrap_or_default();
    if text.is_empty() && local_image_paths.is_empty() {
        None
    } else {
        let local_images = local_image_paths
            .into_iter()
            .enumerate()
            .map(|(idx, path)| LocalImageAttachment {
                placeholder: local_image_label_text(idx + 1),
                path,
            })
            .collect();
        Some(UserMessage {
            text,
            local_images,
            text_elements,
            mention_paths: HashMap::new(),
        })
    }
}

// When merging multiple queued drafts (e.g., after interrupt), each draft starts numbering
// its attachments at [Image #1]. Reassign placeholder labels based on the attachment list so
// the combined local_image_paths order matches the labels, even if placeholders were moved
// in the text (e.g., [Image #2] appearing before [Image #1]).
fn remap_placeholders_for_message(message: UserMessage, next_label: &mut usize) -> UserMessage {
    let UserMessage {
        text,
        text_elements,
        local_images,
        mention_paths,
    } = message;
    if local_images.is_empty() {
        return UserMessage {
            text,
            text_elements,
            local_images,
            mention_paths,
        };
    }

    let mut mapping: HashMap<String, String> = HashMap::new();
    let mut remapped_images = Vec::new();
    for attachment in local_images {
        let new_placeholder = local_image_label_text(*next_label);
        *next_label += 1;
        mapping.insert(attachment.placeholder.clone(), new_placeholder.clone());
        remapped_images.push(LocalImageAttachment {
            placeholder: new_placeholder,
            path: attachment.path,
        });
    }

    let mut elements = text_elements;
    elements.sort_by_key(|elem| elem.byte_range.start);

    let mut cursor = 0usize;
    let mut rebuilt = String::new();
    let mut rebuilt_elements = Vec::new();
    for mut elem in elements {
        let start = elem.byte_range.start.min(text.len());
        let end = elem.byte_range.end.min(text.len());
        if let Some(segment) = text.get(cursor..start) {
            rebuilt.push_str(segment);
        }

        let original = text.get(start..end).unwrap_or("");
        let placeholder = elem.placeholder(&text);
        let replacement = placeholder
            .and_then(|ph| mapping.get(ph))
            .map(String::as_str)
            .unwrap_or(original);

        let elem_start = rebuilt.len();
        rebuilt.push_str(replacement);
        let elem_end = rebuilt.len();

        if let Some(remapped) = placeholder.and_then(|ph| mapping.get(ph)) {
            elem.set_placeholder(Some(remapped.clone()));
        }
        elem.byte_range = (elem_start..elem_end).into();
        rebuilt_elements.push(elem);
        cursor = end;
    }
    if let Some(segment) = text.get(cursor..) {
        rebuilt.push_str(segment);
    }

    UserMessage {
        text: rebuilt,
        local_images: remapped_images,
        text_elements: rebuilt_elements,
        mention_paths,
    }
}

fn paths_from_unified_diff(diff: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut before_hunk = true;
    let mut old_path = None;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            before_hunk = true;
            old_path = None;
            continue;
        }

        if line.starts_with("@@") {
            before_hunk = false;
            old_path = None;
            continue;
        }

        if !before_hunk {
            continue;
        }

        if let Some(raw) = line.strip_prefix("--- ") {
            old_path = parse_old_diff_header_path(raw);
            continue;
        }

        if let Some(raw) = line.strip_prefix("+++ ") {
            let path = match (old_path.take(), parse_new_diff_header_path(raw)) {
                (Some(old_path), Some(new_path)) => new_path.or(old_path),
                _ => None,
            };
            if let Some(path) = path
                && !paths.iter().any(|existing| existing == &path)
            {
                paths.push(path);
            }
        }
    }
    paths
}

fn parse_old_diff_header_path(raw: &str) -> Option<Option<String>> {
    parse_diff_header_path(raw, "a/")
}

fn parse_new_diff_header_path(raw: &str) -> Option<Option<String>> {
    parse_diff_header_path(raw, "b/")
}

fn parse_diff_header_path(raw: &str, prefix: &str) -> Option<Option<String>> {
    let path = raw.split('\t').next().unwrap_or(raw);
    if path == "/dev/null" {
        return Some(None);
    }
    path.strip_prefix(prefix)
        .filter(|path| !path.is_empty())
        .map(|path| Some(path.to_string()))
}

impl ChatWidget {
    /// Synchronize the bottom-pane "task running" indicator with the current lifecycles.
    ///
    /// The bottom pane only has one running flag, but this module treats it as a derived state of
    /// the agent turn lifecycle, MCP startup lifecycle, and review-start transition.
    fn task_running_state(&self) -> bool {
        self.agent_turn_running
            || self.mcp_startup_status.is_some()
            || self.pending_review_start_transition
    }

    fn update_task_running_state(&mut self) {
        self.bottom_pane.set_task_running(self.task_running_state());
    }

    fn update_task_running_state_with_redraw(&mut self, request_redraw: bool) {
        self.bottom_pane
            .set_task_running_with_redraw(self.task_running_state(), request_redraw);
    }

    fn current_reasoning_entries(&self) -> &[ReasoningBufferEntry] {
        let current_section_start = self
            .reasoning_buffer
            .iter()
            .rposition(|entry| matches!(entry, ReasoningBufferEntry::SectionBreak { .. }))
            .map_or(0, |index| index + 1);
        &self.reasoning_buffer[current_section_start..]
    }

    fn reasoning_buffer_has_content(&self) -> bool {
        self.reasoning_buffer.iter().any(|entry| {
            matches!(
                entry,
                ReasoningBufferEntry::Content { markdown, .. } if !markdown.is_empty()
            )
        })
    }

    fn current_reasoning_content(&self, source: ReasoningContentSource) -> &str {
        self.current_reasoning_entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                ReasoningBufferEntry::Content {
                    source: entry_source,
                    markdown,
                    ..
                } if *entry_source == source => Some(markdown.as_str()),
                ReasoningBufferEntry::Content { .. }
                | ReasoningBufferEntry::SectionBreak { .. } => None,
            })
            .unwrap_or_default()
    }

    fn append_reasoning_delta(
        &mut self,
        item_id: Option<&str>,
        source: ReasoningContentSource,
        delta: String,
    ) {
        let item_id = item_id.map(str::to_owned);
        if let Some(ReasoningBufferEntry::Content {
            item_id: last_item_id,
            source: last_source,
            markdown,
        }) = self.reasoning_buffer.last_mut()
            && *last_source == source
            && last_item_id.as_deref() == item_id.as_deref()
        {
            markdown.push_str(&delta);
            return;
        }

        self.reasoning_buffer.push(ReasoningBufferEntry::Content {
            item_id,
            source,
            markdown: delta,
        });
    }

    fn append_reasoning_section_break(&mut self, item_id: Option<&str>) {
        self.reasoning_buffer
            .push(ReasoningBufferEntry::SectionBreak {
                item_id: item_id.map(str::to_owned),
            });
    }

    fn replace_reasoning_item_entries(
        &mut self,
        item_id: &str,
        entries: Vec<ReasoningBufferEntry>,
    ) {
        let mut replacement = Some(entries);
        let mut updated = Vec::with_capacity(self.reasoning_buffer.len());

        for entry in self.reasoning_buffer.drain(..) {
            if entry.item_id() == Some(item_id) {
                if let Some(entries) = replacement.take() {
                    updated.extend(entries);
                }
            } else {
                updated.push(entry);
            }
        }

        if let Some(entries) = replacement {
            if !updated.is_empty() && !entries.is_empty() {
                updated.push(ReasoningBufferEntry::SectionBreak {
                    item_id: Some(item_id.to_string()),
                });
            }
            updated.extend(entries);
        }

        self.reasoning_buffer = updated;
    }

    fn update_reasoning_status_header_from_presentation(
        &mut self,
        presentation: ReasoningPresentation,
    ) {
        if self.unified_exec_wait_streak.is_some() {
            return;
        }

        if let Some(header) = presentation.latest_status_title {
            self.set_status_header(header);
        }
    }

    fn update_reasoning_status_header(&mut self) {
        let presentation = reasoning_buffer_presentation(self.current_reasoning_entries());
        self.update_reasoning_status_header_from_presentation(presentation);
    }

    fn update_reasoning_status_header_from_buffer(&mut self) {
        let presentation = reasoning_buffer_presentation(&self.reasoning_buffer);
        self.update_reasoning_status_header_from_presentation(presentation);
    }

    fn restore_reasoning_status_header(&mut self) {
        if let Some(header) =
            reasoning_buffer_presentation(self.current_reasoning_entries()).latest_status_title
        {
            self.set_status_header(header);
        } else if self.bottom_pane.is_task_running() {
            self.set_status_header(String::from("Working"));
        }
    }

    fn flush_unified_exec_wait_streak(&mut self) {
        let Some(wait) = self.unified_exec_wait_streak.take() else {
            return;
        };
        self.needs_final_message_separator = true;
        let cell = history_cell::new_unified_exec_interaction(wait.command_display, String::new());
        self.app_event_tx.send_history_cell(Box::new(cell));
        self.restore_reasoning_status_header();
    }

    fn flush_answer_stream_with_separator(&mut self) {
        let _ = self.flush_answer_stream_with_separator_impl(false, None);
    }

    fn flush_answer_stream_for_blocking_prompt(&mut self) {
        let _ = self.flush_answer_stream_with_separator_impl(true, None);
    }

    fn flush_answer_stream_with_separator_impl(
        &mut self,
        remember_final_echo: bool,
        leading_separator: Option<history_cell::FinalMessageSeparator>,
    ) -> (bool, Option<history_cell::FinalMessageSeparator>) {
        let Some(controller) = self.stream_controller.take() else {
            return (false, leading_separator);
        };

        self.stop_commit_animation_if_no_stream_controllers();

        let source = controller.finalize();
        if source.is_empty() {
            return (false, leading_separator);
        }

        if remember_final_echo {
            self.pending_streamed_agent_message_echo
                .get_or_insert_with(String::new)
                .push_str(&source);
        }

        if let Some(separator) = leading_separator {
            self.app_event_tx.send_history_cell(Box::new(separator));
        }

        if !self.active_cell_is_answer_stream() {
            self.flush_active_cell();
            self.active_cell = Some(Box::new(AgentMessageCell::new_streaming_markdown(true)));
        }

        let updated = if let Some(cell) = self.active_answer_stream_cell_mut() {
            cell.show_all_markdown(source)
        } else {
            self.add_boxed_history_with_viewport_repaint(Box::new(AgentMessageCell::new_markdown(
                source, true,
            )));
            return (true, None);
        };

        if updated {
            self.bump_active_cell_revision();
        }
        self.flush_active_cell_with_viewport_repaint();
        (true, None)
    }

    fn active_cell_is_answer_stream(&self) -> bool {
        self.active_cell
            .as_ref()
            .and_then(|cell| cell.as_any().downcast_ref::<AgentMessageCell>())
            .is_some_and(AgentMessageCell::is_streaming_markdown)
    }

    fn active_answer_stream_cell_mut(&mut self) -> Option<&mut AgentMessageCell> {
        let cell = self
            .active_cell
            .as_mut()?
            .as_any_mut()
            .downcast_mut::<AgentMessageCell>()?;
        if cell.is_streaming_markdown() {
            Some(cell)
        } else {
            None
        }
    }

    fn ensure_answer_stream_active_cell(&mut self) {
        if self.active_cell_is_answer_stream() {
            return;
        }
        self.flush_active_cell();
        self.active_cell = Some(Box::new(AgentMessageCell::new_streaming_markdown(true)));
        self.bump_active_cell_revision();
    }

    fn sync_answer_stream_active_cell_from_controller(&mut self) {
        let Some((source, visible_lines)) = self.stream_controller.as_ref().map(|controller| {
            (
                controller.completed_source().to_string(),
                controller.visible_rendered_lines(),
            )
        }) else {
            return;
        };
        if source.is_empty() {
            return;
        }

        self.ensure_answer_stream_active_cell();
        let changed = self
            .active_answer_stream_cell_mut()
            .is_some_and(|cell| cell.set_markdown_stream_state(source, visible_lines));
        if changed {
            self.bump_active_cell_revision();
        }
    }

    fn set_answer_stream_visible_lines(&mut self, visible_lines: usize) {
        let changed = self
            .active_answer_stream_cell_mut()
            .is_some_and(|cell| cell.set_visible_rendered_lines(visible_lines));
        if changed {
            self.bump_active_cell_revision();
            self.request_redraw();
        }
    }

    fn bump_plan_stream_revision(&mut self) {
        self.plan_stream_revision = self.plan_stream_revision.wrapping_add(1);
        self.request_redraw_with_risky_row_repair();
    }

    fn plan_stream_has_visible_tail(&self) -> bool {
        self.plan_stream_controller
            .as_ref()
            .is_some_and(|controller| !controller.completed_source().is_empty())
    }

    fn plan_stream_live_tail_revision(&self) -> u64 {
        let completed_source_len = self
            .plan_stream_controller
            .as_ref()
            .map(|controller| controller.completed_source().len() as u64)
            .unwrap_or(0);
        self.plan_stream_revision
            .wrapping_mul(31)
            .wrapping_add(completed_source_len)
    }

    fn clear_plan_stream_controller(&mut self) -> bool {
        let cleared = self.plan_stream_controller.take().is_some();
        if cleared {
            self.bump_plan_stream_revision();
            self.request_redraw();
        }
        cleared
    }

    fn stop_commit_animation_if_no_stream_controllers(&self) {
        if self.stream_controller.is_none() && self.plan_stream_controller.is_none() {
            self.app_event_tx.send(AppEvent::StopCommitAnimation);
        }
    }

    fn discard_pending_proposed_plan_turn_state(&mut self) -> bool {
        let cleared_plan_stream = self.clear_plan_stream_controller();
        self.plan_delta_buffer.clear();
        self.plan_item_active = false;
        self.saw_plan_item_this_turn = false;
        self.pending_proposed_plan_rendered_this_turn = false;
        cleared_plan_stream
    }

    fn plan_stream_lines_for_mode(
        &self,
        width: u16,
        mode: TranscriptRenderMode,
    ) -> Vec<Line<'static>> {
        let Some(controller) = self.plan_stream_controller.as_ref() else {
            return Vec::new();
        };

        let source = controller.completed_source();
        if source.is_empty() {
            return Vec::new();
        }

        let cell = ProposedPlanStreamCell::from_stream_state(
            source.to_string(),
            controller.visible_rendered_lines(),
        );
        match mode {
            TranscriptRenderMode::Display => cell.display_lines(width),
            TranscriptRenderMode::Transcript => cell.transcript_lines(width),
        }
    }

    fn pending_proposed_plan_text(&self) -> Option<String> {
        if !self.saw_plan_item_this_turn {
            return None;
        }
        self.latest_proposed_plan_text
            .as_ref()
            .filter(|text| !text.trim().is_empty())
            .cloned()
    }

    fn flush_pending_proposed_plan(&mut self) -> bool {
        if self.pending_proposed_plan_rendered_this_turn {
            return false;
        }
        let Some(plan_text) = self.pending_proposed_plan_text() else {
            return false;
        };
        let cleared_plan_stream = self.clear_plan_stream_controller();
        if cleared_plan_stream {
            self.stop_commit_animation_if_no_stream_controllers();
        }
        self.flush_active_cell();
        self.add_to_history(history_cell::new_proposed_plan(plan_text));
        self.pending_proposed_plan_rendered_this_turn = true;
        true
    }

    fn final_message_separator(
        &self,
        elapsed_seconds: Option<u64>,
    ) -> Option<history_cell::FinalMessageSeparator> {
        let runtime_metrics = self.otel_manager.runtime_metrics_summary()?;
        Some(history_cell::FinalMessageSeparator::new(
            elapsed_seconds,
            Some(runtime_metrics),
        ))
    }

    /// Update the status indicator header and details.
    ///
    /// Passing `None` clears any existing details.
    fn set_status(
        &mut self,
        header: String,
        details: Option<String>,
        capitalization: StatusDetailsCapitalization,
        details_max_lines: usize,
    ) {
        let details = details
            .filter(|details| !details.is_empty())
            .map(|details| {
                let trimmed = details.trim_start();
                match capitalization {
                    StatusDetailsCapitalization::CapitalizeFirst => capitalize_first(trimmed),
                    StatusDetailsCapitalization::Preserve => trimmed.to_string(),
                }
            });
        self.current_status = StatusIndicatorState {
            header: header.clone(),
            details: details.clone(),
            details_max_lines,
        };
        self.bottom_pane.update_status(
            header,
            details,
            StatusDetailsCapitalization::Preserve,
            details_max_lines,
        );
    }

    /// Convenience wrapper around [`Self::set_status`];
    /// updates the status indicator header and clears any existing details.
    pub(crate) fn set_status_header(&mut self, header: String) {
        self.set_status(
            header,
            None,
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
    }

    fn restore_retry_status_if_present(&mut self) {
        if let Some(status) = self.retry_status.take() {
            self.set_status(
                status.header,
                status.details,
                StatusDetailsCapitalization::Preserve,
                status.details_max_lines,
            );
        }
    }

    // --- Small event handlers ---
    fn on_session_configured(
        &mut self,
        event: crate::product::agent::protocol::SessionConfiguredEvent,
    ) {
        let request_redraw = !self.suppress_session_configured_redraw;
        let session_identity_kind = event.identity_kind;
        let had_pending_initial_identity_sync = self.pending_initial_identity_sync;
        let existing_thread_model_override = if had_pending_initial_identity_sync {
            None
        } else {
            self.pending_existing_thread_model_override.take()
        };
        self.bottom_pane
            .set_history_metadata(event.history_log_id, event.history_entry_count);
        if request_redraw {
            self.set_skills(None);
            self.bottom_pane.set_connectors_snapshot(None);
        } else {
            self.bottom_pane.set_skills_without_redraw(None);
            self.bottom_pane
                .set_connectors_snapshot_without_redraw(None);
        }
        self.thread_id = Some(event.session_id);
        self.agent_shutdown_complete = false;
        self.app_event_tx
            .set_history_thread_id(Some(event.session_id));
        self.thread_name = event.thread_name.clone();
        self.forked_from = event.forked_from_id;
        self.context_compact_count = 0;
        self.input_slimming = None;
        self.counted_context_compaction_item_ids.clear();
        self.pending_live_legacy_context_compactions.clear();
        self.pending_replay_legacy_context_compactions = 0;
        self.clear_reasoning_buffers();
        self.current_rollout_path = event.rollout_path.clone();
        self.current_goal = None;
        self.current_goal_state_known = false;
        self.pending_plan_implementation_goal_state_refresh = false;
        self.pending_plan_implementation_prompt = false;
        self.pending_proposed_plan_rendered_this_turn = false;
        self.latest_proposed_plan_text = None;
        self.latest_proposed_plan_title = None;
        let initial_messages = event.initial_messages.clone();
        let model_for_header =
            existing_thread_model_override.unwrap_or_else(|| event.model.clone());
        self.session_header.set_model(&model_for_header);
        self.current_identity = self.current_identity.with_updates(
            Some(model_for_header.clone()),
            Some(event.reasoning_effort),
            None,
        );
        self.refresh_model_display();
        self.update_footer_info_with_redraw(request_redraw);
        self.sync_personality_command_enabled_with_redraw(request_redraw);
        let session_info_cell = history_cell::new_session_info(
            &self.config,
            &model_for_header,
            event,
            self.show_welcome_banner,
        );
        self.apply_session_info_cell(session_info_cell);

        if let Some(messages) = initial_messages {
            self.replay_initial_messages(messages);
        }
        let should_sync_initial_identity = had_pending_initial_identity_sync
            && self.identities_enabled()
            && self.active_identity_kind() != session_identity_kind;
        self.pending_initial_identity_sync = false;
        if should_sync_initial_identity {
            self.sync_active_identity_to_runtime();
        } else if !self.identities_enabled() && session_identity_kind != IdentityKind::Nobody {
            self.sync_identity_to_runtime(self.identity_without_preset());
        } else if !had_pending_initial_identity_sync && self.identities_enabled() {
            self.set_restored_identity_kind_from_session(session_identity_kind, request_redraw);
        }
        // Ask the LHA product runtime to enumerate custom prompts for this session.
        self.submit_op(Op::ListCustomPrompts);
        self.request_skills_refresh(true);
        if self.connectors_enabled() {
            self.prefetch_connectors();
        }
        if let Some(user_message) = self.initial_user_message.take() {
            self.submit_user_message(user_message);
        }
        if !self.suppress_session_configured_redraw {
            self.request_redraw();
        }
    }

    fn on_thread_name_updated(
        &mut self,
        event: crate::product::agent::protocol::ThreadNameUpdatedEvent,
    ) {
        if self.thread_id == Some(event.thread_id) && self.thread_name != event.thread_name {
            self.thread_name = event.thread_name;
            self.request_redraw_with_risky_row_repair();
        }
    }

    fn on_thread_goal_updated(&mut self, event: ThreadGoalUpdatedEvent) {
        if self.thread_id != Some(event.thread_id) {
            return;
        }
        self.current_goal = Some(event.goal.clone());
        self.current_goal_state_known = true;
        self.show_goal_summary(&event.goal);
        self.request_redraw();
    }

    fn on_thread_goal_cleared(&mut self, event: ThreadGoalClearedEvent) {
        if self.thread_id != Some(event.thread_id) {
            return;
        }
        self.current_goal = None;
        self.current_goal_state_known = true;
        self.add_info_message("Goal cleared.".to_string(), None);
        self.request_redraw();
    }

    fn on_thread_goal_snapshot(&mut self, event: ThreadGoalSnapshotEvent) {
        if self.thread_id != Some(event.thread_id) {
            return;
        }
        let refreshes_plan_implementation =
            std::mem::take(&mut self.pending_plan_implementation_goal_state_refresh);
        self.current_goal = event.goal;
        self.current_goal_state_known = true;
        if refreshes_plan_implementation {
            self.pending_plan_implementation_prompt = true;
            self.request_redraw();
            self.maybe_prompt_plan_implementation();
            return;
        }
        if let Some(goal) = self.current_goal.clone() {
            self.show_goal_summary(&goal);
        } else {
            self.show_no_goal_usage();
        }
        self.request_redraw();
    }

    fn on_thread_goal_replace_confirmation_required(
        &mut self,
        event: ThreadGoalReplaceConfirmationRequiredEvent,
    ) {
        if self.thread_id != Some(event.thread_id) {
            return;
        }
        let expected_goal_id = event.existing_goal.goal_id.clone();
        self.current_goal = Some(event.existing_goal);
        self.current_goal_state_known = true;
        let objective = event.objective;
        let replace_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::CodexOp(Op::ThreadGoalSetObjective {
                objective: objective.clone(),
                mode: ThreadGoalSetMode::ReplaceExisting {
                    expected_goal_id: expected_goal_id.clone(),
                },
            }));
        })];
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Replace current goal?".to_string()),
            subtitle: Some("A goal is already in progress.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Replace goal".to_string(),
                    description: Some("Start the new objective.".to_string()),
                    actions: replace_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Keep current goal".to_string(),
                    description: Some("Cancel the replacement.".to_string()),
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            allow_background_transcript_interaction: true,
            ..Default::default()
        });
        self.request_redraw();
    }

    fn set_skills(&mut self, skills: Option<Vec<SkillMetadata>>) {
        self.bottom_pane.set_skills(skills);
    }

    pub(crate) fn open_feedback_note(
        &mut self,
        category: crate::product::tui_app::app_event::FeedbackCategory,
        include_logs: bool,
    ) {
        // Build a fresh snapshot at the time of opening the note overlay.
        let snapshot = self.feedback.snapshot(self.thread_id);
        let rollout = if include_logs {
            self.current_rollout_path.clone()
        } else {
            None
        };
        let view = crate::product::tui_app::bottom_pane::FeedbackNoteView::new(
            category,
            self.config.lha_home.clone(),
            snapshot,
            rollout,
            self.app_event_tx.clone(),
            include_logs,
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn open_app_link_view(
        &mut self,
        title: String,
        description: Option<String>,
        instructions: String,
        url: String,
        is_installed: bool,
    ) {
        let view = crate::product::tui_app::bottom_pane::AppLinkView::new(
            title,
            description,
            instructions,
            url,
            is_installed,
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn open_feedback_consent(
        &mut self,
        category: crate::product::tui_app::app_event::FeedbackCategory,
    ) {
        let params = crate::product::tui_app::bottom_pane::feedback_upload_consent_params(
            self.app_event_tx.clone(),
            category,
            self.current_rollout_path.clone(),
        );
        self.bottom_pane.show_selection_view(params);
        self.request_redraw();
    }

    fn on_agent_message(&mut self, message: String) {
        // If we have a stream_controller, then the final agent message is redundant and will be a
        // duplicate of what has already been streamed.
        let repeats_forced_stream = self
            .pending_streamed_agent_message_echo
            .take()
            .is_some_and(|pending| pending == message);
        if !repeats_forced_stream && self.stream_controller.is_none() && !message.is_empty() {
            self.handle_streaming_delta(message);
        }
        self.flush_answer_stream_with_separator();
        self.handle_stream_finished();
        self.request_redraw();
    }

    fn on_agent_message_delta(&mut self, delta: String) {
        self.handle_streaming_delta(delta);
    }

    fn on_plan_delta(&mut self, delta: String) {
        if self.active_identity_kind() != IdentityKind::Planner {
            return;
        }
        if !self.plan_item_active {
            self.plan_item_active = true;
            self.plan_delta_buffer.clear();
        }
        self.plan_delta_buffer.push_str(&delta);
        self.flush_unified_exec_wait_streak();
        if self.active_cell.is_some() && !self.active_cell_is_answer_stream() {
            self.flush_active_cell();
        }
        if self.plan_stream_controller.is_none() {
            self.plan_stream_controller = Some(PlanStreamController::new());
            self.bump_plan_stream_revision();
        }
        let stream_width = self.last_rendered_width.get().map(|w| w.saturating_sub(4));
        let should_start_animation = self
            .plan_stream_controller
            .as_mut()
            .is_some_and(|controller| controller.push(&delta, stream_width));
        if should_start_animation {
            self.app_event_tx.send(AppEvent::StartCommitAnimation);
            self.on_commit_tick();
        }
        self.request_redraw();
    }

    fn on_plan_item_completed(&mut self, text: String) {
        let streamed_plan = self.plan_delta_buffer.trim().to_string();
        let plan_text = if text.trim().is_empty() {
            streamed_plan
        } else {
            text
        };
        self.plan_delta_buffer.clear();
        self.plan_item_active = false;
        self.saw_plan_item_this_turn = true;
        self.latest_proposed_plan_title = extract_first_markdown_heading(&plan_text);
        self.latest_proposed_plan_text =
            (!plan_text.trim().is_empty()).then_some(plan_text.clone());
        self.request_redraw_with_risky_row_repair();
    }

    fn on_agent_reasoning_delta(
        &mut self,
        item_id: Option<&str>,
        source: ReasoningContentSource,
        delta: String,
    ) {
        // Reasoning titles are live status; the remaining Markdown is finalized into history.
        self.append_reasoning_delta(item_id, source, delta);

        if source == ReasoningContentSource::Summary {
            self.update_reasoning_status_header();
        }
        self.request_redraw();
    }

    fn on_reasoning_content_delta(&mut self, event: ReasoningContentDeltaEvent) {
        let ReasoningContentDeltaEvent {
            item_id,
            delta,
            summary_index,
            ..
        } = event;
        self.reasoning_item_states
            .entry(item_id.clone())
            .or_default()
            .summary_deltas
            .entry(summary_index)
            .or_default()
            .push_str(&delta);
        self.on_agent_reasoning_delta(Some(&item_id), ReasoningContentSource::Summary, delta);
    }

    fn on_reasoning_raw_content_delta(&mut self, event: ReasoningRawContentDeltaEvent) {
        if !self.config.show_raw_agent_reasoning {
            return;
        }

        let ReasoningRawContentDeltaEvent {
            item_id,
            delta,
            content_index,
            ..
        } = event;
        let has_reasoning_content = self.reasoning_buffer_has_content();
        let should_start_new_section = {
            let state = self
                .reasoning_item_states
                .entry(item_id.clone())
                .or_default();
            let new_raw_part = state
                .last_raw_content_index
                .is_some_and(|index| index != content_index);
            state.last_raw_content_index = Some(content_index);
            state
                .raw_deltas
                .entry(content_index)
                .or_default()
                .push_str(&delta);
            let should_start = !state.raw_section_started || new_raw_part;
            state.raw_section_started = true;
            should_start && has_reasoning_content
        };
        if should_start_new_section {
            self.append_reasoning_section_break(Some(&item_id));
        }
        self.on_agent_reasoning_delta(Some(&item_id), ReasoningContentSource::Raw, delta);
    }

    fn on_reasoning_item_completed(
        &mut self,
        event_id: Option<&str>,
        item: ReasoningItem,
        from_replay: bool,
    ) {
        if from_replay {
            self.replay_reasoning_item(item);
            return;
        }

        let state = self
            .reasoning_item_states
            .remove(&item.id)
            .unwrap_or_default();
        let entries =
            canonical_reasoning_item_entries(&item, &state, self.config.show_raw_agent_reasoning);
        self.replace_reasoning_item_entries(&item.id, entries);
        self.update_reasoning_status_header_from_buffer();
        self.on_agent_reasoning_final();
        if let Some(event_id) = event_id {
            self.register_live_legacy_reasoning_completion(event_id, &item);
        }
    }

    fn replay_reasoning_item(&mut self, item: ReasoningItem) {
        let state = ReasoningItemStreamState::default();
        let entries =
            canonical_reasoning_item_entries(&item, &state, self.config.show_raw_agent_reasoning);
        self.replace_reasoning_item_entries(&item.id, entries);
        self.update_reasoning_status_header_from_buffer();
        if self.reasoning_buffer_has_content() {
            self.on_agent_reasoning_final();
        }
    }

    fn register_live_legacy_reasoning_completion(&mut self, event_id: &str, item: &ReasoningItem) {
        let pending = self
            .pending_live_legacy_reasoning
            .entry(event_id.to_string())
            .or_default();
        pending.extend(
            item.summary_text
                .iter()
                .cloned()
                .map(LegacyReasoningCompletion::Summary),
        );
        if self.config.show_raw_agent_reasoning {
            pending.extend(
                item.raw_content
                    .iter()
                    .cloned()
                    .map(LegacyReasoningCompletion::Raw),
            );
        }
    }

    fn consume_live_legacy_reasoning_completion(
        &mut self,
        event_id: Option<&str>,
        completion: LegacyReasoningCompletion,
    ) -> bool {
        let Some(event_id) = event_id else {
            return false;
        };

        let mut remove_entry = false;
        let consumed = if let Some(pending) = self.pending_live_legacy_reasoning.get_mut(event_id) {
            if pending.front() == Some(&completion) {
                pending.pop_front();
                remove_entry = pending.is_empty();
                true
            } else {
                false
            }
        } else {
            false
        };
        if remove_entry {
            self.pending_live_legacy_reasoning.remove(event_id);
        }
        consumed
    }

    fn on_legacy_reasoning_delta(&mut self, delta: String) {
        self.last_legacy_reasoning_finalized = None;
        self.on_agent_reasoning_delta(None, ReasoningContentSource::Summary, delta);
    }

    fn on_legacy_reasoning_final(&mut self, event_id: Option<&str>, text: String) {
        if self.consume_live_legacy_reasoning_completion(
            event_id,
            LegacyReasoningCompletion::Summary(text.clone()),
        ) {
            return;
        }

        self.last_legacy_reasoning_finalized = None;
        self.append_reasoning_completion(ReasoningContentSource::Summary, &text);
        self.finalize_legacy_reasoning();
    }

    fn on_legacy_raw_reasoning_delta(&mut self, delta: String) {
        if !self.config.show_raw_agent_reasoning {
            return;
        }
        self.last_legacy_reasoning_finalized = None;
        self.on_agent_reasoning_delta(None, ReasoningContentSource::Raw, delta);
    }

    fn on_legacy_raw_reasoning_final(&mut self, event_id: Option<&str>, text: String) {
        if !self.config.show_raw_agent_reasoning
            || self.consume_live_legacy_reasoning_completion(
                event_id,
                LegacyReasoningCompletion::Raw(text.clone()),
            )
            || self
                .last_legacy_reasoning_finalized
                .as_deref()
                .is_some_and(|finalized| finalized.ends_with(&text))
        {
            return;
        }

        self.last_legacy_reasoning_finalized = None;
        self.append_reasoning_completion(ReasoningContentSource::Raw, &text);
        self.finalize_legacy_reasoning();
    }

    fn append_reasoning_completion(&mut self, source: ReasoningContentSource, text: &str) {
        let received = self.current_reasoning_content(source).to_string();
        if source == ReasoningContentSource::Summary {
            let presentation = split_reasoning_presentation(&received);
            if presentation.transcript_markdown.is_empty()
                && presentation.latest_status_title.as_deref() == Some(text.trim())
            {
                return;
            }
        }

        let missing = missing_reasoning_suffix(&received, text);
        if !missing.is_empty() {
            self.on_agent_reasoning_delta(None, source, missing.to_string());
        }
    }

    fn finalize_legacy_reasoning(&mut self) {
        let finalized = reasoning_buffer_markdown(&self.reasoning_buffer);
        self.on_agent_reasoning_final();
        self.last_legacy_reasoning_finalized = (!finalized.is_empty()).then_some(finalized);
    }

    fn on_agent_reasoning_final(&mut self) {
        let presentation = reasoning_buffer_presentation(&self.reasoning_buffer);
        if !presentation.transcript_markdown.is_empty() {
            let cell = if self.should_hide_reasoning_summary_from_display() {
                history_cell::new_reasoning_summary_content_transcript_only(
                    presentation.transcript_markdown,
                )
            } else {
                history_cell::new_reasoning_summary_content(presentation.transcript_markdown)
            };
            self.add_boxed_history(cell);
        }
        self.reasoning_buffer.clear();
        self.request_redraw();
    }

    fn should_hide_reasoning_summary_from_display(&self) -> bool {
        self.answer_stream_started_this_turn || self.is_review_mode
    }

    fn clear_reasoning_buffers(&mut self) {
        self.reasoning_buffer.clear();
        self.reasoning_item_states.clear();
        self.pending_live_legacy_reasoning.clear();
        self.last_legacy_reasoning_finalized = None;
    }

    fn finish_review_progress_ui(&mut self) {
        self.clear_reasoning_buffers();
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.unified_exec_wait_streak = None;
        self.clear_unified_exec_processes();
        self.current_status = StatusIndicatorState::working();
        self.retry_status = None;
        self.pending_review_elapsed_secs = self
            .bottom_pane
            .status_widget()
            .map(super::status_indicator_widget::StatusIndicatorWidget::elapsed_seconds);
        self.bottom_pane.hide_status_indicator();
        self.request_redraw();
    }

    fn on_reasoning_section_break(&mut self) {
        // Start a new reasoning block for header extraction and accumulate transcript.
        self.append_reasoning_section_break(None);
    }

    fn on_agent_job_status(&mut self, event: AgentJobStatusEvent) {
        let AgentJobStatusEvent {
            job_id,
            agent_type,
            name,
            status,
            message,
        } = event;
        let name = normalize_agent_job_name(name);
        let next_display_order = self.cli_agent_jobs.len() + 1;
        let next_entry = if let Some(entry) = self.cli_agent_jobs.get(&job_id) {
            CliAgentJobEntry {
                agent_type,
                name: name.or_else(|| entry.name.clone()),
                status,
                message,
                display_order: entry.display_order,
            }
        } else {
            CliAgentJobEntry {
                agent_type,
                name,
                status,
                message,
                display_order: next_display_order,
            }
        };
        if self.cli_agent_jobs.get(&job_id) != Some(&next_entry) {
            self.cli_agent_jobs.insert(job_id, next_entry);
            self.request_redraw_with_risky_row_repair();
        }
    }

    fn clear_cli_agent_jobs(&mut self) {
        if !self.cli_agent_jobs.is_empty() {
            self.cli_agent_jobs.clear();
            self.request_redraw_with_risky_row_repair();
        }
    }

    fn on_task_started(&mut self) {
        let defer_review_redraw = self.pending_review_start_transition;
        self.pending_review_elapsed_secs = None;
        self.agent_turn_running = true;
        self.turn_sleep_inhibitor
            .set_turn_running(/* turn_running */ true);
        self.saw_plan_update_this_turn = false;
        self.pending_plan_implementation_goal_state_refresh = false;
        self.pending_plan_implementation_prompt = false;
        let cleared_plan_stream = self.discard_pending_proposed_plan_turn_state();
        if cleared_plan_stream {
            self.stop_commit_animation_if_no_stream_controllers();
        }
        self.answer_stream_started_this_turn = false;
        self.pending_streamed_agent_message_echo = None;
        self.otel_manager.reset_runtime_metrics();
        if defer_review_redraw {
            self.bottom_pane.clear_quit_shortcut_hint_with_redraw(false);
        } else {
            self.bottom_pane.clear_quit_shortcut_hint();
        }
        self.quit_shortcut_expires_at = None;
        self.quit_shortcut_key = None;
        self.current_status = StatusIndicatorState::working();
        self.retry_status = None;
        self.clear_reasoning_buffers();
        if defer_review_redraw {
            self.update_task_running_state_with_redraw(false);
            return;
        }
        self.update_task_running_state();
        self.bottom_pane.set_interrupt_hint_visible(true);
        self.set_status_header(String::from("Working"));
        self.request_redraw();
    }

    fn on_task_complete(&mut self, last_agent_message: Option<String>, from_replay: bool) {
        // If a stream is currently active, finalize it.
        let pending_review_elapsed_secs = self.pending_review_elapsed_secs.take();
        let final_separator = if from_replay {
            None
        } else {
            let elapsed_seconds = self
                .bottom_pane
                .status_widget()
                .map(super::status_indicator_widget::StatusIndicatorWidget::elapsed_seconds)
                .or(pending_review_elapsed_secs);
            self.final_message_separator(elapsed_seconds)
        };
        let (_flushed_answer, mut final_separator) =
            self.flush_answer_stream_with_separator_impl(false, final_separator);
        self.flush_unified_exec_wait_streak();
        if self.pending_proposed_plan_text().is_some()
            && let Some(separator) = final_separator.take()
        {
            self.app_event_tx
                .send_history_cell_with_viewport_repaint(Box::new(separator));
        }
        self.flush_pending_proposed_plan();
        if !from_replay {
            if let Some(separator) = final_separator {
                self.add_to_history_with_viewport_repaint(separator);
            }
            self.needs_final_message_separator = false;
            self.had_work_activity = false;
        }
        if !from_replay {
            self.scroll_transcript_to_bottom();
        }
        // Mark task stopped and request redraw now that all content is in history.
        self.agent_turn_running = false;
        self.turn_sleep_inhibitor
            .set_turn_running(/* turn_running */ false);
        self.update_task_running_state();
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.unified_exec_wait_streak = None;
        self.pending_streamed_agent_message_echo = None;
        self.clear_unified_exec_processes();
        self.clear_reasoning_buffers();
        self.request_redraw();

        if !from_replay && self.queued_user_messages.is_empty() {
            self.maybe_prompt_plan_implementation();
        }
        // If there is a queued user message, send exactly one now to begin the next turn.
        self.maybe_send_next_queued_input();
        // Emit a notification when the turn completes (suppressed if focused).
        self.notify(Notification::AgentTurnComplete {
            response: last_agent_message.unwrap_or_default(),
        });
    }

    fn maybe_prompt_plan_implementation(&mut self) {
        if self.task_running_state() {
            return;
        }
        if !self.identities_enabled() {
            return;
        }
        if !self.queued_user_messages.is_empty() {
            return;
        }
        if self.active_identity_kind() != IdentityKind::Planner {
            return;
        }
        if !self.saw_plan_item_this_turn {
            return;
        }
        if !self.bottom_pane.no_modal_or_popup_active() {
            return;
        }
        if self.config.features.enabled(Feature::Goals) && !self.current_goal_state_known {
            if !self.pending_plan_implementation_goal_state_refresh {
                self.pending_plan_implementation_goal_state_refresh = true;
                self.submit_op(Op::ThreadGoalGet);
            }
            return;
        }

        self.pending_plan_implementation_prompt = false;
        self.open_plan_implementation_prompt();
    }

    fn retry_pending_plan_implementation_prompt(&mut self) {
        if self.pending_plan_implementation_prompt && self.bottom_pane.no_modal_or_popup_active() {
            self.maybe_prompt_plan_implementation();
        }
    }

    fn open_plan_implementation_prompt(&mut self) {
        let programmer_mask = identities::programmer_mask(self.thread_manager.as_ref());
        let goal_tracking_enabled = self.config.features.enabled(Feature::Goals);
        let latest_plan_text = self.latest_proposed_plan_text.clone();
        let unfinished_goal = if goal_tracking_enabled {
            self.current_goal
                .as_ref()
                .filter(|goal| goal.status != ThreadGoalStatus::Complete)
        } else {
            None
        };
        let (implement_actions, implement_disabled_reason) = if let Some(goal) = unfinished_goal {
            (
                Vec::new(),
                Some(format!(
                    "Clear the current {} /goal before implementing this plan.",
                    goal_status_label(goal.status)
                )),
            )
        } else {
            match (programmer_mask.as_ref(), goal_tracking_enabled) {
                (Some(mask), true) => {
                    if let Some(plan_text) = latest_plan_text.as_ref() {
                        let mask = mask.clone();
                        let plan_text = plan_text.clone();
                        let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                            tx.send(AppEvent::StartGoalFromProposedPlan {
                                plan_text: plan_text.clone(),
                                identity: mask.clone(),
                            });
                        })];
                        (actions, None)
                    } else {
                        (Vec::new(), Some("latest plan text unavailable".to_string()))
                    }
                }
                (Some(mask), false) => {
                    let mask = mask.clone();
                    let user_text = PLAN_IMPLEMENTATION_CODING_MESSAGE.to_string();
                    let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                        tx.send(AppEvent::SubmitUserMessageWithMode {
                            text: user_text.clone(),
                            identity: mask.clone(),
                        });
                    })];
                    (actions, None)
                }
                (None, _) => (
                    Vec::new(),
                    Some("programmer identity unavailable".to_string()),
                ),
            }
        };

        let mut items = vec![SelectionItem {
            name: PLAN_IMPLEMENTATION_YES.to_string(),
            description: Some(if goal_tracking_enabled {
                "Switch to programmer and track this plan as a /goal until complete.".to_string()
            } else {
                "Switch to programmer identity.".to_string()
            }),
            selected_description: None,
            is_current: false,
            actions: implement_actions,
            disabled_reason: implement_disabled_reason,
            dismiss_on_select: true,
            ..Default::default()
        }];
        if let Some(goal) = unfinished_goal {
            let (actions, disabled_reason) =
                match (programmer_mask.as_ref(), latest_plan_text.as_ref()) {
                    (Some(mask), Some(plan_text)) if !goal.goal_id.is_empty() => {
                        let mask = mask.clone();
                        let plan_text = plan_text.clone();
                        let expected_goal_id = goal.goal_id.clone();
                        let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                            tx.send(AppEvent::ClearGoalAndStartFromProposedPlan {
                                plan_text: plan_text.clone(),
                                expected_goal_id: expected_goal_id.clone(),
                                identity: mask.clone(),
                            });
                        })];
                        (actions, None)
                    }
                    (Some(_), Some(_)) => {
                        (Vec::new(), Some("current goal id unavailable".to_string()))
                    }
                    (Some(_), None) => {
                        (Vec::new(), Some("latest plan text unavailable".to_string()))
                    }
                    (None, _) => (
                        Vec::new(),
                        Some("programmer identity unavailable".to_string()),
                    ),
                };
            items.push(SelectionItem {
                name: PLAN_IMPLEMENTATION_CLEAR_UNFINISHED_GOAL.to_string(),
                description: Some(
                    "Clear the current /goal, switch to programmer, and track this plan until complete."
                        .to_string(),
                ),
                selected_description: None,
                is_current: false,
                actions,
                disabled_reason,
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        items.push(SelectionItem {
            name: PLAN_IMPLEMENTATION_NO.to_string(),
            description: Some("Continue planning with the model.".to_string()),
            selected_description: None,
            is_current: false,
            actions: Vec::new(),
            dismiss_on_select: true,
            ..Default::default()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(PLAN_IMPLEMENTATION_TITLE.to_string()),
            subtitle: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            allow_background_transcript_interaction: true,
            ..Default::default()
        });
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        match info {
            Some(info) => self.apply_token_info(info),
            None => {
                if self.token_info.is_none() {
                    return;
                }
                self.bottom_pane.set_context_window(None, None);
                self.token_info = None;
                self.request_redraw_with_risky_row_repair();
            }
        }
    }

    fn on_input_slimming(&mut self, event: InputSlimmingEvent) {
        let has_context_update = event.last.tokens_saved > 0 && event.last.replacements > 0;
        let has_billing_update =
            event.last.saved_usd_micros.is_some() || event.total.saved_usd_micros.is_some();

        if !has_context_update && !has_billing_update {
            return;
        }

        let previous = self.input_slimming.clone();
        if has_context_update {
            self.input_slimming = Some(InputSlimmingPanelSnapshot {
                scope: event.scope,
                last_before_tokens: event.last.tokens_before,
                last_after_tokens: event.last.tokens_after,
                last_saved_tokens: event.last.tokens_saved,
                last_saved_usd_micros: event.last.saved_usd_micros,
                total_saved_tokens: event.total.tokens_saved,
                total_saved_usd_micros: event.total.saved_usd_micros,
            });
        } else {
            let snapshot = self
                .input_slimming
                .get_or_insert(InputSlimmingPanelSnapshot {
                    scope: event.scope,
                    last_before_tokens: event.last.tokens_before,
                    last_after_tokens: event.last.tokens_after,
                    last_saved_tokens: event.last.tokens_saved,
                    last_saved_usd_micros: None,
                    total_saved_tokens: event.total.tokens_saved,
                    total_saved_usd_micros: None,
                });
            snapshot.last_saved_usd_micros = event.last.saved_usd_micros;
            snapshot.total_saved_usd_micros = event.total.saved_usd_micros;
        }
        if self.input_slimming != previous {
            self.request_redraw_with_risky_row_repair();
        }
    }

    fn apply_token_info(&mut self, info: TokenUsageInfo) {
        if self.token_info.as_ref() == Some(&info) {
            return;
        }
        let percent = self.context_remaining_percent(&info);
        let used_tokens = self.context_used_tokens(&info, percent.is_some());
        self.bottom_pane.set_context_window(percent, used_tokens);
        self.token_info = Some(info);
        self.request_redraw_with_risky_row_repair();
    }

    fn context_remaining_percent(&self, info: &TokenUsageInfo) -> Option<i64> {
        info.model_context_window.map(|window| {
            info.last_token_usage
                .percent_of_context_window_remaining(window)
        })
    }

    fn context_used_tokens(&self, info: &TokenUsageInfo, percent_known: bool) -> Option<i64> {
        if percent_known {
            return None;
        }

        Some(info.total_token_usage.tokens_in_context_window())
    }

    fn restore_pre_review_token_info(&mut self) {
        if let Some(saved) = self.pre_review_token_info.take() {
            match saved {
                Some(info) => self.apply_token_info(info),
                None => {
                    self.bottom_pane.set_context_window(None, None);
                    self.token_info = None;
                }
            }
        }
    }

    /// Finalize any active exec as failed and stop/clear agent-turn UI state.
    ///
    /// This does not clear MCP startup tracking, because MCP startup can overlap with turn cleanup
    /// and should continue to drive the bottom-pane running indicator while it is in progress.
    fn finalize_turn(&mut self) {
        // Ensure any spinner is replaced by a red ✗ and flushed into history.
        self.finalize_active_cell_as_failed();
        // Reset running state and clear streaming buffers.
        self.pending_review_elapsed_secs = None;
        self.agent_turn_running = false;
        self.turn_sleep_inhibitor
            .set_turn_running(/* turn_running */ false);
        self.update_task_running_state();
        self.running_commands.clear();
        self.suppressed_exec_calls.clear();
        self.last_unified_wait = None;
        self.unified_exec_wait_streak = None;
        let had_answer_stream = self.stream_controller.take().is_some();
        let had_plan_stream = self.discard_pending_proposed_plan_turn_state();
        if had_answer_stream || had_plan_stream {
            self.app_event_tx.send(AppEvent::StopCommitAnimation);
        }
        self.pending_streamed_agent_message_echo = None;
    }

    fn on_model_cap_error(&mut self, model: String, reset_after_seconds: Option<u64>) {
        self.finalize_turn();

        let mut message = format!("Model {model} is at capacity. Please try a different model.");
        if let Some(seconds) = reset_after_seconds {
            message.push_str(&format!(
                " Try again in {}.",
                format_duration_short(seconds)
            ));
        } else {
            message.push_str(" Try again later.");
        }

        self.add_to_history(history_cell::new_warning_event(message));
        self.request_redraw();
        self.maybe_send_next_queued_input();
    }

    fn on_error(&mut self, message: String) {
        self.finalize_turn();
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();

        // After an error ends the turn, try sending the next queued input.
        self.maybe_send_next_queued_input();
    }

    fn on_warning(&mut self, message: impl Into<String>) {
        self.add_to_history(history_cell::new_warning_event(message.into()));
        self.request_redraw();
    }

    fn on_mcp_startup_update(&mut self, ev: McpStartupUpdateEvent) {
        let mut status = self.mcp_startup_status.take().unwrap_or_default();
        if let McpStartupStatus::Failed { error } = &ev.status {
            self.on_warning(error);
        }
        status.insert(ev.server, ev.status);
        self.mcp_startup_status = Some(status);
        self.update_task_running_state();
        if let Some(current) = &self.mcp_startup_status {
            let total = current.len();
            let mut starting: Vec<_> = current
                .iter()
                .filter_map(|(name, state)| {
                    if matches!(state, McpStartupStatus::Starting) {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect();
            starting.sort();
            if let Some(first) = starting.first() {
                let completed = total.saturating_sub(starting.len());
                let max_to_show = 3;
                let mut to_show: Vec<String> = starting
                    .iter()
                    .take(max_to_show)
                    .map(ToString::to_string)
                    .collect();
                if starting.len() > max_to_show {
                    to_show.push("…".to_string());
                }
                let header = if total > 1 {
                    format!(
                        "Starting MCP servers ({completed}/{total}): {}",
                        to_show.join(", ")
                    )
                } else {
                    format!("Booting MCP server: {first}")
                };
                self.set_status_header(header);
            }
        }
        self.request_redraw();
    }

    fn on_mcp_startup_complete(&mut self, ev: McpStartupCompleteEvent) {
        let mut parts = Vec::new();
        if !ev.failed.is_empty() {
            let failed_servers: Vec<_> = ev.failed.iter().map(|f| f.server.clone()).collect();
            parts.push(format!("failed: {}", failed_servers.join(", ")));
        }
        if !ev.cancelled.is_empty() {
            self.on_warning(format!(
                "MCP startup interrupted. The following servers were not initialized: {}",
                ev.cancelled.join(", ")
            ));
        }
        if !parts.is_empty() {
            self.on_warning(format!("MCP startup incomplete ({})", parts.join("; ")));
        }

        self.mcp_startup_status = None;
        self.update_task_running_state();
        self.maybe_send_next_queued_input();
        self.request_redraw();
    }

    /// Handle a turn aborted due to user interrupt (Esc).
    /// When there are queued user messages, restore them into the composer
    /// separated by newlines rather than auto‑submitting the next one.
    fn on_interrupted_turn(&mut self, reason: TurnAbortReason) {
        // Finalize, log a gentle prompt, and clear running state.
        self.finalize_turn();

        if reason != TurnAbortReason::ReviewEnded {
            self.add_to_history(history_cell::new_error_event(
                "Conversation interrupted - tell the model what to do differently. Something went wrong? Hit `/feedback` to report the issue.".to_owned(),
            ));
        }

        if let Some(combined) = self.drain_queued_messages_for_restore() {
            let combined_local_image_paths = combined
                .local_images
                .iter()
                .map(|img| img.path.clone())
                .collect();
            self.bottom_pane.set_composer_text(
                combined.text,
                combined.text_elements,
                combined_local_image_paths,
            );
            self.refresh_queued_user_messages();
        }

        self.request_redraw();
    }

    /// Merge queued drafts (plus the current composer state) into a single message for restore.
    ///
    /// Each queued draft numbers attachments from `[Image #1]`. When we concatenate drafts, we
    /// must renumber placeholders in a stable order so the merged attachment list stays aligned
    /// with the labels embedded in text. This helper drains the queue, remaps placeholders, and
    /// fixes text element byte ranges as content is appended. Returns `None` when there is nothing
    /// to restore.
    fn drain_queued_messages_for_restore(&mut self) -> Option<UserMessage> {
        if self.queued_user_messages.is_empty() {
            return None;
        }

        let existing_message = UserMessage {
            text: self.bottom_pane.composer_text(),
            text_elements: self.bottom_pane.composer_text_elements(),
            local_images: self.bottom_pane.composer_local_images(),
            mention_paths: HashMap::new(),
        };

        let mut to_merge: Vec<UserMessage> = self.queued_user_messages.drain(..).collect();
        if !existing_message.text.is_empty() || !existing_message.local_images.is_empty() {
            to_merge.push(existing_message);
        }

        let mut combined = UserMessage {
            text: String::new(),
            text_elements: Vec::new(),
            local_images: Vec::new(),
            mention_paths: HashMap::new(),
        };
        let mut combined_offset = 0usize;
        let mut next_image_label = 1usize;

        for (idx, message) in to_merge.into_iter().enumerate() {
            if idx > 0 {
                combined.text.push('\n');
                combined_offset += 1;
            }
            let message = remap_placeholders_for_message(message, &mut next_image_label);
            let base = combined_offset;
            combined.text.push_str(&message.text);
            combined_offset += message.text.len();
            combined
                .text_elements
                .extend(message.text_elements.into_iter().map(|mut elem| {
                    elem.byte_range.start += base;
                    elem.byte_range.end += base;
                    elem
                }));
            combined.local_images.extend(message.local_images);
            combined.mention_paths.extend(message.mention_paths);
        }

        Some(combined)
    }

    fn on_plan_update(&mut self, update: UpdatePlanArgs) {
        self.saw_plan_update_this_turn = true;
        self.latest_update_plan = Some(update.clone());
        self.add_to_history(history_cell::new_plan_update(update));
    }

    fn track_loaded_skills_from_inputs(&mut self, items: &[UserInput]) {
        for item in items {
            let UserInput::Skill { name, path } = item else {
                continue;
            };
            let dedupe_path = dunce::canonicalize(path).unwrap_or_else(|_| path.clone());
            if self
                .loaded_skills
                .iter()
                .any(|skill| skill.path == dedupe_path)
            {
                continue;
            }
            self.loaded_skills.push(LoadedSkillSummary {
                name: name.clone(),
                path: dedupe_path,
            });
        }
    }

    fn on_exec_approval_request(&mut self, id: String, ev: ExecApprovalRequestEvent) {
        self.handle_blocking_prompt(|s| s.handle_exec_approval_now(id, ev));
    }

    fn on_apply_patch_approval_request(&mut self, id: String, ev: ApplyPatchApprovalRequestEvent) {
        self.handle_blocking_prompt(|s| s.handle_apply_patch_approval_now(id, ev));
    }

    fn on_elicitation_request(&mut self, ev: ElicitationRequestEvent) {
        self.handle_blocking_prompt(|s| s.handle_elicitation_request_now(ev));
    }

    fn on_request_user_input(&mut self, ev: RequestUserInputEvent) {
        self.handle_blocking_prompt(|s| s.handle_request_user_input_now(ev));
    }

    fn on_exec_command_begin(&mut self, ev: ExecCommandBeginEvent) {
        self.flush_answer_stream_with_separator();
        if is_unified_exec_source(ev.source) {
            self.track_unified_exec_process_begin(&ev);
            if !is_standard_tool_call(&ev.parsed_cmd) {
                return;
            }
        }
        let ev2 = ev.clone();
        self.defer_or_handle_exec(|q| q.push_exec_begin(ev), |s| s.handle_exec_begin_now(ev2));
    }

    fn on_exec_command_output_delta(&mut self, ev: ExecCommandOutputDeltaEvent) {
        self.track_unified_exec_output_chunk(&ev.call_id, &ev.chunk);

        let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|c| c.as_any_mut().downcast_mut::<ExecCell>())
        else {
            return;
        };

        if cell.append_output(&ev.call_id, std::str::from_utf8(&ev.chunk).unwrap_or("")) {
            self.bump_active_cell_revision();
            self.request_redraw();
        }
    }

    fn on_terminal_interaction(&mut self, ev: TerminalInteractionEvent) {
        if !self.bottom_pane.is_task_running() {
            return;
        }
        self.flush_answer_stream_with_separator();
        self.promote_unified_exec_process(&ev.process_id);
        let command_display = self
            .unified_exec_processes
            .iter()
            .find(|process| process.key == ev.process_id)
            .map(|process| process.command_display.clone());
        if ev.stdin.is_empty() {
            // Empty stdin means we are polling for background output.
            // Surface this in the status indicator (single "waiting" surface) instead of
            // the transcript. Keep the header short so the interrupt hint remains visible.
            self.bottom_pane.ensure_status_indicator();
            self.bottom_pane.set_interrupt_hint_visible(true);
            self.set_status(
                "Waiting for background terminal".to_string(),
                command_display.clone(),
                StatusDetailsCapitalization::Preserve,
                1,
            );
            match &mut self.unified_exec_wait_streak {
                Some(wait) if wait.process_id == ev.process_id => {
                    wait.update_command_display(command_display);
                }
                Some(_) => {
                    self.flush_unified_exec_wait_streak();
                    self.unified_exec_wait_streak =
                        Some(UnifiedExecWaitStreak::new(ev.process_id, command_display));
                }
                None => {
                    self.unified_exec_wait_streak =
                        Some(UnifiedExecWaitStreak::new(ev.process_id, command_display));
                }
            }
            self.request_redraw();
        } else {
            if self
                .unified_exec_wait_streak
                .as_ref()
                .is_some_and(|wait| wait.process_id == ev.process_id)
            {
                self.flush_unified_exec_wait_streak();
            }
            self.add_to_history(history_cell::new_unified_exec_interaction(
                command_display,
                ev.stdin,
            ));
        }
    }

    fn on_patch_apply_begin(&mut self, event: PatchApplyBeginEvent) {
        self.add_to_history(history_cell::new_patch_event(
            event.changes,
            &self.config.cwd,
        ));
    }

    fn on_view_image_tool_call(&mut self, event: ViewImageToolCallEvent) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_view_image_tool_call(
            event.path,
            &self.config.cwd,
        ));
        self.request_redraw();
    }

    fn on_patch_apply_end(&mut self, event: crate::product::agent::protocol::PatchApplyEndEvent) {
        let ev2 = event.clone();
        self.defer_or_handle(
            |q| q.push_patch_end(event),
            |s| s.handle_patch_apply_end_now(ev2),
        );
    }

    fn on_exec_command_end(&mut self, ev: ExecCommandEndEvent) {
        if is_unified_exec_source(ev.source) {
            if let Some(process_id) = ev.process_id.as_deref()
                && self
                    .unified_exec_wait_streak
                    .as_ref()
                    .is_some_and(|wait| wait.process_id == process_id)
            {
                self.flush_unified_exec_wait_streak();
            }
            self.track_unified_exec_process_end(&ev);
            if !self.bottom_pane.is_task_running() {
                return;
            }
        }
        let ev2 = ev.clone();
        self.defer_or_handle_exec(|q| q.push_exec_end(ev), |s| s.handle_exec_end_now(ev2));
    }

    fn track_unified_exec_process_begin(&mut self, ev: &ExecCommandBeginEvent) {
        if ev.source != ExecCommandSource::UnifiedExecStartup {
            return;
        }
        let key = ev.process_id.clone().unwrap_or(ev.call_id.to_string());
        let command_display = strip_bash_lc_and_escape(&ev.command);
        if let Some(existing) = self
            .unified_exec_processes
            .iter_mut()
            .find(|process| process.key == key)
        {
            existing.call_id = ev.call_id.clone();
            existing.command_display = command_display;
            existing.started_at = Instant::now();
            existing.visible = false;
            existing.recent_chunks.clear();
        } else {
            self.unified_exec_processes.push(UnifiedExecProcessSummary {
                key,
                call_id: ev.call_id.clone(),
                command_display,
                started_at: Instant::now(),
                visible: false,
                recent_chunks: Vec::new(),
            });
        }
        self.frame_requester
            .schedule_frame_in(UNIFIED_EXEC_VISIBILITY_DELAY);
    }

    fn track_unified_exec_process_end(&mut self, ev: &ExecCommandEndEvent) {
        let key = ev.process_id.clone().unwrap_or(ev.call_id.to_string());
        let was_visible = self
            .unified_exec_processes
            .iter()
            .any(|process| process.key == key && process.visible);
        self.unified_exec_processes
            .retain(|process| process.key != key);
        if was_visible {
            self.sync_unified_exec_footer();
        }
    }

    fn sync_unified_exec_footer(&mut self) {
        let processes = self
            .visible_unified_exec_processes()
            .map(|process| process.command_display.clone())
            .collect();
        self.bottom_pane.set_unified_exec_processes(processes);
    }

    fn visible_unified_exec_processes(&self) -> impl Iterator<Item = &UnifiedExecProcessSummary> {
        self.unified_exec_processes
            .iter()
            .filter(|process| process.visible)
    }

    fn promote_unified_exec_process(&mut self, process_id: &str) {
        let Some(process) = self
            .unified_exec_processes
            .iter_mut()
            .find(|process| process.key == process_id)
        else {
            return;
        };
        if process.visible {
            return;
        }
        process.visible = true;
        self.sync_unified_exec_footer();
    }

    fn promote_due_unified_exec_processes(&mut self) {
        let now = Instant::now();
        let mut changed = false;
        for process in &mut self.unified_exec_processes {
            if !process.visible
                && now.saturating_duration_since(process.started_at)
                    >= UNIFIED_EXEC_VISIBILITY_DELAY
            {
                process.visible = true;
                changed = true;
            }
        }
        if changed {
            self.sync_unified_exec_footer();
        }
    }

    /// Record recent stdout/stderr lines for the unified exec footer.
    fn track_unified_exec_output_chunk(&mut self, call_id: &str, chunk: &[u8]) {
        let Some(process) = self
            .unified_exec_processes
            .iter_mut()
            .find(|process| process.call_id == call_id)
        else {
            return;
        };

        let text = String::from_utf8_lossy(chunk);
        for line in text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
        {
            process.recent_chunks.push(line.to_string());
        }

        const MAX_RECENT_CHUNKS: usize = 3;
        if process.recent_chunks.len() > MAX_RECENT_CHUNKS {
            let drop_count = process.recent_chunks.len() - MAX_RECENT_CHUNKS;
            process.recent_chunks.drain(0..drop_count);
        }
    }

    fn clear_unified_exec_processes(&mut self) {
        if self.unified_exec_processes.is_empty() {
            return;
        }
        self.unified_exec_processes.clear();
        self.sync_unified_exec_footer();
    }

    pub(crate) fn prepare_for_draw(&mut self) {
        self.promote_due_unified_exec_processes();
    }

    fn on_mcp_tool_call_begin(&mut self, ev: McpToolCallBeginEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_mcp_begin(ev), |s| s.handle_mcp_begin_now(ev2));
    }

    fn on_mcp_tool_call_end(&mut self, ev: McpToolCallEndEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(|q| q.push_mcp_end(ev), |s| s.handle_mcp_end_now(ev2));
    }

    fn on_web_search_begin(&mut self, ev: WebSearchBeginEvent) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.active_cell = Some(Box::new(history_cell::new_active_web_search_call(
            ev.call_id,
            String::new(),
            self.config.animations,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    fn on_web_search_end(&mut self, ev: WebSearchEndEvent) {
        self.flush_answer_stream_with_separator();
        let WebSearchEndEvent {
            call_id,
            query,
            action,
        } = ev;
        let mut handled = false;
        if let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<WebSearchCell>())
            && cell.call_id() == call_id
        {
            cell.update(action.clone(), query.clone());
            cell.complete();
            self.bump_active_cell_revision();
            self.flush_active_cell();
            handled = true;
        }

        if !handled {
            self.add_to_history(history_cell::new_web_search_call(call_id, query, action));
        }
        self.had_work_activity = true;
    }

    fn on_get_history_entry_response(
        &mut self,
        event: crate::product::agent::protocol::GetHistoryEntryResponseEvent,
    ) {
        let crate::product::agent::protocol::GetHistoryEntryResponseEvent {
            offset,
            log_id,
            entry,
        } = event;
        self.bottom_pane
            .on_history_entry_response(log_id, offset, entry.map(|e| e.text));
    }

    fn on_shutdown_complete(&mut self) {
        self.clear_cli_agent_jobs();
        self.agent_shutdown_complete = true;
    }

    fn on_turn_diff(&mut self, unified_diff: String) {
        debug!("TurnDiffEvent: {unified_diff}");
        let mut changed = false;
        for path in paths_from_unified_diff(&unified_diff) {
            if self.changed_files.iter().any(|existing| existing == &path) {
                continue;
            }
            self.changed_files.push_back(path);
            changed = true;
        }
        if changed {
            self.request_redraw_with_risky_row_repair();
        }
    }

    fn on_deprecation_notice(&mut self, event: DeprecationNoticeEvent) {
        let DeprecationNoticeEvent { summary, details } = event;
        self.add_to_history(history_cell::new_deprecation_notice(summary, details));
        self.request_redraw();
    }

    fn on_background_event(&mut self, message: String) {
        debug!("BackgroundEvent: {message}");
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane.set_interrupt_hint_visible(true);
        self.set_status_header(message);
    }

    fn on_undo_started(&mut self, event: UndoStartedEvent) {
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane.set_interrupt_hint_visible(false);
        let message = event
            .message
            .unwrap_or_else(|| "Undo in progress...".to_string());
        self.set_status_header(message);
    }

    fn on_undo_completed(&mut self, event: UndoCompletedEvent) {
        let UndoCompletedEvent { success, message } = event;
        self.bottom_pane.hide_status_indicator();
        let message = message.unwrap_or_else(|| {
            if success {
                "Undo completed successfully.".to_string()
            } else {
                "Undo failed.".to_string()
            }
        });
        if success {
            self.add_info_message(message, None);
        } else {
            self.add_error_message(message);
        }
    }

    fn on_stream_error(&mut self, message: String, additional_details: Option<String>) {
        if self.retry_status.is_none() {
            self.retry_status = Some(self.current_status.clone());
        }
        self.bottom_pane.ensure_status_indicator();
        self.set_status(
            message,
            additional_details,
            StatusDetailsCapitalization::CapitalizeFirst,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
    }

    /// Periodic tick to commit at most one queued line to history with a small delay,
    /// animating the output.
    pub(crate) fn on_commit_tick(&mut self) {
        let mut has_controller = false;
        let mut all_idle = true;
        if let Some(controller) = self.stream_controller.as_mut() {
            has_controller = true;
            let (advanced, is_idle) = controller.on_commit_tick();
            let visible_lines = controller.visible_rendered_lines();
            if advanced {
                self.bottom_pane.suspend_status_indicator();
                self.set_answer_stream_visible_lines(visible_lines);
            }
            all_idle &= is_idle;
        }
        if let Some(controller) = self.plan_stream_controller.as_mut() {
            has_controller = true;
            let (advanced, is_idle) = controller.on_commit_tick();
            if advanced {
                self.bottom_pane.suspend_status_indicator();
                self.bump_plan_stream_revision();
                self.request_redraw();
            }
            all_idle &= is_idle;
        }
        if has_controller && all_idle {
            self.app_event_tx.send(AppEvent::StopCommitAnimation);
        }
    }

    fn flush_interrupt_queue(&mut self) {
        let mut mgr = std::mem::take(&mut self.interrupts);
        mgr.flush_all(self);
        self.interrupts = mgr;
    }

    fn handle_blocking_prompt(&mut self, handle: impl FnOnce(&mut Self)) {
        self.flush_answer_stream_for_blocking_prompt();
        self.flush_pending_proposed_plan();
        if !self.interrupts.is_empty() {
            self.flush_interrupt_queue();
        }
        handle(self);
    }

    #[inline]
    fn defer_or_handle(
        &mut self,
        push: impl FnOnce(&mut InterruptManager),
        handle: impl FnOnce(&mut Self),
    ) {
        // Preserve deterministic FIFO across queued interrupts: once anything
        // is queued due to an active write cycle, continue queueing until the
        // queue is flushed to avoid reordering (e.g., ExecEnd before ExecBegin).
        if self.stream_controller.is_some() || !self.interrupts.is_empty() {
            push(&mut self.interrupts);
        } else {
            handle(self);
        }
    }

    #[inline]
    fn defer_or_handle_exec(
        &mut self,
        push: impl FnOnce(&mut InterruptManager),
        handle: impl FnOnce(&mut Self),
    ) {
        if self.interrupts.is_empty() {
            handle(self);
        } else {
            push(&mut self.interrupts);
        }
    }

    fn handle_stream_finished(&mut self) {
        if self.task_complete_pending {
            self.bottom_pane.hide_status_indicator();
            self.task_complete_pending = false;
        }
        // A completed stream indicates non-exec content was just inserted.
        self.flush_interrupt_queue();
    }

    #[inline]
    fn handle_streaming_delta(&mut self, delta: String) {
        if !delta.is_empty() {
            self.answer_stream_started_this_turn = true;
        }

        // Before streaming agent content, flush any active exec cell group.
        self.flush_unified_exec_wait_streak();
        if !self.active_cell_is_answer_stream() {
            self.flush_active_cell();
        }

        if self.stream_controller.is_none() {
            // If the previous turn inserted non-stream history (exec output, patch status, MCP
            // calls), render a separator before starting the next streamed assistant message.
            if self.needs_final_message_separator && self.had_work_activity {
                let elapsed_seconds = self
                    .bottom_pane
                    .status_widget()
                    .map(super::status_indicator_widget::StatusIndicatorWidget::elapsed_seconds)
                    .map(|current| self.worked_elapsed_from(current));
                self.add_to_history(history_cell::FinalMessageSeparator::new(
                    elapsed_seconds,
                    None,
                ));
                self.needs_final_message_separator = false;
                self.had_work_activity = false;
            } else if self.needs_final_message_separator {
                // Reset the flag even if we don't show separator (no work was done)
                self.needs_final_message_separator = false;
            }
            self.stream_controller = Some(AgentMarkdownStreamController::new());
        }
        let stream_width = self.last_rendered_width.get().map(|w| w.saturating_sub(2));
        let should_start_animation = self
            .stream_controller
            .as_mut()
            .is_some_and(|controller| controller.push(&delta, stream_width));
        self.sync_answer_stream_active_cell_from_controller();
        if should_start_animation {
            self.app_event_tx.send(AppEvent::StartCommitAnimation);
            self.on_commit_tick();
        }
        self.request_redraw();
    }

    fn worked_elapsed_from(&mut self, current_elapsed: u64) -> u64 {
        let baseline = match self.last_separator_elapsed_secs {
            Some(last) if current_elapsed < last => 0,
            Some(last) => last,
            None => 0,
        };
        let elapsed = current_elapsed.saturating_sub(baseline);
        self.last_separator_elapsed_secs = Some(current_elapsed);
        elapsed
    }

    pub(crate) fn handle_exec_end_now(&mut self, ev: ExecCommandEndEvent) {
        let running = self.running_commands.remove(&ev.call_id);
        if self.suppressed_exec_calls.remove(&ev.call_id) {
            return;
        }
        let (command, parsed, source) = match running {
            Some(rc) => (rc.command, rc.parsed_cmd, rc.source),
            None => (ev.command.clone(), ev.parsed_cmd.clone(), ev.source),
        };
        let is_unified_exec_interaction =
            matches!(source, ExecCommandSource::UnifiedExecInteraction);

        let needs_new = self
            .active_cell
            .as_ref()
            .map(|cell| cell.as_any().downcast_ref::<ExecCell>().is_none())
            .unwrap_or(true);
        if needs_new {
            self.flush_active_cell();
            self.active_cell = Some(Box::new(new_active_exec_command(
                ev.call_id.clone(),
                command,
                parsed,
                source,
                ev.interaction_input.clone(),
                self.config.animations,
            )));
        }

        if let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|c| c.as_any_mut().downcast_mut::<ExecCell>())
        {
            let output = if is_unified_exec_interaction {
                CommandOutput {
                    exit_code: ev.exit_code,
                    formatted_output: String::new(),
                    aggregated_output: String::new(),
                }
            } else {
                CommandOutput {
                    exit_code: ev.exit_code,
                    formatted_output: ev.formatted_output.clone(),
                    aggregated_output: ev.aggregated_output.clone(),
                }
            };
            cell.complete_call(&ev.call_id, output, ev.duration);
            if cell.should_flush() {
                self.flush_active_cell();
            } else {
                self.bump_active_cell_revision();
                self.request_redraw();
            }
        }
        // Mark that actual work was done (command executed)
        self.had_work_activity = true;
    }

    pub(crate) fn handle_patch_apply_end_now(
        &mut self,
        event: crate::product::agent::protocol::PatchApplyEndEvent,
    ) {
        // If the patch was successful, just let the "Edited" block stand.
        // Otherwise, add a failure block.
        if !event.success {
            self.add_to_history(history_cell::new_patch_apply_failure(event.stderr));
        }
        // Mark that actual work was done (patch applied)
        self.had_work_activity = true;
    }

    pub(crate) fn handle_exec_approval_now(&mut self, id: String, ev: ExecApprovalRequestEvent) {
        self.flush_answer_stream_with_separator();
        let command = shlex::try_join(ev.command.iter().map(String::as_str))
            .unwrap_or_else(|_| ev.command.join(" "));
        self.notify(Notification::ExecApprovalRequested { command });

        let request = ApprovalRequest::Exec {
            id,
            command: ev.command,
            reason: ev.reason,
            proposed_execpolicy_amendment: ev.proposed_execpolicy_amendment,
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub(crate) fn handle_apply_patch_approval_now(
        &mut self,
        id: String,
        ev: ApplyPatchApprovalRequestEvent,
    ) {
        self.flush_answer_stream_with_separator();

        let request = ApprovalRequest::ApplyPatch {
            id,
            reason: ev.reason,
            changes: ev.changes.clone(),
            cwd: self.config.cwd.clone(),
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
        self.notify(Notification::EditApprovalRequested {
            cwd: self.config.cwd.clone(),
            changes: ev.changes.keys().cloned().collect(),
        });
    }

    pub(crate) fn handle_elicitation_request_now(&mut self, ev: ElicitationRequestEvent) {
        self.flush_answer_stream_with_separator();

        self.notify(Notification::ElicitationRequested {
            server_name: ev.server_name.clone(),
        });

        let request = ApprovalRequest::McpElicitation {
            server_name: ev.server_name,
            request_id: ev.id,
            message: ev.message,
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub(crate) fn handle_request_user_input_now(&mut self, ev: RequestUserInputEvent) {
        self.flush_answer_stream_with_separator();
        self.bottom_pane.push_user_input_request(ev);
        self.request_redraw();
    }

    pub(crate) fn handle_exec_begin_now(&mut self, ev: ExecCommandBeginEvent) {
        // Ensure the status indicator is visible while the command runs.
        self.running_commands.insert(
            ev.call_id.clone(),
            RunningCommand {
                command: ev.command.clone(),
                parsed_cmd: ev.parsed_cmd.clone(),
                source: ev.source,
            },
        );
        let is_wait_interaction = matches!(ev.source, ExecCommandSource::UnifiedExecInteraction)
            && ev
                .interaction_input
                .as_deref()
                .map(str::is_empty)
                .unwrap_or(true);
        let command_display = ev.command.join(" ");
        let should_suppress_unified_wait = is_wait_interaction
            && self
                .last_unified_wait
                .as_ref()
                .is_some_and(|wait| wait.is_duplicate(&command_display));
        if is_wait_interaction {
            self.last_unified_wait = Some(UnifiedExecWaitState::new(command_display));
        } else {
            self.last_unified_wait = None;
        }
        if should_suppress_unified_wait {
            self.suppressed_exec_calls.insert(ev.call_id);
            return;
        }
        let interaction_input = ev.interaction_input.clone();
        if let Some(cell) = self
            .active_cell
            .as_mut()
            .and_then(|c| c.as_any_mut().downcast_mut::<ExecCell>())
            && let Some(new_exec) = cell.with_added_call(
                ev.call_id.clone(),
                ev.command.clone(),
                ev.parsed_cmd.clone(),
                ev.source,
                interaction_input.clone(),
            )
        {
            *cell = new_exec;
            self.bump_active_cell_revision();
        } else {
            self.flush_active_cell();

            self.active_cell = Some(Box::new(new_active_exec_command(
                ev.call_id.clone(),
                ev.command.clone(),
                ev.parsed_cmd,
                ev.source,
                interaction_input,
                self.config.animations,
            )));
            self.bump_active_cell_revision();
        }

        self.request_redraw();
    }

    pub(crate) fn handle_mcp_begin_now(&mut self, ev: McpToolCallBeginEvent) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.active_cell = Some(Box::new(history_cell::new_active_mcp_tool_call(
            ev.call_id,
            ev.invocation,
            self.config.animations,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }
    pub(crate) fn handle_mcp_end_now(&mut self, ev: McpToolCallEndEvent) {
        self.flush_answer_stream_with_separator();

        let McpToolCallEndEvent {
            call_id,
            invocation,
            duration,
            result,
        } = ev;

        let extra_cell = match self
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<McpToolCallCell>())
        {
            Some(cell) if cell.call_id() == call_id => cell.complete(duration, result),
            _ => {
                self.flush_active_cell();
                let mut cell = history_cell::new_active_mcp_tool_call(
                    call_id,
                    invocation,
                    self.config.animations,
                );
                let extra_cell = cell.complete(duration, result);
                self.active_cell = Some(Box::new(cell));
                extra_cell
            }
        };

        self.flush_active_cell();
        if let Some(extra) = extra_cell {
            self.add_boxed_history(extra);
        }
        // Mark that actual work was done (MCP tool call)
        self.had_work_activity = true;
    }

    pub(crate) fn new(common: ChatWidgetInit) -> Self {
        let ChatWidgetInit {
            config,
            thread_manager,
            frame_requester,
            app_event_tx,
            initial_user_message,
            enhanced_keys_supported,
            auth_manager,
            feedback,
            is_first_run,
            startup,
            otel_manager,
        } = common;
        let app_event_tx = app_event_tx.bind_history_to_widget();
        let auto_open_provider_config = matches!(
            startup,
            ChatWidgetStartup::NeedsProviderConfig { auto_open: true }
        );
        let (model, needs_provider_config, defer_startup) = match startup {
            ChatWidgetStartup::Configured { model } => (model, false, false),
            ChatWidgetStartup::NeedsProviderConfig { .. } => (None, true, false),
            ChatWidgetStartup::Deferred => (config.model.clone(), false, true),
        };
        let model = model.filter(|m| !m.trim().is_empty());
        let mut config = config;
        config.model = model.clone();
        let mut rng = rand::rng();
        let placeholder = PLACEHOLDERS[rng.random_range(0..PLACEHOLDERS.len())].to_string();
        let (codex_op_tx, _) = unbounded_channel::<Op>();
        let codex_op_tx = if needs_provider_config || defer_startup {
            codex_op_tx
        } else {
            spawn_agent(
                config.clone(),
                app_event_tx.clone(),
                Arc::clone(&thread_manager),
            )
        };

        let model_override = model.as_deref();
        let model_for_header = model.clone().unwrap_or_else(|| {
            if needs_provider_config {
                "No provider configured".to_string()
            } else {
                DEFAULT_MODEL_DISPLAY_NAME.to_string()
            }
        });
        let active_identity_mask =
            Self::initial_identity_mask(&config, thread_manager.as_ref(), model_override);
        let header_model = active_identity_mask
            .as_ref()
            .and_then(|mask| mask.model.clone())
            .unwrap_or_else(|| model_for_header.clone());
        let current_identity =
            Self::initial_custom_mode(header_model.clone(), config.model_reasoning_effort);

        let active_cell = Some(Self::placeholder_session_header_cell(&config));
        let prevent_idle_sleep = config.features.enabled(Feature::PreventIdleSleep);

        let mut widget = Self {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            codex_op_tx,
            bottom_pane: BottomPane::new(BottomPaneParams {
                frame_requester,
                app_event_tx,
                has_input_focus: true,
                enhanced_keys_supported,
                placeholder_text: placeholder,
                disable_paste_burst: config.disable_paste_burst,
                animations_enabled: config.animations,
                skills: None,
            }),
            transcript: RefCell::new(TranscriptView::new(
                Vec::new(),
                TranscriptRenderMode::Display,
            )),
            mouse_scroll: MouseScrollState::default(),
            active_cell,
            active_cell_revision: 0,
            config,
            skills_all: Vec::new(),
            skills_have_loaded: false,
            skills_request_in_flight: false,
            skills_refresh_pending: None,
            skills_initial_state: None,
            reasoning_effort_overrides: HashMap::new(),
            current_identity,
            active_identity_mask,
            pending_initial_identity_sync: true,
            pending_existing_thread_model_override: None,
            auth_manager,
            thread_manager,
            otel_manager,
            session_header: SessionHeader::new(header_model),
            initial_user_message,
            token_info: None,
            stream_controller: None,
            pending_streamed_agent_message_echo: None,
            plan_stream_controller: None,
            plan_stream_revision: 0,
            answer_stream_started_this_turn: false,
            running_commands: HashMap::new(),
            suppressed_exec_calls: HashSet::new(),
            last_unified_wait: None,
            unified_exec_wait_streak: None,
            loaded_skills: Vec::new(),
            turn_sleep_inhibitor: SleepInhibitor::new(prevent_idle_sleep),
            task_complete_pending: false,
            unified_exec_processes: Vec::new(),
            changed_files: VecDeque::new(),
            cli_agent_jobs: HashMap::new(),
            agent_turn_running: false,
            mcp_startup_status: None,
            connectors_cache: ConnectorsCacheState::default(),
            interrupts: InterruptManager::new(),
            reasoning_buffer: Vec::new(),
            reasoning_item_states: HashMap::new(),
            pending_live_legacy_reasoning: HashMap::new(),
            last_legacy_reasoning_finalized: None,
            current_status: StatusIndicatorState::working(),
            retry_status: None,
            thread_id: None,
            agent_shutdown_complete: false,
            thread_name: None,
            forked_from: None,
            context_compact_count: 0,
            input_slimming: None,
            counted_context_compaction_item_ids: HashSet::new(),
            pending_live_legacy_context_compactions: HashMap::new(),
            pending_replay_legacy_context_compactions: 0,
            queued_user_messages: VecDeque::new(),
            show_welcome_banner: is_first_run,
            suppress_session_configured_redraw: false,
            pending_notification: None,
            quit_shortcut_expires_at: None,
            quit_shortcut_key: None,
            is_review_mode: false,
            pre_review_token_info: None,
            pending_review_start_transition: false,
            pending_review_elapsed_secs: None,
            needs_final_message_separator: false,
            had_work_activity: false,
            saw_plan_update_this_turn: false,
            saw_plan_item_this_turn: false,
            pending_proposed_plan_rendered_this_turn: false,
            plan_delta_buffer: String::new(),
            plan_item_active: false,
            latest_proposed_plan_title: None,
            latest_proposed_plan_text: None,
            latest_update_plan: None,
            last_separator_elapsed_secs: None,
            last_rendered_width: std::cell::Cell::new(None),
            last_transcript_area: std::cell::Cell::new(None),
            last_bottom_area: std::cell::Cell::new(None),
            feedback,
            current_rollout_path: None,
            current_goal: None,
            current_goal_state_known: false,
            pending_plan_implementation_goal_state_refresh: false,
            pending_plan_implementation_prompt: false,
            external_editor_state: ExternalEditorState::Closed,
            git_branch: None,
        };

        widget
            .bottom_pane
            .set_steer_enabled(widget.config.features.enabled(Feature::Steer));
        widget
            .bottom_pane
            .set_identities_enabled(widget.config.features.enabled(Feature::Identities));
        widget.sync_personality_command_enabled();
        widget.sync_buddy_config_from_config();
        #[cfg(target_os = "windows")]
        widget.bottom_pane.set_windows_degraded_sandbox_active(
            crate::product::agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                && matches!(
                    WindowsSandboxLevel::from_config(&widget.config),
                    WindowsSandboxLevel::RestrictedToken
                ),
        );
        widget.update_identity_indicator();
        widget.update_footer_info();

        widget
            .bottom_pane
            .set_connectors_enabled(widget.config.features.enabled(Feature::Apps));

        if auto_open_provider_config {
            widget.open_provider_popup();
        }

        widget
    }

    /// Create a ChatWidget attached to an existing conversation (e.g., a fork).
    pub(crate) fn new_from_existing(
        common: ChatWidgetInit,
        conversation: std::sync::Arc<crate::product::agent::CodexThread>,
        thread_id: crate::product::protocol::ThreadId,
        session_configured: crate::product::agent::protocol::SessionConfiguredEvent,
    ) -> Self {
        let ChatWidgetInit {
            config,
            thread_manager,
            frame_requester,
            app_event_tx,
            initial_user_message,
            enhanced_keys_supported,
            auth_manager,
            feedback,
            startup,
            otel_manager,
            ..
        } = common;
        let app_event_tx = app_event_tx.bind_history_to_widget();
        let ChatWidgetStartup::Configured { model } = startup else {
            panic!("new_from_existing requires configured startup");
        };
        let model = model.filter(|m| !m.trim().is_empty());
        let mut rng = rand::rng();
        let placeholder = PLACEHOLDERS[rng.random_range(0..PLACEHOLDERS.len())].to_string();

        let header_model = model
            .clone()
            .unwrap_or_else(|| session_configured.model.clone());
        let session_identity_kind = session_configured.identity_kind;
        let active_identity_mask = None;
        let pending_existing_thread_model_override = model;
        let initial_reasoning_effort = session_configured
            .reasoning_effort
            .or(config.model_reasoning_effort);

        let codex_op_tx = attach_existing_thread(
            conversation,
            thread_id,
            session_configured,
            app_event_tx.clone(),
        );

        let mut current_identity =
            Self::initial_custom_mode(header_model.clone(), initial_reasoning_effort);
        current_identity.kind = session_identity_kind;
        let prevent_idle_sleep = config.features.enabled(Feature::PreventIdleSleep);

        let mut widget = Self {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            codex_op_tx,
            bottom_pane: BottomPane::new(BottomPaneParams {
                frame_requester,
                app_event_tx,
                has_input_focus: true,
                enhanced_keys_supported,
                placeholder_text: placeholder,
                disable_paste_burst: config.disable_paste_burst,
                animations_enabled: config.animations,
                skills: None,
            }),
            transcript: RefCell::new(TranscriptView::new(
                Vec::new(),
                TranscriptRenderMode::Display,
            )),
            mouse_scroll: MouseScrollState::default(),
            active_cell: None,
            active_cell_revision: 0,
            config,
            skills_all: Vec::new(),
            skills_have_loaded: false,
            skills_request_in_flight: false,
            skills_refresh_pending: None,
            skills_initial_state: None,
            reasoning_effort_overrides: HashMap::new(),
            current_identity,
            active_identity_mask,
            pending_initial_identity_sync: false,
            pending_existing_thread_model_override,
            auth_manager,
            thread_manager,
            otel_manager,
            session_header: SessionHeader::new(header_model),
            initial_user_message,
            token_info: None,
            stream_controller: None,
            pending_streamed_agent_message_echo: None,
            plan_stream_controller: None,
            plan_stream_revision: 0,
            answer_stream_started_this_turn: false,
            running_commands: HashMap::new(),
            suppressed_exec_calls: HashSet::new(),
            last_unified_wait: None,
            unified_exec_wait_streak: None,
            loaded_skills: Vec::new(),
            turn_sleep_inhibitor: SleepInhibitor::new(prevent_idle_sleep),
            task_complete_pending: false,
            unified_exec_processes: Vec::new(),
            changed_files: VecDeque::new(),
            cli_agent_jobs: HashMap::new(),
            agent_turn_running: false,
            mcp_startup_status: None,
            connectors_cache: ConnectorsCacheState::default(),
            interrupts: InterruptManager::new(),
            reasoning_buffer: Vec::new(),
            reasoning_item_states: HashMap::new(),
            pending_live_legacy_reasoning: HashMap::new(),
            last_legacy_reasoning_finalized: None,
            current_status: StatusIndicatorState::working(),
            retry_status: None,
            thread_id: None,
            agent_shutdown_complete: false,
            thread_name: None,
            forked_from: None,
            context_compact_count: 0,
            input_slimming: None,
            counted_context_compaction_item_ids: HashSet::new(),
            pending_live_legacy_context_compactions: HashMap::new(),
            pending_replay_legacy_context_compactions: 0,
            queued_user_messages: VecDeque::new(),
            show_welcome_banner: false,
            suppress_session_configured_redraw: true,
            pending_notification: None,
            quit_shortcut_expires_at: None,
            quit_shortcut_key: None,
            is_review_mode: false,
            pre_review_token_info: None,
            pending_review_start_transition: false,
            pending_review_elapsed_secs: None,
            needs_final_message_separator: false,
            had_work_activity: false,
            saw_plan_update_this_turn: false,
            saw_plan_item_this_turn: false,
            pending_proposed_plan_rendered_this_turn: false,
            plan_delta_buffer: String::new(),
            plan_item_active: false,
            latest_proposed_plan_title: None,
            latest_proposed_plan_text: None,
            latest_update_plan: None,
            last_separator_elapsed_secs: None,
            last_rendered_width: std::cell::Cell::new(None),
            last_transcript_area: std::cell::Cell::new(None),
            last_bottom_area: std::cell::Cell::new(None),
            feedback,
            current_rollout_path: None,
            current_goal: None,
            current_goal_state_known: false,
            pending_plan_implementation_goal_state_refresh: false,
            pending_plan_implementation_prompt: false,
            external_editor_state: ExternalEditorState::Closed,
            git_branch: None,
        };

        widget
            .bottom_pane
            .set_steer_enabled(widget.config.features.enabled(Feature::Steer));
        widget
            .bottom_pane
            .set_identities_enabled(widget.config.features.enabled(Feature::Identities));
        widget.sync_personality_command_enabled();
        widget.sync_buddy_config_from_config();
        #[cfg(target_os = "windows")]
        widget.bottom_pane.set_windows_degraded_sandbox_active(
            crate::product::agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                && matches!(
                    WindowsSandboxLevel::from_config(&widget.config),
                    WindowsSandboxLevel::RestrictedToken
                ),
        );
        widget.update_identity_indicator();
        widget.update_footer_info();

        widget
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            key if is_copy_shortcut(key) => {
                self.copy_transcript_selection();
                return;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'c') => {
                self.on_ctrl_c();
                self.retry_pending_plan_implementation_prompt();
                return;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'d') => {
                if self.on_ctrl_d() {
                    return;
                }
                self.bottom_pane.clear_quit_shortcut_hint();
                self.quit_shortcut_expires_at = None;
                self.quit_shortcut_key = None;
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            } if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && c.eq_ignore_ascii_case(&'v') =>
            {
                match paste_image_to_temp_png() {
                    Ok((path, info)) => {
                        tracing::debug!(
                            "pasted image size={}x{} format={}",
                            info.width,
                            info.height,
                            info.encoded_format.label()
                        );
                        self.attach_image(path);
                    }
                    Err(err) => {
                        tracing::warn!("failed to paste image: {err}");
                        self.add_to_history(history_cell::new_error_event(format!(
                            "Failed to paste image: {err}",
                        )));
                    }
                }
                return;
            }
            other if other.kind == KeyEventKind::Press => {
                self.bottom_pane.clear_quit_shortcut_hint();
                self.quit_shortcut_expires_at = None;
                self.quit_shortcut_key = None;
            }
            _ => {}
        }

        match key_event {
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.handle_transcript_scroll_key(key_event) => {}
            KeyEvent {
                code: KeyCode::BackTab,
                kind: KeyEventKind::Press,
                ..
            } if self.identities_enabled() => {
                if self.bottom_pane.is_task_running() {
                    self.on_warning("Cannot change identity while a task is running.");
                } else if !self.bottom_pane.no_modal_or_popup_active() {
                    self.on_warning("Cannot change identity while another picker is open.");
                } else {
                    self.app_event_tx.send(AppEvent::OpenIdentityModal);
                    self.request_redraw();
                }
            }
            KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::ALT,
                kind: KeyEventKind::Press,
                ..
            } if !self.queued_user_messages.is_empty() => {
                // Prefer the most recently queued item.
                if let Some(user_message) = self.queued_user_messages.pop_back() {
                    let local_image_paths = user_message
                        .local_images
                        .iter()
                        .map(|img| img.path.clone())
                        .collect();
                    self.bottom_pane.set_composer_text(
                        user_message.text,
                        user_message.text_elements,
                        local_image_paths,
                    );
                    self.refresh_queued_user_messages();
                    self.request_redraw();
                }
            }
            _ => {
                match self.bottom_pane.handle_key_event(key_event) {
                    InputResult::Submitted {
                        text,
                        text_elements,
                    } => {
                        let user_message = UserMessage {
                            text,
                            local_images: self
                                .bottom_pane
                                .take_recent_submission_images_with_placeholders(),
                            text_elements,
                            mention_paths: self.bottom_pane.take_mention_paths(),
                        };
                        if self.is_session_configured() {
                            // Submitted is only emitted when steer is enabled (Enter sends immediately).
                            // Reset any reasoning header only when we are actually submitting a turn.
                            self.clear_reasoning_buffers();
                            self.set_status_header(String::from("Working"));
                            self.submit_user_message(user_message);
                        } else {
                            self.queue_user_message(user_message);
                        }
                    }
                    InputResult::Queued {
                        text,
                        text_elements,
                    } => {
                        let user_message = UserMessage {
                            text,
                            local_images: self
                                .bottom_pane
                                .take_recent_submission_images_with_placeholders(),
                            text_elements,
                            mention_paths: self.bottom_pane.take_mention_paths(),
                        };
                        self.queue_user_message(user_message);
                    }
                    InputResult::Command(cmd) => {
                        self.dispatch_command(cmd);
                    }
                    InputResult::CommandWithArgs(cmd, args) => {
                        self.dispatch_command_with_args(cmd, args);
                    }
                    InputResult::None => {}
                }
                self.retry_pending_plan_implementation_prompt();
            }
        }
    }

    fn copy_transcript_selection(&mut self) {
        self.copy_transcript_selection_with(write_text_to_clipboard);
    }

    fn copy_transcript_selection_with(
        &mut self,
        write_clipboard: impl FnOnce(&str, ClipboardTextConfig) -> Result<(), String>,
    ) {
        let Some(text) = self.transcript.borrow().selected_text() else {
            self.set_status_header("No selection to copy".to_string());
            self.request_redraw();
            return;
        };

        if self.write_selection_to_clipboard_with(&text, write_clipboard) {
            self.transcript.borrow_mut().clear_selection();
        }
        self.request_redraw();
    }

    fn write_selection_to_clipboard_with(
        &mut self,
        text: &str,
        write_clipboard: impl FnOnce(&str, ClipboardTextConfig) -> Result<(), String>,
    ) -> bool {
        match write_clipboard(text, self.clipboard_text_config()) {
            Ok(()) => {
                self.set_status_header("Selection copied".to_string());
                true
            }
            Err(err) => {
                self.set_status_header(format!("Copy failed: {err}"));
                false
            }
        }
    }

    fn copy_completed_transcript_selection(&mut self, text: Option<String>) {
        self.copy_completed_transcript_selection_with(text, write_text_to_clipboard);
    }

    fn copy_completed_transcript_selection_with(
        &mut self,
        text: Option<String>,
        write_clipboard: impl FnOnce(&str, ClipboardTextConfig) -> Result<(), String>,
    ) {
        if let Some(text) = text.filter(|text| !text.is_empty())
            && self.write_selection_to_clipboard_with(&text, write_clipboard)
        {
            self.transcript.borrow_mut().clear_selection();
        }
        self.request_redraw();
    }

    pub(crate) fn clipboard_text_config(&self) -> ClipboardTextConfig {
        ClipboardTextConfig::new(self.config.tui_osc52_tmux_mode)
    }

    pub(crate) fn handle_mouse_event(&mut self, mouse_event: MouseEvent) {
        if !self.bottom_pane.no_modal_or_popup_active()
            && !self.bottom_pane.allow_background_transcript_interaction()
        {
            return;
        }

        let in_transcript = Self::mouse_event_in_area(mouse_event, self.last_transcript_area.get());
        let in_bottom = Self::mouse_event_in_area(mouse_event, self.last_bottom_area.get());
        let transcript_dragging = self.transcript.borrow().selection_dragging();

        if (!in_transcript || in_bottom) && !transcript_dragging {
            if matches!(mouse_event.kind, MouseEventKind::Down(MouseButton::Left))
                && self.transcript.borrow_mut().clear_selection()
            {
                self.request_redraw();
            }
            return;
        }

        let outcome = self
            .transcript
            .borrow_mut()
            .handle_mouse_event(mouse_event, &mut self.mouse_scroll);
        match outcome {
            TranscriptMouseOutcome::Ignored => {}
            TranscriptMouseOutcome::Scrolled | TranscriptMouseOutcome::SelectionChanged => {
                self.request_redraw();
            }
            TranscriptMouseOutcome::SelectionCompleted(text) => {
                self.copy_completed_transcript_selection(text);
            }
        }
    }

    fn mouse_event_in_area(mouse_event: MouseEvent, area: Option<Rect>) -> bool {
        area.is_some_and(|area| {
            mouse_event.column >= area.x
                && mouse_event.column < area.right()
                && mouse_event.row >= area.y
                && mouse_event.row < area.bottom()
        })
    }

    #[cfg(test)]
    pub(crate) fn cached_transcript_area(&self) -> Option<Rect> {
        self.last_transcript_area.get()
    }

    #[cfg(test)]
    pub(crate) fn cached_bottom_area(&self) -> Option<Rect> {
        self.last_bottom_area.get()
    }

    pub(crate) fn attach_image(&mut self, path: PathBuf) {
        tracing::info!("attach_image path={path:?}");
        self.bottom_pane.attach_image(path);
        self.request_redraw();
    }

    pub(crate) fn composer_text_with_pending(&self) -> String {
        self.bottom_pane.composer_text_with_pending()
    }

    pub(crate) fn apply_external_edit(&mut self, text: String) {
        self.bottom_pane.apply_external_edit(text);
        self.request_redraw();
    }

    pub(crate) fn external_editor_state(&self) -> ExternalEditorState {
        self.external_editor_state
    }

    pub(crate) fn set_external_editor_state(&mut self, state: ExternalEditorState) {
        self.external_editor_state = state;
    }

    pub(crate) fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.bottom_pane.set_footer_hint_override(items);
    }

    pub(crate) fn can_launch_external_editor(&self) -> bool {
        self.bottom_pane.can_launch_external_editor()
    }

    fn dispatch_command(&mut self, cmd: SlashCommand) {
        if !cmd.available_during_task() && self.bottom_pane.is_task_running() {
            self.prepare_slash_command_transcript_output();
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.request_redraw();
            return;
        }
        match cmd {
            SlashCommand::Feedback => {
                if !self.config.feedback_enabled {
                    let params = crate::product::tui_app::bottom_pane::feedback_disabled_params();
                    self.bottom_pane.show_selection_view(params);
                    self.request_redraw();
                    return;
                }
                // Step 1: pick a category (UI built in feedback_view)
                let params = crate::product::tui_app::bottom_pane::feedback_selection_params(
                    self.app_event_tx.clone(),
                );
                self.bottom_pane.show_selection_view(params);
                self.request_redraw();
            }
            SlashCommand::New => {
                self.app_event_tx.send(AppEvent::NewSession);
            }
            SlashCommand::Resume => {
                self.app_event_tx.send(AppEvent::OpenResumePicker);
            }
            SlashCommand::Fork => {
                self.app_event_tx.send(AppEvent::ForkCurrentSession);
            }
            SlashCommand::Init => {
                self.prepare_slash_command_transcript_output();
                let init_target = self.config.cwd.join(DEFAULT_PROJECT_DOC_FILENAME);
                if init_target.exists() {
                    let message = format!(
                        "{DEFAULT_PROJECT_DOC_FILENAME} already exists here. Skipping /init to avoid overwriting it."
                    );
                    self.add_info_message(message, None);
                    return;
                }
                const INIT_PROMPT: &str = include_str!("../prompt_for_init_command.md");
                self.submit_user_message(INIT_PROMPT.to_string().into());
            }
            SlashCommand::Compact => {
                self.prepare_slash_command_transcript_output();
                self.clear_token_usage();
                self.app_event_tx.send(AppEvent::CodexOp(Op::Compact));
            }
            SlashCommand::Review => {
                self.app_event_tx.send(AppEvent::OpenReviewModal);
                self.request_redraw();
            }
            SlashCommand::Rename => {
                self.show_rename_prompt();
            }
            SlashCommand::Model => {
                self.open_model_popup();
            }
            SlashCommand::Providers => {
                self.open_provider_popup();
            }
            SlashCommand::Personality => {
                self.open_personality_popup();
            }
            SlashCommand::Identity => {
                if !self.identities_enabled() {
                    self.add_info_message(
                        "Identities are disabled.".to_string(),
                        Some("Enable identities to use /identity.".to_string()),
                    );
                    return;
                }
                self.open_identity_popup();
            }
            SlashCommand::Approvals => {
                self.app_event_tx.send(AppEvent::OpenApprovalsPopup);
                self.request_redraw();
            }
            SlashCommand::Permissions => {
                self.app_event_tx.send(AppEvent::OpenPermissionsPopup);
                self.request_redraw();
            }
            SlashCommand::ElevateSandbox => {
                #[cfg(target_os = "windows")]
                {
                    let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
                    let windows_degraded_sandbox_enabled =
                        matches!(windows_sandbox_level, WindowsSandboxLevel::RestrictedToken);
                    if !windows_degraded_sandbox_enabled
                        || !crate::product::agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                    {
                        // This command should not be visible/recognized outside degraded mode,
                        // but guard anyway in case something dispatches it directly.
                        return;
                    }

                    let Some(preset) = builtin_approval_presets()
                        .into_iter()
                        .find(|preset| preset.id == "auto")
                    else {
                        // Avoid panicking in interactive UI; treat this as a recoverable
                        // internal error.
                        self.add_error_message(
                            "Internal error: missing the 'auto' approval preset.".to_string(),
                        );
                        return;
                    };

                    if let Err(err) = self.config.approval_policy.can_set(&preset.approval) {
                        self.add_error_message(err.to_string());
                        return;
                    }

                    self.otel_manager.counter(
                        "lha.windows_sandbox.setup_elevated_sandbox_command",
                        1,
                        &[],
                    );
                    self.app_event_tx
                        .send(AppEvent::BeginWindowsSandboxElevatedSetup { preset });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = &self.otel_manager;
                    // Not supported; on non-Windows this command should never be reachable.
                };
            }
            SlashCommand::Experimental => {
                self.app_event_tx
                    .send(AppEvent::OpenExperimentalFeaturesModal);
                self.request_redraw();
            }
            SlashCommand::Memories => {
                self.app_event_tx.send(AppEvent::OpenMemoriesSettingsView);
                self.request_redraw();
            }
            SlashCommand::Buddy => {
                self.prepare_slash_command_transcript_output();
                self.show_buddy_status();
            }
            SlashCommand::Quit | SlashCommand::Exit => {
                self.request_quit_without_confirmation();
            }
            SlashCommand::Logout => {
                self.request_quit_without_confirmation();
            }
            // SlashCommand::Undo => {
            //     self.app_event_tx.send(AppEvent::CodexOp(Op::Undo));
            // }
            SlashCommand::Diff => {
                self.add_diff_in_progress();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let text = match get_git_diff().await {
                        Ok((is_git_repo, diff_text)) => {
                            if is_git_repo {
                                diff_text
                            } else {
                                "`/diff` — _not inside a git repository_".to_string()
                            }
                        }
                        Err(e) => format!("Failed to compute diff: {e}"),
                    };
                    tx.send(AppEvent::DiffResult(text));
                });
            }
            SlashCommand::Changelog => {
                self.prepare_slash_command_transcript_output();
                self.app_event_tx.send(AppEvent::RequestChangelog);
            }
            SlashCommand::Mention => {
                self.insert_str("@");
            }
            SlashCommand::Skills => {
                self.app_event_tx.send(AppEvent::OpenSkillsModal);
                self.request_redraw();
            }
            SlashCommand::Status => {
                self.prepare_slash_command_transcript_output();
                self.add_status_output();
            }
            SlashCommand::Plan => {
                if self
                    .transcript
                    .borrow_mut()
                    .scroll_to_latest_proposed_plan()
                {
                    self.request_redraw();
                } else {
                    self.scroll_transcript_to_bottom();
                    self.add_info_message(
                        "No proposed plan found in this session.".to_string(),
                        Some("Ask LHA to create a plan first.".to_string()),
                    );
                }
            }
            SlashCommand::Goal => {
                self.prepare_slash_command_transcript_output();
                if !self.ensure_programmer_goal_allowed() {
                    return;
                }
                self.app_event_tx.send(AppEvent::CodexOp(Op::ThreadGoalGet));
            }
            SlashCommand::Bottom => {
                self.scroll_transcript_to_bottom();
            }
            SlashCommand::Ps => {
                self.prepare_slash_command_transcript_output();
                self.add_ps_output();
            }
            SlashCommand::Stop => {
                self.prepare_slash_command_transcript_output();
                self.clean_background_terminals();
            }
            SlashCommand::Mcp => {
                self.app_event_tx.send(AppEvent::OpenMcpToolsModal);
                self.request_redraw();
            }
            SlashCommand::Rollout => {
                self.prepare_slash_command_transcript_output();
                if let Some(path) = self.rollout_path() {
                    self.add_info_message(
                        format!("Current rollout path: {}", path.display()),
                        None,
                    );
                } else {
                    self.add_info_message("Rollout path is not available yet.".to_string(), None);
                }
            }
            SlashCommand::TestApproval => {
                use crate::product::agent::protocol::EventMsg;
                use std::collections::HashMap;

                use crate::product::agent::protocol::ApplyPatchApprovalRequestEvent;
                use crate::product::agent::protocol::FileChange;

                self.app_event_tx.send(AppEvent::CodexEvent(Event {
                    id: "1".to_string(),
                    // msg: EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                    //     call_id: "1".to_string(),
                    //     command: vec!["git".into(), "apply".into()],
                    //     cwd: self.config.cwd.clone(),
                    //     reason: Some("test".to_string()),
                    // }),
                    msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                        call_id: "1".to_string(),
                        turn_id: "turn-1".to_string(),
                        changes: HashMap::from([
                            (
                                PathBuf::from("/tmp/test.txt"),
                                FileChange::Add {
                                    content: "test".to_string(),
                                },
                            ),
                            (
                                PathBuf::from("/tmp/test2.txt"),
                                FileChange::Update {
                                    unified_diff: "+test\n-test2".to_string(),
                                    move_path: None,
                                },
                            ),
                        ]),
                        reason: None,
                        grant_root: Some(PathBuf::from("/tmp")),
                    }),
                }));
            }
        }
    }

    fn dispatch_command_with_args(&mut self, cmd: SlashCommand, args: String) {
        if !cmd.available_during_task() && self.bottom_pane.is_task_running() {
            self.prepare_slash_command_transcript_output();
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.request_redraw();
            return;
        }

        let trimmed = args.trim();
        match cmd {
            SlashCommand::Rename if !trimmed.is_empty() => {
                self.prepare_slash_command_transcript_output();
                let Some(name) = crate::product::agent::util::normalize_thread_name(trimmed) else {
                    self.add_error_message("Thread name cannot be empty.".to_string());
                    return;
                };
                let cell = Self::rename_confirmation_cell(&name, self.thread_id);
                self.add_boxed_history(Box::new(cell));
                self.request_redraw();
                self.app_event_tx
                    .send(AppEvent::CodexOp(Op::SetThreadName { name }));
            }
            SlashCommand::Identity => {
                let _ = trimmed;
                self.dispatch_command(cmd);
            }
            SlashCommand::Buddy => {
                self.prepare_slash_command_transcript_output();
                self.dispatch_buddy_command(trimmed);
            }
            SlashCommand::Goal => {
                self.prepare_slash_command_transcript_output();
                self.dispatch_goal_command(trimmed);
            }
            SlashCommand::Plan => {
                self.prepare_slash_command_transcript_output();
                self.dispatch_plan_command(trimmed);
            }
            SlashCommand::Review if !trimmed.is_empty() => {
                self.prepare_slash_command_transcript_output();
                self.app_event_tx.send(AppEvent::StartReview {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Custom {
                            instructions: trimmed.to_string(),
                        },
                        user_facing_hint: None,
                    },
                });
            }
            _ => self.dispatch_command(cmd),
        }
    }

    fn dispatch_plan_command(&mut self, trimmed: &str) {
        match trimmed {
            "" => self.dispatch_command(SlashCommand::Plan),
            "status" | "pause" | "resume" | "clear" => self.add_info_message(
                "Plan execution is tracked by /goal.".to_string(),
                Some("Use /goal to view, pause, resume, or clear the active goal.".to_string()),
            ),
            _ => self.add_error_message("Usage: /plan".to_string()),
        }
    }

    fn prepare_slash_command_transcript_output(&mut self) {
        self.scroll_transcript_to_bottom();
    }

    fn ensure_programmer_goal_allowed(&mut self) -> bool {
        if !self.config.features.enabled(Feature::Goals) {
            self.add_info_message(
                "Goals are disabled.".to_string(),
                Some("Enable the goals feature to use /goal.".to_string()),
            );
            return false;
        }
        if self.effective_identity().kind != IdentityKind::Programmer {
            self.add_info_message(
                "Goal requires programmer identity.".to_string(),
                Some("Use /identity and choose Programmer before running /goal.".to_string()),
            );
            return false;
        }
        true
    }

    fn dispatch_goal_command(&mut self, trimmed: &str) {
        if !self.ensure_programmer_goal_allowed() {
            return;
        }
        match trimmed {
            "" => self.dispatch_command(SlashCommand::Goal),
            "clear" => self
                .app_event_tx
                .send(AppEvent::CodexOp(Op::ThreadGoalClear)),
            "pause" => self
                .app_event_tx
                .send(AppEvent::CodexOp(Op::ThreadGoalSetStatus {
                    status: ThreadGoalStatus::Paused,
                })),
            "resume" => self
                .app_event_tx
                .send(AppEvent::CodexOp(Op::ThreadGoalSetStatus {
                    status: ThreadGoalStatus::Active,
                })),
            "edit" => {
                if !self.current_goal_state_known {
                    self.request_goal_state_refresh_for_edit();
                    return;
                }
                let Some(goal) = self.current_goal.clone() else {
                    self.add_info_message(
                        "No goal is currently set.".to_string(),
                        Some("Use /goal <objective> to create one.".to_string()),
                    );
                    return;
                };
                if goal.goal_id.is_empty() {
                    self.request_goal_state_refresh_for_edit();
                    return;
                }
                self.show_goal_edit_prompt(goal);
            }
            objective => {
                if let Err(message) =
                    crate::product::agent::protocol::validate_thread_goal_objective(objective)
                {
                    self.add_error_message(message);
                    return;
                }
                self.app_event_tx
                    .send(AppEvent::CodexOp(Op::ThreadGoalSetObjective {
                        objective: objective.to_string(),
                        mode: ThreadGoalSetMode::ConfirmIfExists,
                    }));
            }
        }
    }

    fn request_goal_state_refresh_for_edit(&mut self) {
        self.app_event_tx.send(AppEvent::CodexOp(Op::ThreadGoalGet));
        self.add_info_message(
            "Goal state is refreshing.".to_string(),
            Some("Run /goal edit again after it updates.".to_string()),
        );
    }

    fn show_no_goal_usage(&mut self) {
        self.add_info_message(
            "No goal is currently set.".to_string(),
            Some("Use /goal <objective> to start a programmer goal.".to_string()),
        );
    }

    fn show_goal_summary(&mut self, goal: &ThreadGoal) {
        self.add_info_message(
            format_goal_summary_title(goal),
            Some(goal_usage_summary(goal)),
        );
    }

    fn show_goal_edit_prompt(&mut self, goal: ThreadGoal) {
        let expected_goal_id = goal.goal_id.clone();
        let status = edited_goal_status(goal.status);
        let token_budget = goal.token_budget;
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new_with_initial_text(
            "Edit goal".to_string(),
            "Update the objective and press Enter".to_string(),
            None,
            goal.objective,
            Box::new(move |objective: String| {
                if let Err(message) =
                    crate::product::agent::protocol::validate_thread_goal_objective(&objective)
                {
                    tx.send_history_cell(Box::new(history_cell::new_error_event(message)));
                    return;
                }
                tx.send(AppEvent::CodexOp(Op::ThreadGoalSetObjective {
                    objective,
                    mode: ThreadGoalSetMode::UpdateExisting {
                        expected_goal_id: expected_goal_id.clone(),
                        status,
                        token_budget,
                    },
                }));
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn set_buddy_config(&mut self, config: TuiBuddy) {
        self.config.tui_buddy = config.clone();
        self.bottom_pane.set_buddy_config(config);
    }

    fn sync_buddy_config_from_config(&mut self) {
        self.bottom_pane
            .set_buddy_config(self.config.tui_buddy.clone());
        self.bottom_pane
            .set_buddy_identity_kind(self.active_identity_kind());
    }

    fn active_tui_buddy_for_turn(&self) -> TuiBuddy {
        let mut config = self.config.tui_buddy.clone();
        if let Some(buddy) = self.bottom_pane.buddy() {
            config.name = Some(buddy.name.clone());
            config.species = Some(buddy.species);
            config.eye = Some(buddy.eye);
            config.hat = Some(buddy.hat);
            config.rarity = Some(buddy.rarity);
            config.shiny = Some(buddy.shiny);
            config.personality = Some(buddy.personality.clone());
        }
        config
    }

    fn active_buddy_snapshot_for_turn(&self) -> BuddyTurnSnapshot {
        let buddy = self.active_tui_buddy_for_turn();
        BuddyTurnSnapshot {
            enabled: buddy.enabled,
            muted: buddy.muted,
            name: buddy.name,
            species: buddy.species.map(|species| species.to_string()),
            eye: buddy.eye.map(|eye| eye.to_string()),
            hat: buddy.hat.map(|hat| hat.to_string()),
            rarity: buddy.rarity.map(|rarity| rarity.to_string()),
            shiny: buddy.shiny,
            personality: buddy.personality,
            observer_enabled: buddy.observer.enabled,
            observer_model: buddy.observer.model,
            observer_max_reaction_chars: buddy.observer.max_reaction_chars,
        }
    }

    fn show_buddy_status(&mut self) {
        let Some(buddy) = self.bottom_pane.buddy().cloned() else {
            self.add_info_message("Buddy is getting ready.".to_string(), None);
            return;
        };
        self.add_to_history(history_cell::new_buddy_details(
            buddy,
            self.active_tui_buddy_for_turn(),
        ));
        self.request_redraw();
    }

    fn dispatch_buddy_command(&mut self, args: &str) {
        let mut parts = args.split_whitespace();
        let Some(action) = parts.next() else {
            self.show_buddy_status();
            return;
        };

        match action {
            "hatch" => {
                self.add_info_message(
                    "Buddy is generated automatically for each identity.".to_string(),
                    Some("Use /buddy to view it.".to_string()),
                );
            }
            "pet" => {
                if !self.bottom_pane.buddy_is_hatched() {
                    self.add_info_message(
                        "Buddy is getting ready.".to_string(),
                        Some("Try /buddy again in a moment.".to_string()),
                    );
                    return;
                }
                self.bottom_pane.pet_buddy();
            }
            "hide" => self.app_event_tx.send(AppEvent::PersistBuddyConfig {
                edit: BuddyConfigEdit::Enabled(false),
            }),
            "show" => {
                if !self.bottom_pane.buddy_is_hatched() {
                    self.add_info_message(
                        "Buddy is getting ready.".to_string(),
                        Some("Try /buddy again in a moment.".to_string()),
                    );
                    return;
                }
                self.app_event_tx.send(AppEvent::PersistBuddyConfig {
                    edit: BuddyConfigEdit::Enabled(true),
                });
            }
            "mute" => self.app_event_tx.send(AppEvent::PersistBuddyConfig {
                edit: BuddyConfigEdit::Muted(true),
            }),
            "unmute" => self.app_event_tx.send(AppEvent::PersistBuddyConfig {
                edit: BuddyConfigEdit::Muted(false),
            }),
            "rename" => {
                self.add_info_message(
                    "Buddy names are generated automatically for this session.".to_string(),
                    Some("Switch identities to meet a different buddy.".to_string()),
                );
            }
            "talk" => match parts.next() {
                Some("on") => self.app_event_tx.send(AppEvent::PersistBuddyConfig {
                    edit: BuddyConfigEdit::ObserverEnabled(true),
                }),
                Some("off") => self.app_event_tx.send(AppEvent::PersistBuddyConfig {
                    edit: BuddyConfigEdit::ObserverEnabled(false),
                }),
                _ => self.add_error_message("Usage: /buddy talk on|off".to_string()),
            },
            _ => self.add_error_message(
                "Usage: /buddy [pet|show|hide|mute|unmute|talk on|talk off]".to_string(),
            ),
        }
    }

    fn show_rename_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let has_name = self
            .thread_name
            .as_ref()
            .is_some_and(|name| !name.is_empty());
        let title = if has_name {
            "Rename thread"
        } else {
            "Name thread"
        };
        let thread_id = self.thread_id;
        let view = CustomPromptView::new(
            title.to_string(),
            "Type a name and press Enter".to_string(),
            None,
            Box::new(move |name: String| {
                let Some(name) = crate::product::agent::util::normalize_thread_name(&name) else {
                    tx.send_history_cell(Box::new(history_cell::new_error_event(
                        "Thread name cannot be empty.".to_string(),
                    )));
                    return;
                };
                let cell = Self::rename_confirmation_cell(&name, thread_id);
                tx.send_history_cell(Box::new(cell));
                tx.send(AppEvent::CodexOp(Op::SetThreadName { name }));
            }),
        );

        self.bottom_pane.show_view(Box::new(view));
    }

    pub(crate) fn handle_paste(&mut self, text: String) {
        self.bottom_pane.handle_paste(text);
    }

    // Returns true if caller should skip rendering this frame (a future frame is scheduled).
    pub(crate) fn handle_paste_burst_tick(&mut self, frame_requester: FrameRequester) -> bool {
        if self.bottom_pane.flush_paste_burst_if_due() {
            // A paste just flushed; request an immediate redraw and skip this frame.
            self.request_redraw();
            true
        } else if self.bottom_pane.is_in_paste_burst() {
            // While capturing a burst, schedule a follow-up tick and skip this frame
            // to avoid redundant renders between ticks.
            frame_requester.schedule_frame_in(
                crate::product::tui_app::bottom_pane::ChatComposer::recommended_paste_flush_delay(),
            );
            true
        } else {
            false
        }
    }

    fn flush_active_cell(&mut self) {
        if let Some(active) = self.active_cell.take() {
            self.needs_final_message_separator = true;
            self.app_event_tx.send_history_cell(active);
        }
    }

    fn flush_active_cell_with_viewport_repaint(&mut self) {
        if let Some(active) = self.active_cell.take() {
            self.needs_final_message_separator = true;
            self.app_event_tx
                .send_history_cell_with_viewport_repaint(active);
        }
    }

    pub(crate) fn add_to_history(&mut self, cell: impl HistoryCell + 'static) {
        self.add_boxed_history(Box::new(cell));
    }

    fn add_to_history_with_viewport_repaint(&mut self, cell: impl HistoryCell + 'static) {
        self.add_boxed_history_with_viewport_repaint(Box::new(cell));
    }

    fn add_boxed_history(&mut self, cell: Box<dyn HistoryCell>) {
        self.add_boxed_history_impl(cell, false);
    }

    fn add_boxed_history_with_viewport_repaint(&mut self, cell: Box<dyn HistoryCell>) {
        self.add_boxed_history_impl(cell, true);
    }

    fn add_boxed_history_impl(&mut self, cell: Box<dyn HistoryCell>, repaint_viewport: bool) {
        // Keep the placeholder session header as the active cell until real session info arrives,
        // so we can merge headers instead of committing a duplicate box to history.
        let keep_placeholder_header_active = !self.is_session_configured()
            && self
                .active_cell
                .as_ref()
                .is_some_and(|c| c.as_any().is::<history_cell::SessionHeaderHistoryCell>());

        if !keep_placeholder_header_active && cell.has_display_content() {
            // Only break exec grouping if the cell renders visible lines.
            self.flush_active_cell();
            self.needs_final_message_separator = true;
        }
        if repaint_viewport {
            self.app_event_tx
                .send_history_cell_with_viewport_repaint(cell);
        } else {
            self.app_event_tx.send_history_cell(cell);
        }
    }

    fn queue_user_message(&mut self, user_message: UserMessage) {
        if !self.is_session_configured()
            || self.bottom_pane.is_task_running()
            || self.is_review_mode
        {
            self.queued_user_messages.push_back(user_message);
            self.refresh_queued_user_messages();
        } else {
            self.submit_user_message(user_message);
        }
    }

    fn submit_user_message(&mut self, user_message: UserMessage) {
        if self.config.provider_config_required {
            self.add_info_message(
                "Configure a model provider before starting a session.".to_string(),
                Some("Use the provider setup form to create ~/.lha/models.json.".to_string()),
            );
            self.open_provider_popup();
            return;
        }
        if !self.is_session_configured() {
            tracing::warn!("cannot submit user message before session is configured; queueing");
            self.queued_user_messages.push_front(user_message);
            self.refresh_queued_user_messages();
            return;
        }

        let UserMessage {
            text,
            local_images,
            text_elements,
            mention_paths,
        } = user_message;
        if text.is_empty() && local_images.is_empty() {
            return;
        }

        self.scroll_transcript_to_bottom();

        let mut items: Vec<UserInput> = Vec::new();

        // Special-case: "!cmd" executes a local shell command instead of sending to the model.
        if let Some(stripped) = text.strip_prefix('!') {
            let cmd = stripped.trim();
            if cmd.is_empty() {
                self.app_event_tx
                    .send_history_cell(Box::new(history_cell::new_info_event(
                        USER_SHELL_COMMAND_HELP_TITLE.to_string(),
                        Some(USER_SHELL_COMMAND_HELP_HINT.to_string()),
                    )));
                return;
            }
            self.submit_op(Op::RunUserShellCommand {
                command: cmd.to_string(),
            });
            return;
        }

        for image in &local_images {
            items.push(UserInput::LocalImage {
                path: image.path.clone(),
            });
        }

        if !text.is_empty() {
            items.push(UserInput::Text {
                text: text.clone(),
                text_elements: text_elements.clone(),
            });
        }

        let mentions = collect_tool_mentions(&text, &mention_paths);
        let mut skill_names_lower: HashSet<String> = HashSet::new();

        if let Some(skills) = self.bottom_pane.skills() {
            skill_names_lower = skills
                .iter()
                .map(|skill| skill.name.to_ascii_lowercase())
                .collect();
            let skill_mentions = find_skill_mentions_with_tool_mentions(&mentions, skills);
            for skill in skill_mentions {
                items.push(UserInput::Skill {
                    name: skill.name.clone(),
                    path: skill.path.clone(),
                });
            }
        }

        if let Some(apps) = self.connectors_for_mentions() {
            let app_mentions = find_app_mentions(&mentions, apps, &skill_names_lower);
            for app in app_mentions {
                let app_id = app.id.as_str();
                items.push(UserInput::Mention {
                    name: app.name.clone(),
                    path: format!("app://{app_id}"),
                });
            }
        }

        self.track_loaded_skills_from_inputs(&items);

        let effective_mode = self.effective_identity();
        let identity = if self.identities_enabled() {
            self.active_identity_mask
                .as_ref()
                .map(|_| effective_mode.clone())
        } else {
            Some(effective_mode.clone())
        };
        let personality = self
            .config
            .personality
            .filter(|_| self.config.features.enabled(Feature::Personality))
            .filter(|_| self.current_model_supports_personality());
        let op = Op::UserTurn {
            items,
            cwd: self.config.cwd.clone(),
            approval_policy: self.config.approval_policy.value(),
            sandbox_policy: self.config.sandbox_policy.get().clone(),
            model: effective_mode.model().to_string(),
            effort: effective_mode.reasoning_effort(),
            summary: self.config.model_reasoning_summary,
            final_output_json_schema: None,
            identity,
            personality,
            tui_buddy: Some(self.active_buddy_snapshot_for_turn()),
        };

        self.codex_op_tx.send(op).unwrap_or_else(|e| {
            tracing::error!("failed to send message: {e}");
        });

        // Persist the text to cross-session message history.
        if !text.is_empty() {
            self.codex_op_tx
                .send(Op::AddToHistory { text: text.clone() })
                .unwrap_or_else(|e| {
                    tracing::error!("failed to send AddHistory op: {e}");
                });
        }

        // Only show the text portion in conversation history.
        if !text.is_empty() {
            let local_image_paths = local_images.into_iter().map(|img| img.path).collect();
            self.add_to_history(history_cell::new_user_prompt(
                text,
                text_elements,
                local_image_paths,
            ));
        }

        self.needs_final_message_separator = false;
    }

    /// Replay a subset of initial events into the UI to seed the transcript when
    /// resuming an existing session. This approximates the live event flow and
    /// is intentionally conservative: only safe-to-replay items are rendered to
    /// avoid triggering side effects. Event ids are passed as `None` to
    /// distinguish replayed events from live ones.
    fn replay_initial_messages(&mut self, events: Vec<EventMsg>) {
        for msg in events {
            if matches!(
                msg,
                EventMsg::SessionConfigured(_) | EventMsg::ThreadNameUpdated(_)
            ) {
                continue;
            }
            // `id: None` indicates a synthetic/fake id coming from replay.
            self.dispatch_event_msg(None, msg, true);
        }
    }

    pub(crate) fn handle_codex_event(&mut self, event: Event) {
        let Event { id, msg } = event;
        self.dispatch_event_msg(Some(id), msg, false);
    }

    #[cfg(test)]
    pub(crate) fn handle_codex_event_replay(&mut self, event: Event) {
        let Event { msg, .. } = event;
        if matches!(msg, EventMsg::ShutdownComplete) {
            return;
        }
        self.dispatch_event_msg(None, msg, true);
    }

    /// Dispatch a protocol `EventMsg` to the appropriate handler.
    ///
    /// `id` is `Some` for live events and `None` for replayed events from
    /// `replay_initial_messages()`. Callers should treat `None` as a "fake" id
    /// that must not be used to correlate follow-up actions.
    fn dispatch_event_msg(&mut self, id: Option<String>, msg: EventMsg, from_replay: bool) {
        let is_stream_error = matches!(&msg, EventMsg::StreamError(_));
        if !is_stream_error {
            self.restore_retry_status_if_present();
        }

        if self.pending_review_start_transition
            && matches!(
                &msg,
                EventMsg::Error(_)
                    | EventMsg::StreamError(_)
                    | EventMsg::TurnAborted(_)
                    | EventMsg::TurnComplete(_)
            )
        {
            self.clear_review_start_transition();
        }

        if from_replay
            && self.pending_replay_legacy_context_compactions > 0
            && !matches!(&msg, EventMsg::ContextCompacted(_))
        {
            self.pending_replay_legacy_context_compactions = 0;
        }

        match msg {
            EventMsg::AgentMessageDelta(_)
            | EventMsg::PlanDelta(_)
            | EventMsg::AgentReasoningDelta(_)
            | EventMsg::TerminalInteraction(_)
            | EventMsg::ExecCommandOutputDelta(_) => {}
            _ => {
                tracing::trace!("handle_codex_event: {:?}", msg);
            }
        }

        match msg {
            EventMsg::SessionConfigured(e) => self.on_session_configured(e),
            EventMsg::ThreadNameUpdated(e) => self.on_thread_name_updated(e),
            EventMsg::ThreadGoalUpdated(e) => self.on_thread_goal_updated(e),
            EventMsg::ThreadGoalCleared(e) => self.on_thread_goal_cleared(e),
            EventMsg::ThreadGoalSnapshot(e) => self.on_thread_goal_snapshot(e),
            EventMsg::ThreadGoalReplaceConfirmationRequired(e) => {
                self.on_thread_goal_replace_confirmation_required(e)
            }
            EventMsg::AgentMessage(AgentMessageEvent { message, .. }) => {
                self.on_agent_message(message)
            }
            EventMsg::AgentMessageDelta(AgentMessageDeltaEvent { delta }) => {
                self.on_agent_message_delta(delta)
            }
            EventMsg::PlanDelta(event) => self.on_plan_delta(event.delta),
            EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent { delta }) => {
                self.on_legacy_reasoning_delta(delta)
            }
            EventMsg::AgentReasoningRawContentDelta(AgentReasoningRawContentDeltaEvent {
                delta,
            }) => self.on_legacy_raw_reasoning_delta(delta),
            EventMsg::AgentReasoning(AgentReasoningEvent { text }) => {
                self.on_legacy_reasoning_final(id.as_deref(), text)
            }
            EventMsg::AgentReasoningRawContent(AgentReasoningRawContentEvent { text }) => {
                self.on_legacy_raw_reasoning_final(id.as_deref(), text)
            }
            EventMsg::AgentReasoningSectionBreak(_) => {
                self.last_legacy_reasoning_finalized = None;
                self.on_reasoning_section_break();
            }
            EventMsg::TurnStarted(_) => self.on_task_started(),
            EventMsg::TurnComplete(TurnCompleteEvent { last_agent_message }) => {
                self.on_task_complete(last_agent_message, from_replay)
            }
            EventMsg::TokenCount(ev) => {
                self.set_token_info(ev.info);
            }
            EventMsg::InputSlimming(ev) => self.on_input_slimming(ev),
            EventMsg::BuddyReaction(ev) => {
                self.bottom_pane.set_buddy_reaction(ev.text);
            }
            EventMsg::Warning(WarningEvent { message }) => self.on_warning(message),
            EventMsg::Error(ErrorEvent {
                message,
                codex_error_info,
            }) => {
                if let Some(info) = codex_error_info
                    && let Some(RateLimitErrorKind::ModelCap {
                        model,
                        reset_after_seconds,
                    }) = rate_limit_error_kind(&info)
                {
                    self.on_model_cap_error(model, reset_after_seconds);
                } else {
                    self.on_error(message);
                }
            }
            EventMsg::McpStartupUpdate(ev) => self.on_mcp_startup_update(ev),
            EventMsg::McpStartupComplete(ev) => self.on_mcp_startup_complete(ev),
            EventMsg::TurnAborted(ev) => match ev.reason {
                TurnAbortReason::Interrupted => {
                    self.on_interrupted_turn(ev.reason);
                }
                TurnAbortReason::Replaced => {
                    self.on_error("Turn aborted: replaced by a new task".to_owned())
                }
                TurnAbortReason::ReviewEnded => {
                    self.on_interrupted_turn(ev.reason);
                }
            },
            EventMsg::PlanUpdate(update) => self.on_plan_update(update),
            EventMsg::AgentJobStatus(event) => {
                if !from_replay {
                    self.on_agent_job_status(event);
                }
            }
            EventMsg::ExecApprovalRequest(ev) => {
                // For replayed events, synthesize an empty id (these should not occur).
                self.on_exec_approval_request(id.unwrap_or_default(), ev)
            }
            EventMsg::ApplyPatchApprovalRequest(ev) => {
                self.on_apply_patch_approval_request(id.unwrap_or_default(), ev)
            }
            EventMsg::ElicitationRequest(ev) => {
                self.on_elicitation_request(ev);
            }
            EventMsg::RequestUserInput(ev) => {
                self.on_request_user_input(ev);
            }
            EventMsg::ExecCommandBegin(ev) => self.on_exec_command_begin(ev),
            EventMsg::TerminalInteraction(delta) => self.on_terminal_interaction(delta),
            EventMsg::ExecCommandOutputDelta(delta) => self.on_exec_command_output_delta(delta),
            EventMsg::PatchApplyBegin(ev) => self.on_patch_apply_begin(ev),
            EventMsg::PatchApplyEnd(ev) => self.on_patch_apply_end(ev),
            EventMsg::ExecCommandEnd(ev) => self.on_exec_command_end(ev),
            EventMsg::ViewImageToolCall(ev) => self.on_view_image_tool_call(ev),
            EventMsg::McpToolCallBegin(ev) => self.on_mcp_tool_call_begin(ev),
            EventMsg::McpToolCallEnd(ev) => self.on_mcp_tool_call_end(ev),
            EventMsg::WebSearchBegin(ev) => self.on_web_search_begin(ev),
            EventMsg::WebSearchEnd(ev) => self.on_web_search_end(ev),
            EventMsg::GetHistoryEntryResponse(ev) => self.on_get_history_entry_response(ev),
            EventMsg::McpListToolsResponse(ev) => self.on_list_mcp_tools(ev),
            EventMsg::ListCustomPromptsResponse(ev) => self.on_list_custom_prompts(ev),
            EventMsg::ListSkillsResponse(ev) => self.on_list_skills(ev),
            EventMsg::SkillsUpdateAvailable => {
                self.request_skills_refresh(true);
            }
            EventMsg::ShutdownComplete => self.on_shutdown_complete(),
            EventMsg::TurnDiff(TurnDiffEvent { unified_diff }) => self.on_turn_diff(unified_diff),
            EventMsg::DeprecationNotice(ev) => self.on_deprecation_notice(ev),
            EventMsg::BackgroundEvent(BackgroundEventEvent { message }) => {
                self.on_background_event(message)
            }
            EventMsg::UndoStarted(ev) => self.on_undo_started(ev),
            EventMsg::UndoCompleted(ev) => self.on_undo_completed(ev),
            EventMsg::StreamError(StreamErrorEvent {
                message,
                additional_details,
                ..
            }) => self.on_stream_error(message, additional_details),
            EventMsg::UserMessage(ev) => {
                if from_replay {
                    self.on_user_message_event(ev);
                }
            }
            EventMsg::EnteredReviewMode(review_request) => {
                self.on_entered_review_mode(review_request, from_replay)
            }
            EventMsg::ExitedReviewMode(review) => self.on_exited_review_mode(review),
            EventMsg::ContextCompacted(_) => self.on_context_compacted(id),
            EventMsg::ThreadRolledBack(_) => {}
            EventMsg::RawTranscriptItem(_)
            | EventMsg::ItemStarted(_)
            | EventMsg::AgentMessageContentDelta(_)
            | EventMsg::DynamicToolCallRequest(_)
            | EventMsg::WorkflowUpdate(_) => {}
            EventMsg::ReasoningContentDelta(event) => self.on_reasoning_content_delta(event),
            EventMsg::ReasoningRawContentDelta(event) => self.on_reasoning_raw_content_delta(event),
            EventMsg::ItemCompleted(event) => {
                let crate::product::agent::protocol::ItemCompletedEvent {
                    thread_id, item, ..
                } = event;
                match item {
                    TurnItem::Reasoning(item) => {
                        self.on_reasoning_item_completed(id.as_deref(), item, from_replay);
                    }
                    TurnItem::ContextCompaction(item) => {
                        // Replay omits the outer event id, but legacy compact events still follow
                        // structured compaction events in order. Reserve a suppression slot even
                        // for parent-thread compactions so fork replay does not misattribute them.
                        if id.is_none() {
                            self.pending_replay_legacy_context_compactions += 1;
                        }

                        if self.thread_id == Some(thread_id)
                            && self.counted_context_compaction_item_ids.insert(item.id)
                        {
                            self.context_compact_count += 1;
                            if let Some(event_id) = id.as_ref() {
                                *self
                                    .pending_live_legacy_context_compactions
                                    .entry(event_id.clone())
                                    .or_default() += 1;
                            }
                        }
                    }
                    TurnItem::Plan(plan_item) => self.on_plan_item_completed(plan_item.text),
                    TurnItem::UserMessage(_)
                    | TurnItem::AgentMessage(_)
                    | TurnItem::WebSearch(_) => {}
                }
            }
        }
    }

    fn on_context_compacted(&mut self, id: Option<String>) {
        match id {
            Some(event_id) => {
                // Live legacy compaction notifications are compatibility echoes of structured
                // compaction items. Consume a pending echo when present; otherwise treat it as a
                // standalone legacy compact and count it.
                let mut remove_entry = false;
                let should_increment = if let Some(pending) = self
                    .pending_live_legacy_context_compactions
                    .get_mut(&event_id)
                {
                    *pending = pending.saturating_sub(1);
                    remove_entry = *pending == 0;
                    false
                } else {
                    true
                };
                if remove_entry {
                    self.pending_live_legacy_context_compactions
                        .remove(&event_id);
                }
                if should_increment {
                    self.context_compact_count += 1;
                }
            }
            None => {
                // Replay has no event id, so suppress only the next expected legacy echo.
                if self.pending_replay_legacy_context_compactions > 0 {
                    self.pending_replay_legacy_context_compactions -= 1;
                } else {
                    self.context_compact_count += 1;
                }
            }
        }

        self.on_agent_message("Context compacted".to_owned());
    }

    pub(crate) fn prepare_for_review_start_transition(&mut self) {
        self.pending_review_start_transition = true;
        self.update_task_running_state_with_redraw(false);
    }

    pub(crate) fn clear_review_start_transition(&mut self) {
        self.pending_review_start_transition = false;
        self.update_task_running_state_with_redraw(false);
    }

    fn on_entered_review_mode(&mut self, review: ReviewRequest, from_replay: bool) {
        // Enter review mode and emit a concise banner
        if self.pre_review_token_info.is_none() {
            self.pre_review_token_info = Some(self.token_info.clone());
        }
        // Avoid toggling running state for replayed history events on resume.
        self.is_review_mode = true;
        let hint = review.user_facing_hint.unwrap_or_else(|| {
            crate::product::agent::review_prompts::user_facing_hint(&review.target)
        });
        let banner = format!(">> Code review started: {hint} <<");
        self.add_to_history(history_cell::new_review_status_line(banner));
        self.clear_review_start_transition();
        if !from_replay {
            if !self.bottom_pane.is_task_running() {
                self.bottom_pane.set_task_running_with_redraw(true, false);
            }
            self.bottom_pane
                .set_interrupt_hint_visible_with_redraw(true, false);
        }
    }

    fn on_exited_review_mode(&mut self, review: ExitedReviewModeEvent) {
        // Leave review mode; if output is present, flush pending stream + show results.
        if let Some(output) = review.review_output {
            self.flush_answer_stream_with_separator();
            self.flush_interrupt_queue();
            self.flush_active_cell_with_viewport_repaint();

            if output.findings.is_empty() {
                let explanation = output.overall_explanation.trim().to_string();
                if explanation.is_empty() {
                    tracing::error!("Reviewer failed to output a response.");
                    self.add_to_history(history_cell::new_error_event(
                        "Reviewer failed to output a response.".to_owned(),
                    ));
                } else {
                    // Show explanation when there are no structured findings.
                    let mut rendered: Vec<ratatui::text::Line<'static>> = vec!["".into()];
                    append_markdown(&explanation, None, &mut rendered);
                    let body_cell = AgentMessageCell::new(rendered, false);
                    self.app_event_tx
                        .send_history_cell_with_viewport_repaint(Box::new(body_cell));
                }
            }
            // Final message is rendered as part of the AgentMessage.
        }

        self.finish_review_progress_ui();
        self.is_review_mode = false;
        self.restore_pre_review_token_info();
        // Append a finishing banner at the end of this turn.
        self.add_to_history(history_cell::new_review_status_line(
            "<< Code review finished >>".to_string(),
        ));
        self.request_redraw();
    }

    fn on_user_message_event(&mut self, event: UserMessageEvent) {
        if !event.message.trim().is_empty() {
            self.add_to_history(history_cell::new_user_prompt(
                event.message,
                event.text_elements,
                event.local_images,
            ));
        }

        // User messages reset separator state so the next agent response doesn't add a stray break.
        self.needs_final_message_separator = false;
    }

    /// Exit the UI immediately without waiting for shutdown.
    ///
    /// Prefer [`Self::request_quit_without_confirmation`] for user-initiated exits;
    /// this is mainly a fallback for shutdown completion or emergency exits.
    fn request_immediate_exit(&self) {
        self.app_event_tx.send(AppEvent::Exit(ExitMode::Immediate));
    }

    /// Request a user-initiated quit.
    ///
    /// When a session thread exists we prefer a graceful shutdown so the agent can flush state.
    /// If startup has not produced a thread yet, exit immediately instead of waiting on a
    /// `ShutdownComplete` event that can never arrive.
    fn request_quit_without_confirmation(&self) {
        let exit_mode = if self.thread_id.is_some() {
            ExitMode::ShutdownFirst
        } else {
            ExitMode::Immediate
        };
        self.app_event_tx.send(AppEvent::Exit(exit_mode));
    }

    fn request_redraw(&mut self) {
        self.frame_requester.schedule_frame();
    }

    fn request_redraw_with_risky_row_repair(&self) {
        self.frame_requester.schedule_frame_with_risky_row_repair();
    }

    fn bump_active_cell_revision(&mut self) {
        // Wrapping avoids overflow; wraparound would require 2^64 bumps and at
        // worst causes a one-time cache-key collision.
        self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
        self.request_redraw_with_risky_row_repair();
    }

    fn notify(&mut self, notification: Notification) {
        if !notification.allowed_for(&self.config.tui_notifications) {
            return;
        }
        self.pending_notification = Some(notification);
        self.request_redraw();
    }

    pub(crate) fn maybe_post_pending_notification(
        &mut self,
        tui: &mut crate::product::tui_app::tui::Tui,
    ) {
        if let Some(notif) = self.pending_notification.take() {
            tui.notify(notif.display());
        }
    }

    /// Mark the active cell as failed (✗) and flush it into history.
    fn finalize_active_cell_as_failed(&mut self) {
        if let Some(mut cell) = self.active_cell.take() {
            // Insert finalized cell into history and keep grouping consistent.
            if let Some(exec) = cell.as_any_mut().downcast_mut::<ExecCell>() {
                exec.mark_failed();
            } else if let Some(tool) = cell.as_any_mut().downcast_mut::<McpToolCallCell>() {
                tool.mark_failed();
            }
            self.add_boxed_history(cell);
        }
    }

    // If idle and there are queued inputs, submit exactly one to start the next turn.
    fn maybe_send_next_queued_input(&mut self) {
        if self.bottom_pane.is_task_running() {
            return;
        }
        if let Some(user_message) = self.queued_user_messages.pop_front() {
            self.submit_user_message(user_message);
        }
        // Update the list to reflect the remaining queued messages (if any).
        self.refresh_queued_user_messages();
    }

    /// Rebuild and update the queued user messages from the current queue.
    fn refresh_queued_user_messages(&mut self) {
        let messages: Vec<String> = self
            .queued_user_messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        self.bottom_pane.set_queued_user_messages(messages);
    }

    pub(crate) fn add_diff_in_progress(&mut self) {
        self.request_redraw();
    }

    pub(crate) fn on_diff_complete(&mut self) {
        self.request_redraw();
    }

    pub(crate) fn add_status_output(&mut self) {
        let default_usage = TokenUsage::default();
        let token_info = self.token_info.as_ref();
        let total_usage = token_info
            .map(|ti| &ti.total_token_usage)
            .unwrap_or(&default_usage);
        let identity = self.identity_label();
        let reasoning_effort_override = Some(self.effective_reasoning_effort());
        self.add_to_history(
            crate::product::tui_app::status::new_status_output_with_context_compact_count(
                &self.config,
                self.auth_manager.as_ref(),
                token_info,
                total_usage,
                &self.thread_id,
                self.thread_name.clone(),
                self.forked_from,
                self.model_display_name(),
                identity,
                reasoning_effort_override,
                self.context_compact_count,
            ),
        );
    }

    pub(crate) fn add_ps_output(&mut self) {
        let processes = self
            .unified_exec_processes
            .iter()
            .map(|process| history_cell::UnifiedExecProcessDetails {
                command_display: process.command_display.clone(),
                recent_chunks: process.recent_chunks.clone(),
            })
            .collect();
        self.add_to_history(history_cell::new_unified_exec_processes_output(processes));
    }

    fn clean_background_terminals(&mut self) {
        self.submit_op(Op::CleanBackgroundTerminals);
        self.add_info_message("Stopping all background terminals.".to_string(), None);
    }

    fn prefetch_connectors(&mut self) {
        if !self.connectors_enabled() {
            return;
        }
        if matches!(self.connectors_cache, ConnectorsCacheState::Loading) {
            return;
        }

        self.connectors_cache = ConnectorsCacheState::Loading;
        let config = self.config.clone();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result: Result<ConnectorsSnapshot, anyhow::Error> = async {
                let connectors =
                    connectors::list_accessible_connectors_from_mcp_tools(&config).await?;
                Ok(ConnectorsSnapshot { connectors })
            }
            .await;
            let result = result.map_err(|err| format!("Failed to load apps: {err}"));
            app_event_tx.send(AppEvent::ConnectorsLoaded(result));
        });
    }

    /// Open a popup to choose a quick auto model. Selecting "All models"
    /// opens the full picker with every available preset.
    pub(crate) fn open_model_popup(&mut self) {
        if self.config.provider_config_required {
            self.add_info_message(
                "Configure a model provider before choosing a model.".to_string(),
                Some("Add a provider to create ~/.lha/models.json.".to_string()),
            );
            self.open_provider_popup();
            return;
        }
        if !self.is_session_configured() {
            self.add_info_message(
                "Model selection is disabled until startup completes.".to_string(),
                None,
            );
            return;
        }

        let presets: Vec<ModelPreset> = match self
            .thread_manager
            .try_list_model_switcher_models(&self.config)
        {
            Ok(models) => models,
            Err(_) => {
                self.add_info_message(
                    "Models are being updated; please try /model again in a moment.".to_string(),
                    None,
                );
                return;
            }
        };
        self.app_event_tx
            .send(AppEvent::OpenModelSelectionModal { presets });
        self.request_redraw();
    }

    pub(crate) fn open_provider_popup(&mut self) {
        self.app_event_tx.send(AppEvent::OpenProviderConfigModal);
        self.request_redraw();
    }

    pub(crate) fn attach_started_thread(
        &mut self,
        thread: std::sync::Arc<crate::product::agent::CodexThread>,
        thread_id: crate::product::protocol::ThreadId,
        session_configured: crate::product::agent::protocol::SessionConfiguredEvent,
    ) {
        self.agent_shutdown_complete = false;
        self.pending_initial_identity_sync = true;
        self.codex_op_tx = attach_existing_thread(
            thread,
            thread_id,
            session_configured,
            self.app_event_tx.clone(),
        );
    }

    pub(crate) fn open_personality_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Personality selection is disabled until startup completes.".to_string(),
                None,
            );
            return;
        }
        if !self.current_model_supports_personality() {
            let current_model = self.current_model();
            self.add_error_message(format!(
                "Current model ({current_model}) doesn't support personalities. Try /model to pick a different model."
            ));
            return;
        }
        self.app_event_tx
            .send(AppEvent::OpenPersonalitySelectionModal {
                current_personality: self.config.personality.unwrap_or(Personality::Friendly),
            });
        self.request_redraw();
    }

    fn model_menu_header(&self, title: &str, subtitle: &str) -> Box<dyn Renderable> {
        let title = title.to_string();
        let subtitle = subtitle.to_string();
        let mut header = ColumnRenderable::new();
        header.push(Line::from(title.bold()));
        header.push(Line::from(subtitle.dim()));
        if let Some(warning) = self.model_menu_warning_line() {
            header.push(warning);
        }
        Box::new(header)
    }

    fn model_menu_warning_line(&self) -> Option<Line<'static>> {
        let base_url = self.custom_openai_base_url()?;
        let warning = format!(
            "Warning: OPENAI_BASE_URL is set to {base_url}. Selecting models may not be supported or work properly."
        );
        Some(Line::from(warning.red()))
    }

    fn custom_openai_base_url(&self) -> Option<String> {
        if !self.config.model_provider.is_openai() {
            return None;
        }

        let base_url = self.config.model_provider.base_url.as_ref()?;
        let trimmed = base_url.trim();
        if trimmed.is_empty() {
            return None;
        }

        let normalized = trimmed.trim_end_matches('/');
        if normalized == DEFAULT_OPENAI_BASE_URL {
            return None;
        }

        Some(trimmed.to_string())
    }

    pub(crate) fn model_selection_context(
        &self,
    ) -> crate::product::tui_app::model_selection_modal::ModelSelectionModalContext {
        crate::product::tui_app::model_selection_modal::ModelSelectionModalContext {
            current_model: self.current_model().to_string(),
            current_provider_id: self.config.model_provider_id.clone(),
            effective_reasoning_effort: self.effective_reasoning_effort(),
            custom_openai_base_url: self.custom_openai_base_url(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn open_model_popup_with_presets(&mut self, presets: Vec<ModelPreset>) {
        let presets: Vec<ModelPreset> = presets
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .collect();
        let has_exact_provider_match = self.has_exact_current_model_provider_match(&presets);

        let current_label = presets
            .iter()
            .find(|preset| {
                self.is_current_model_preset_with_exact_provider_match(
                    preset,
                    has_exact_provider_match,
                )
            })
            .map(|preset| preset.display_name.to_string())
            .unwrap_or_else(|| self.model_display_name().to_string());

        let (mut auto_presets, other_presets): (Vec<ModelPreset>, Vec<ModelPreset>) = presets
            .into_iter()
            .partition(|preset| Self::is_auto_model(&preset.model));

        if auto_presets.is_empty() {
            self.open_all_models_popup(other_presets);
            return;
        }

        auto_presets.sort_by_key(|preset| Self::auto_model_order(&preset.model));

        let mut items: Vec<SelectionItem> = auto_presets
            .into_iter()
            .map(|preset| {
                let description =
                    (!preset.description.is_empty()).then_some(preset.description.clone());
                let model = preset.model.clone();
                let provider_id = preset.model_provider_id.clone();
                let actions = Self::model_selection_actions(
                    model,
                    provider_id,
                    Some(preset.default_reasoning_effort),
                );
                SelectionItem {
                    name: preset.display_name.clone(),
                    description,
                    is_current: self.is_current_model_preset_with_exact_provider_match(
                        &preset,
                        has_exact_provider_match,
                    ),
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        if !other_presets.is_empty() {
            let all_models = other_presets;
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenAllModelsPopup {
                    models: all_models.clone(),
                });
            })];

            let is_current = !items.iter().any(|item| item.is_current);
            let description = Some(format!(
                "Choose a specific model and reasoning level (current: {current_label})"
            ));

            items.push(SelectionItem {
                name: "All models".to_string(),
                description,
                is_current,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let header = self.model_menu_header(
            "Select Model",
            "Pick a quick auto mode or browse all models.",
        );
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header,
            ..Default::default()
        });
    }

    #[allow(dead_code)]
    fn is_auto_model(model: &str) -> bool {
        model.starts_with("codex-auto-")
    }

    #[allow(dead_code)]
    fn auto_model_order(model: &str) -> usize {
        match model {
            "codex-auto-fast" => 0,
            "codex-auto-balanced" => 1,
            "codex-auto-thorough" => 2,
            _ => 3,
        }
    }

    pub(crate) fn open_all_models_popup(&mut self, presets: Vec<ModelPreset>) {
        if presets.is_empty() {
            self.add_info_message(
                "No additional models are available right now.".to_string(),
                None,
            );
            return;
        }

        let has_exact_provider_match = self.has_exact_current_model_provider_match(&presets);
        let mut items: Vec<SelectionItem> = Vec::new();
        for preset in presets.iter().cloned() {
            let description =
                (!preset.description.is_empty()).then_some(preset.description.to_string());
            let is_current = self.is_current_model_preset_with_exact_provider_match(
                &preset,
                has_exact_provider_match,
            );
            let requires_reasoning_picker = preset.supported_reasoning_efforts.len() > 1;
            let preset_for_action = preset.clone();
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                let preset_for_event = preset_for_action.clone();
                tx.send(AppEvent::OpenReasoningPopup {
                    model: preset_for_event,
                });
            })];
            items.push(SelectionItem {
                name: preset.display_name.clone(),
                description,
                is_current,
                actions,
                dismiss_on_select: !requires_reasoning_picker,
                ..Default::default()
            });
        }

        let header = self.model_menu_header(
            "Select Model and Effort",
            "Access legacy models by running lha -m <model_name> or in your config.toml",
        );
        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some("Press enter to select reasoning effort, or esc to dismiss.".into()),
            items,
            header,
            ..Default::default()
        });
    }

    pub(crate) fn open_identity_popup(&mut self) {
        self.app_event_tx.send(AppEvent::OpenIdentityModal);
    }

    fn model_selection_actions(
        model_for_action: String,
        provider_id_for_action: Option<String>,
        effort_for_action: Option<ReasoningEffortConfig>,
    ) -> Vec<SelectionAction> {
        vec![Box::new(move |tx| {
            let effort_label = effort_for_action
                .map(|effort| effort.to_string())
                .unwrap_or_else(|| "default".to_string());
            tx.send(AppEvent::PersistModelSelection {
                model: model_for_action.clone(),
                provider_id: provider_id_for_action.clone(),
                effort: effort_for_action,
            });
            tracing::info!(
                "Selected model: {}, Selected effort: {}",
                model_for_action,
                effort_label
            );
        })]
    }

    /// Open a popup to choose the reasoning effort (stage 2) for the given model.
    pub(crate) fn open_reasoning_popup(&mut self, preset: ModelPreset) {
        let default_effort: ReasoningEffortConfig = preset.default_reasoning_effort;
        let supported = preset.supported_reasoning_efforts;

        let warn_effort = if supported
            .iter()
            .any(|option| option.effort == ReasoningEffortConfig::XHigh)
        {
            Some(ReasoningEffortConfig::XHigh)
        } else if supported
            .iter()
            .any(|option| option.effort == ReasoningEffortConfig::High)
        {
            Some(ReasoningEffortConfig::High)
        } else {
            None
        };
        let warning_text = warn_effort.map(|effort| {
            let effort_label = Self::reasoning_effort_label(effort);
            format!("⚠ {effort_label} reasoning effort can quickly consume Plus plan rate limits.")
        });
        let warn_for_model = preset.model.starts_with("gpt-5.1-codex")
            || preset.model.starts_with("gpt-5.1-codex-max")
            || preset.model.starts_with("gpt-5.2");

        struct EffortChoice {
            stored: Option<ReasoningEffortConfig>,
            display: ReasoningEffortConfig,
        }
        let mut choices: Vec<EffortChoice> = Vec::new();
        for effort in ReasoningEffortConfig::iter() {
            if supported.iter().any(|option| option.effort == effort) {
                choices.push(EffortChoice {
                    stored: Some(effort),
                    display: effort,
                });
            }
        }
        if choices.is_empty() {
            choices.push(EffortChoice {
                stored: (default_effort != ReasoningEffortConfig::None).then_some(default_effort),
                display: default_effort,
            });
        }

        if choices.len() == 1 {
            if let Some(effort) = choices.first().and_then(|c| c.stored) {
                self.apply_model_and_effort(preset.model, preset.model_provider_id, Some(effort));
            } else {
                self.apply_model_and_effort(preset.model, preset.model_provider_id, None);
            }
            return;
        }

        let default_choice: Option<ReasoningEffortConfig> = choices
            .iter()
            .any(|choice| choice.stored == Some(default_effort))
            .then_some(Some(default_effort))
            .flatten()
            .or_else(|| choices.iter().find_map(|choice| choice.stored))
            .or(Some(default_effort));

        let model_slug = preset.model.to_string();
        let is_current_model = self.is_current_model_for_selection(
            preset.model.as_str(),
            preset.model_provider_id.as_deref(),
        );
        let highlight_choice = if is_current_model {
            self.effective_reasoning_effort()
        } else {
            default_choice
        };
        let selection_choice = highlight_choice.or(default_choice);
        let initial_selected_idx = choices
            .iter()
            .position(|choice| choice.stored == selection_choice)
            .or_else(|| {
                selection_choice
                    .and_then(|effort| choices.iter().position(|choice| choice.display == effort))
            });
        let mut items: Vec<SelectionItem> = Vec::new();
        for choice in choices.iter() {
            let effort = choice.display;
            let mut effort_label = Self::reasoning_effort_label(effort).to_string();
            if choice.stored == default_choice {
                effort_label.push_str(" (default)");
            }

            let description = choice
                .stored
                .and_then(|effort| {
                    supported
                        .iter()
                        .find(|option| option.effort == effort)
                        .map(|option| option.description.to_string())
                })
                .filter(|text| !text.is_empty());

            let show_warning = warn_for_model && warn_effort == Some(effort);
            let selected_description = if show_warning {
                warning_text.as_ref().map(|warning_message| {
                    description.as_ref().map_or_else(
                        || warning_message.clone(),
                        |d| format!("{d}\n{warning_message}"),
                    )
                })
            } else {
                None
            };

            let model_for_action = model_slug.clone();
            let actions = Self::model_selection_actions(
                model_for_action,
                preset.model_provider_id.clone(),
                choice.stored,
            );

            items.push(SelectionItem {
                name: effort_label,
                description,
                selected_description,
                is_current: is_current_model && choice.stored == highlight_choice,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let mut header = ColumnRenderable::new();
        header.push(Line::from(
            format!("Select Reasoning Level for {model_slug}").bold(),
        ));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    fn reasoning_effort_label(effort: ReasoningEffortConfig) -> &'static str {
        match effort {
            ReasoningEffortConfig::None => "None",
            ReasoningEffortConfig::Minimal => "Minimal",
            ReasoningEffortConfig::Low => "Low",
            ReasoningEffortConfig::Medium => "Medium",
            ReasoningEffortConfig::High => "High",
            ReasoningEffortConfig::XHigh => "Extra high",
            ReasoningEffortConfig::Max => "Max",
            ReasoningEffortConfig::Ultra => "Ultra",
        }
    }

    fn apply_model_and_effort(
        &self,
        model: String,
        provider_id: Option<String>,
        effort: Option<ReasoningEffortConfig>,
    ) {
        self.app_event_tx.send(AppEvent::PersistModelSelection {
            model: model.clone(),
            provider_id,
            effort,
        });
        tracing::info!(
            "Selected model: {}, Selected effort: {}",
            model,
            effort
                .map(|e| e.to_string())
                .unwrap_or_else(|| "default".to_string())
        );
    }

    fn is_current_model_preset(&self, preset: &ModelPreset) -> bool {
        self.is_current_model_for_selection(
            preset.model.as_str(),
            preset.model_provider_id.as_deref(),
        )
    }

    fn has_exact_current_model_provider_match(&self, presets: &[ModelPreset]) -> bool {
        let current_model = self.current_model();
        let current_provider_id = self.config.model_provider_id.as_str();

        presets.iter().any(|candidate| {
            candidate.model == current_model
                && candidate.model_provider_id.as_deref() == Some(current_provider_id)
        })
    }

    fn is_current_model_preset_with_exact_provider_match(
        &self,
        preset: &ModelPreset,
        has_exact_provider_match: bool,
    ) -> bool {
        if self.current_model() != preset.model {
            return false;
        }

        if has_exact_provider_match {
            return preset.model_provider_id.as_deref()
                == Some(self.config.model_provider_id.as_str());
        }

        self.is_current_model_preset(preset)
    }

    fn is_current_model_for_selection(&self, model: &str, provider_id: Option<&str>) -> bool {
        if self.current_model() != model {
            return false;
        }

        provider_id.is_none_or(|provider_id| self.config.model_provider_id == provider_id)
    }

    /// Open a popup to choose the approvals mode (ask for approval policy + sandbox policy).
    #[allow(dead_code)]
    pub(crate) fn open_approvals_popup(&mut self) {
        self.open_approval_mode_popup(true);
    }

    /// Open a popup to choose the permissions mode (approval policy + sandbox policy).
    #[allow(dead_code)]
    pub(crate) fn open_permissions_popup(&mut self) {
        let include_read_only = cfg!(target_os = "windows");
        self.open_approval_mode_popup(include_read_only);
    }

    #[allow(dead_code)]
    fn open_approval_mode_popup(&mut self, include_read_only: bool) {
        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let mut items: Vec<SelectionItem> = Vec::new();
        let presets: Vec<ApprovalPreset> = builtin_approval_presets();

        #[cfg(target_os = "windows")]
        let windows_sandbox_level = WindowsSandboxLevel::from_config(&self.config);
        #[cfg(target_os = "windows")]
        let windows_degraded_sandbox_enabled =
            matches!(windows_sandbox_level, WindowsSandboxLevel::RestrictedToken);
        #[cfg(not(target_os = "windows"))]
        let windows_degraded_sandbox_enabled = false;

        let show_elevate_sandbox_hint =
            crate::product::agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                && windows_degraded_sandbox_enabled
                && presets.iter().any(|preset| preset.id == "auto");

        for preset in presets.into_iter() {
            if !include_read_only && preset.id == "read-only" {
                continue;
            }
            let is_current =
                Self::preset_matches_current(current_approval, current_sandbox, &preset);
            let name = if preset.id == "auto" && windows_degraded_sandbox_enabled {
                "Default (non-elevated sandbox)".to_string()
            } else {
                preset.label.to_string()
            };
            let description = Some(preset.description.to_string());
            let disabled_reason = match self.config.approval_policy.can_set(&preset.approval) {
                Ok(()) => None,
                Err(err) => Some(err.to_string()),
            };
            let requires_confirmation = preset.id == "full-access"
                && !self
                    .config
                    .notices
                    .hide_full_access_warning
                    .unwrap_or(false);
            let actions: Vec<SelectionAction> = if requires_confirmation {
                let preset_clone = preset.clone();
                vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenFullAccessConfirmation {
                        preset: preset_clone.clone(),
                        return_to_permissions: !include_read_only,
                    });
                })]
            } else if preset.id == "auto" {
                #[cfg(target_os = "windows")]
                {
                    if WindowsSandboxLevel::from_config(&self.config)
                        == WindowsSandboxLevel::Disabled
                    {
                        let preset_clone = preset.clone();
                        if crate::product::agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                            && crate::product::agent::windows_sandbox::sandbox_setup_is_complete(
                                self.config.lha_home.as_path(),
                            )
                        {
                            vec![Box::new(move |tx| {
                                tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                                    preset: preset_clone.clone(),
                                    mode: WindowsSandboxEnableMode::Elevated,
                                });
                            })]
                        } else {
                            vec![Box::new(move |tx| {
                                tx.send(AppEvent::OpenWindowsSandboxEnablePrompt {
                                    preset: preset_clone.clone(),
                                });
                            })]
                        }
                    } else if let Some((sample_paths, extra_count, failed_scan)) =
                        self.world_writable_warning_details()
                    {
                        let preset_clone = preset.clone();
                        vec![Box::new(move |tx| {
                            tx.send(AppEvent::OpenWorldWritableWarningConfirmation {
                                preset: Some(preset_clone.clone()),
                                sample_paths: sample_paths.clone(),
                                extra_count,
                                failed_scan,
                            });
                        })]
                    } else {
                        Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
                }
            } else {
                Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
            };
            items.push(SelectionItem {
                name,
                description,
                is_current,
                actions,
                dismiss_on_select: true,
                disabled_reason,
                ..Default::default()
            });
        }

        let footer_note = show_elevate_sandbox_hint.then(|| {
            vec![
                "The non-elevated sandbox protects your files and prevents network access under most circumstances. However, it carries greater risk if prompt injected. To upgrade to the elevated sandbox, run ".dim(),
                "/setup-elevated-sandbox".cyan(),
                ".".dim(),
            ]
            .into()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Update Model Permissions".to_string()),
            footer_note,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(()),
            ..Default::default()
        });
    }

    #[allow(dead_code)]
    fn approval_preset_actions(
        approval: AskForApproval,
        sandbox: SandboxPolicy,
    ) -> Vec<SelectionAction> {
        vec![Box::new(move |tx| {
            let sandbox_clone = sandbox.clone();
            tx.send(AppEvent::CodexOp(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(approval),
                sandbox_policy: Some(sandbox_clone.clone()),
                windows_sandbox_level: None,
                model: None,
                effort: None,
                summary: None,
                identity: None,
                personality: None,
            }));
            tx.send(AppEvent::UpdateAskForApprovalPolicy(approval));
            tx.send(AppEvent::UpdateSandboxPolicy(sandbox_clone));
        })]
    }

    pub(crate) fn preset_matches_current(
        current_approval: AskForApproval,
        current_sandbox: &SandboxPolicy,
        preset: &ApprovalPreset,
    ) -> bool {
        if current_approval != preset.approval {
            return false;
        }
        matches!(
            (&preset.sandbox, current_sandbox),
            (SandboxPolicy::ReadOnly, SandboxPolicy::ReadOnly)
                | (
                    SandboxPolicy::DangerFullAccess,
                    SandboxPolicy::DangerFullAccess
                )
                | (
                    SandboxPolicy::WorkspaceWrite { .. },
                    SandboxPolicy::WorkspaceWrite { .. }
                )
        )
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn world_writable_warning_details(&self) -> Option<(Vec<String>, usize, bool)> {
        if self
            .config
            .notices
            .hide_world_writable_warning
            .unwrap_or(false)
        {
            return None;
        }
        let cwd = self.config.cwd.clone();
        let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
        match crate::product::windows_sandbox::apply_world_writable_scan_and_denies(
            self.config.lha_home.as_path(),
            cwd.as_path(),
            &env_map,
            self.config.sandbox_policy.get(),
            Some(self.config.lha_home.as_path()),
        ) {
            Ok(_) => None,
            Err(_) => Some((Vec::new(), 0, true)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn world_writable_warning_details(&self) -> Option<(Vec<String>, usize, bool)> {
        None
    }

    #[allow(dead_code)]
    pub(crate) fn open_full_access_confirmation(
        &mut self,
        preset: ApprovalPreset,
        return_to_permissions: bool,
    ) {
        let approval = preset.approval;
        let sandbox = preset.sandbox;
        let mut header_children: Vec<Box<dyn Renderable>> = Vec::new();
        let title_line = Line::from("Enable full access?").bold();
        let info_line = Line::from(vec![
            "When LHA runs with full access, it can edit any file on your computer and run commands with network, without your approval. "
                .into(),
            "Exercise caution when enabling full access. This significantly increases the risk of data loss, leaks, or unexpected behavior."
                .fg(Color::Red),
        ]);
        header_children.push(Box::new(title_line));
        header_children.push(Box::new(
            Paragraph::new(vec![info_line]).wrap(Wrap { trim: false }),
        ));
        let header = ColumnRenderable::with(header_children);

        let mut accept_actions = Self::approval_preset_actions(approval, sandbox.clone());
        accept_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateFullAccessWarningAcknowledged(true));
        }));

        let mut accept_and_remember_actions = Self::approval_preset_actions(approval, sandbox);
        accept_and_remember_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateFullAccessWarningAcknowledged(true));
            tx.send(AppEvent::PersistFullAccessWarningAcknowledged);
        }));

        let deny_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            if return_to_permissions {
                tx.send(AppEvent::OpenPermissionsPopup);
            } else {
                tx.send(AppEvent::OpenApprovalsPopup);
            }
        })];

        let items = vec![
            SelectionItem {
                name: "Yes, continue anyway".to_string(),
                description: Some("Apply full access for this session".to_string()),
                actions: accept_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Yes, and don't ask again".to_string(),
                description: Some("Enable full access and remember this choice".to_string()),
                actions: accept_and_remember_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                description: Some("Go back without enabling full access".to_string()),
                actions: deny_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(target_os = "windows")]
    #[allow(dead_code)]
    pub(crate) fn open_world_writable_warning_confirmation(
        &mut self,
        preset: Option<ApprovalPreset>,
        sample_paths: Vec<String>,
        extra_count: usize,
        failed_scan: bool,
    ) {
        let (approval, sandbox) = match &preset {
            Some(p) => (Some(p.approval), Some(p.sandbox.clone())),
            None => (None, None),
        };
        let mut header_children: Vec<Box<dyn Renderable>> = Vec::new();
        let describe_policy = |policy: &SandboxPolicy| match policy {
            SandboxPolicy::WorkspaceWrite { .. } => "Agent mode",
            SandboxPolicy::ReadOnly => "Read-Only mode",
            _ => "Agent mode",
        };
        let mode_label = preset
            .as_ref()
            .map(|p| describe_policy(&p.sandbox))
            .unwrap_or_else(|| describe_policy(self.config.sandbox_policy.get()));
        let info_line = if failed_scan {
            Line::from(vec![
                "We couldn't complete the world-writable scan, so protections cannot be verified. "
                    .into(),
                format!("The Windows sandbox cannot guarantee protection in {mode_label}.")
                    .fg(Color::Red),
            ])
        } else {
            Line::from(vec![
                "The Windows sandbox cannot protect writes to folders that are writable by Everyone.".into(),
                " Consider removing write access for Everyone from the following folders:".into(),
            ])
        };
        header_children.push(Box::new(
            Paragraph::new(vec![info_line]).wrap(Wrap { trim: false }),
        ));

        if !sample_paths.is_empty() {
            // Show up to three examples and optionally an "and X more" line.
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            for p in &sample_paths {
                lines.push(Line::from(format!("  - {p}")));
            }
            if extra_count > 0 {
                lines.push(Line::from(format!("and {extra_count} more")));
            }
            header_children.push(Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })));
        }
        let header = ColumnRenderable::with(header_children);

        // Build actions ensuring acknowledgement happens before applying the new sandbox policy,
        // so downstream policy-change hooks don't re-trigger the warning.
        let mut accept_actions: Vec<SelectionAction> = Vec::new();
        // Suppress the immediate re-scan only when a preset will be applied (i.e., via /approvals),
        // to avoid duplicate warnings from the ensuing policy change.
        if preset.is_some() {
            accept_actions.push(Box::new(|tx| {
                tx.send(AppEvent::SkipNextWorldWritableScan);
            }));
        }
        if let (Some(approval), Some(sandbox)) = (approval, sandbox.clone()) {
            accept_actions.extend(Self::approval_preset_actions(approval, sandbox));
        }

        let mut accept_and_remember_actions: Vec<SelectionAction> = Vec::new();
        accept_and_remember_actions.push(Box::new(|tx| {
            tx.send(AppEvent::UpdateWorldWritableWarningAcknowledged(true));
            tx.send(AppEvent::PersistWorldWritableWarningAcknowledged);
        }));
        if let (Some(approval), Some(sandbox)) = (approval, sandbox) {
            accept_and_remember_actions.extend(Self::approval_preset_actions(approval, sandbox));
        }

        let items = vec![
            SelectionItem {
                name: "Continue".to_string(),
                description: Some(format!("Apply {mode_label} for this session")),
                actions: accept_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Continue and don't warn again".to_string(),
                description: Some(format!("Enable {mode_label} and remember this choice")),
                actions: accept_and_remember_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn open_world_writable_warning_confirmation(
        &mut self,
        _preset: Option<ApprovalPreset>,
        _sample_paths: Vec<String>,
        _extra_count: usize,
        _failed_scan: bool,
    ) {
    }

    #[cfg(target_os = "windows")]
    #[allow(dead_code)]
    pub(crate) fn open_windows_sandbox_enable_prompt(&mut self, preset: ApprovalPreset) {
        use ratatui_macros::line;

        if !crate::product::agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED {
            // Legacy flow (pre-NUX): explain the experimental sandbox and let the user enable it
            // directly (no elevation prompts).
            let mut header = ColumnRenderable::new();
            header.push(*Box::new(
                Paragraph::new(vec![
                    line!["Agent mode on Windows uses an experimental sandbox to limit network and filesystem access.".bold()],
                    line!["Learn more: https://developers.openai.com/codex/windows"],
                ])
                .wrap(Wrap { trim: false }),
            ));

            let preset_clone = preset;
            let items = vec![
                SelectionItem {
                    name: "Enable experimental sandbox".to_string(),
                    description: None,
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                            preset: preset_clone.clone(),
                            mode: WindowsSandboxEnableMode::Legacy,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Go back".to_string(),
                    description: None,
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::OpenApprovalsPopup);
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ];

            self.bottom_pane.show_selection_view(SelectionViewParams {
                title: None,
                footer_hint: Some(standard_popup_hint_line()),
                items,
                header: Box::new(header),
                ..Default::default()
            });
            return;
        }

        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let presets = builtin_approval_presets();
        let stay_full_access = presets
            .iter()
            .find(|preset| preset.id == "full-access")
            .is_some_and(|preset| {
                Self::preset_matches_current(current_approval, current_sandbox, preset)
            });
        self.otel_manager
            .counter("lha.windows_sandbox.elevated_prompt_shown", 1, &[]);

        let mut header = ColumnRenderable::new();
        header.push(*Box::new(
            Paragraph::new(vec![
                line!["Set Up Agent Sandbox".bold()],
                line![""],
                line!["Agent mode uses an experimental Windows sandbox that protects your files and prevents network access by default."],
                line!["Learn more: https://developers.openai.com/codex/windows"],
            ])
            .wrap(Wrap { trim: false }),
        ));

        let stay_label = if stay_full_access {
            "Stay in Agent Full Access".to_string()
        } else {
            "Stay in Read-Only".to_string()
        };
        let mut stay_actions = if stay_full_access {
            Vec::new()
        } else {
            presets
                .iter()
                .find(|preset| preset.id == "read-only")
                .map(|preset| {
                    Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
                })
                .unwrap_or_default()
        };
        stay_actions.insert(
            0,
            Box::new({
                let otel = self.otel_manager.clone();
                move |_tx| {
                    otel.counter("lha.windows_sandbox.elevated_prompt_decline", 1, &[]);
                }
            }),
        );

        let accept_otel = self.otel_manager.clone();
        let items = vec![
            SelectionItem {
                name: "Set up agent sandbox (requires elevation)".to_string(),
                description: None,
                actions: vec![Box::new(move |tx| {
                    accept_otel.counter("lha.windows_sandbox.elevated_prompt_accept", 1, &[]);
                    tx.send(AppEvent::BeginWindowsSandboxElevatedSetup {
                        preset: preset.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: stay_label,
                description: None,
                actions: stay_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn open_windows_sandbox_enable_prompt(&mut self, _preset: ApprovalPreset) {}

    #[cfg(target_os = "windows")]
    #[allow(dead_code)]
    pub(crate) fn open_windows_sandbox_fallback_prompt(
        &mut self,
        preset: ApprovalPreset,
        reason: WindowsSandboxFallbackReason,
    ) {
        use ratatui_macros::line;

        let _ = reason;

        let current_approval = self.config.approval_policy.value();
        let current_sandbox = self.config.sandbox_policy.get();
        let presets = builtin_approval_presets();
        let stay_full_access = presets
            .iter()
            .find(|preset| preset.id == "full-access")
            .is_some_and(|preset| {
                Self::preset_matches_current(current_approval, current_sandbox, preset)
            });
        let mut lines = Vec::new();
        lines.push(line!["Use Non-Elevated Sandbox?".bold()]);
        lines.push(line![""]);
        lines.push(line![
            "Elevation failed. You can also use a non-elevated sandbox, which protects your files and prevents network access under most circumstances. However, it carries greater risk if prompt injected."
        ]);
        lines.push(line![
            "Learn more: https://developers.openai.com/codex/windows"
        ]);

        let mut header = ColumnRenderable::new();
        header.push(*Box::new(Paragraph::new(lines).wrap(Wrap { trim: false })));

        let elevated_preset = preset.clone();
        let legacy_preset = preset;
        let stay_label = if stay_full_access {
            "Stay in Agent Full Access".to_string()
        } else {
            "Stay in Read-Only".to_string()
        };
        let mut stay_actions = if stay_full_access {
            Vec::new()
        } else {
            presets
                .iter()
                .find(|preset| preset.id == "read-only")
                .map(|preset| {
                    Self::approval_preset_actions(preset.approval, preset.sandbox.clone())
                })
                .unwrap_or_default()
        };
        stay_actions.insert(
            0,
            Box::new({
                let otel = self.otel_manager.clone();
                move |_tx| {
                    otel.counter("lha.windows_sandbox.fallback_stay_current", 1, &[]);
                }
            }),
        );
        let items = vec![
            SelectionItem {
                name: "Try elevated agent sandbox setup again".to_string(),
                description: None,
                actions: vec![Box::new({
                    let otel = self.otel_manager.clone();
                    let preset = elevated_preset;
                    move |tx| {
                        otel.counter("lha.windows_sandbox.fallback_retry_elevated", 1, &[]);
                        tx.send(AppEvent::BeginWindowsSandboxElevatedSetup {
                            preset: preset.clone(),
                        });
                    }
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Use non-elevated agent sandbox".to_string(),
                description: None,
                actions: vec![Box::new({
                    let otel = self.otel_manager.clone();
                    let preset = legacy_preset;
                    move |tx| {
                        otel.counter("lha.windows_sandbox.fallback_use_legacy", 1, &[]);
                        tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                            preset: preset.clone(),
                            mode: WindowsSandboxEnableMode::Legacy,
                        });
                    }
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: stay_label,
                description: None,
                actions: stay_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn open_windows_sandbox_fallback_prompt(
        &mut self,
        _preset: ApprovalPreset,
        _reason: WindowsSandboxFallbackReason,
    ) {
    }

    #[cfg(target_os = "windows")]
    #[allow(dead_code)]
    pub(crate) fn maybe_prompt_windows_sandbox_enable(&mut self) {
        if self.config.forced_auto_mode_downgraded_on_windows
            && WindowsSandboxLevel::from_config(&self.config) == WindowsSandboxLevel::Disabled
            && let Some(preset) = builtin_approval_presets()
                .into_iter()
                .find(|preset| preset.id == "auto")
        {
            self.open_windows_sandbox_enable_prompt(preset);
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn maybe_prompt_windows_sandbox_enable(&mut self) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn show_windows_sandbox_setup_status(&mut self) {
        // While elevated sandbox setup runs, prevent typing so the user doesn't
        // accidentally queue messages that will run under an unexpected mode.
        self.bottom_pane.set_composer_input_enabled(
            false,
            Some("Input disabled until setup completes.".to_string()),
        );
        self.bottom_pane.ensure_status_indicator();
        self.bottom_pane.set_interrupt_hint_visible(false);
        self.set_status_header("Setting up agent sandbox. This can take a minute.".to_string());
        self.request_redraw();
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn show_windows_sandbox_setup_status(&mut self) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn clear_windows_sandbox_setup_status(&mut self) {
        self.bottom_pane.set_composer_input_enabled(true, None);
        self.bottom_pane.hide_status_indicator();
        self.request_redraw();
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn clear_windows_sandbox_setup_status(&mut self) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn clear_forced_auto_mode_downgrade(&mut self) {
        self.config.forced_auto_mode_downgraded_on_windows = false;
    }

    #[cfg(not(target_os = "windows"))]
    #[allow(dead_code)]
    pub(crate) fn clear_forced_auto_mode_downgrade(&mut self) {}

    /// Set the approval policy in the widget's config copy.
    pub(crate) fn set_approval_policy(&mut self, policy: AskForApproval) {
        if let Err(err) = self.config.approval_policy.set(policy) {
            tracing::warn!(%err, "failed to set approval_policy on chat config");
        }
    }

    /// Set the sandbox policy in the widget's config copy.
    pub(crate) fn set_sandbox_policy(&mut self, policy: SandboxPolicy) -> ConstraintResult<()> {
        #[cfg(target_os = "windows")]
        let should_clear_downgrade = !matches!(&policy, SandboxPolicy::ReadOnly)
            || WindowsSandboxLevel::from_config(&self.config) != WindowsSandboxLevel::Disabled;

        self.config.sandbox_policy.set(policy)?;

        #[cfg(target_os = "windows")]
        if should_clear_downgrade {
            self.config.forced_auto_mode_downgraded_on_windows = false;
        }

        Ok(())
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn set_feature_enabled(&mut self, feature: Feature, enabled: bool) {
        if enabled {
            self.config.features.enable(feature);
        } else {
            self.config.features.disable(feature);
        }
        if feature == Feature::Steer {
            self.bottom_pane.set_steer_enabled(enabled);
        }
        if feature == Feature::Identities {
            self.bottom_pane.set_identities_enabled(enabled);
            self.current_identity = self.identity_without_preset();
            self.active_identity_mask = None;
            if !enabled {
                self.sync_identity_to_runtime(self.identity_without_preset());
            }
            self.update_identity_indicator();
            self.refresh_model_display();
            self.update_footer_info();
            self.request_redraw();
        }
        if feature == Feature::Personality {
            self.sync_personality_command_enabled();
        }
        if feature == Feature::PreventIdleSleep {
            self.turn_sleep_inhibitor = SleepInhibitor::new(enabled);
            self.turn_sleep_inhibitor
                .set_turn_running(self.agent_turn_running);
        }
        #[cfg(target_os = "windows")]
        if matches!(
            feature,
            Feature::WindowsSandbox | Feature::WindowsSandboxElevated
        ) {
            self.bottom_pane.set_windows_degraded_sandbox_active(
                crate::product::agent::windows_sandbox::ELEVATED_SANDBOX_NUX_ENABLED
                    && matches!(
                        WindowsSandboxLevel::from_config(&self.config),
                        WindowsSandboxLevel::RestrictedToken
                    ),
            );
        }
    }

    pub(crate) fn set_memories_config(
        &mut self,
        memories: crate::product::agent::config::types::MemoriesConfig,
    ) {
        self.config.memories = memories;
    }

    pub(crate) fn open_memories_settings_view(&mut self) {
        self.dismiss_active_view();
        let params = crate::product::tui_app::bottom_pane::memories_settings_params(&self.config);
        self.bottom_pane.show_selection_view(params);
        self.request_redraw();
    }

    pub(crate) fn set_full_access_warning_acknowledged(&mut self, acknowledged: bool) {
        self.config.notices.hide_full_access_warning = Some(acknowledged);
    }

    pub(crate) fn set_world_writable_warning_acknowledged(&mut self, acknowledged: bool) {
        self.config.notices.hide_world_writable_warning = Some(acknowledged);
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn world_writable_warning_hidden(&self) -> bool {
        self.config
            .notices
            .hide_world_writable_warning
            .unwrap_or(false)
    }

    /// Set the reasoning effort in the stored identity.
    pub(crate) fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.current_identity = self.current_identity.with_updates(None, Some(effort), None);
        if self.identities_enabled()
            && let Some(mask) = self.active_identity_mask.as_mut()
        {
            mask.reasoning_effort = Some(effort);
            if let Some(mode) = mask
                .kind
                .filter(|mode| Self::stores_effort_override_for(*mode))
            {
                self.reasoning_effort_overrides.insert(mode, effort);
            }
        }
        self.update_footer_info();
        self.request_redraw();
    }

    /// Set the personality in the widget's config copy.
    pub(crate) fn set_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
    }

    pub(crate) fn sync_provider_config(&mut self, config: &Config, update_active_provider: bool) {
        self.config.config_layer_stack = config.config_layer_stack.clone();
        self.config.model = config.model.clone();
        self.config.model_context_window = config.model_context_window;
        self.config.model_auto_compact_token_limit = config.model_auto_compact_token_limit;
        self.config.model_providers = config.model_providers.clone();
        self.config.provider_config_required = config.provider_config_required;
        if update_active_provider {
            self.config.model_provider_id = config.model_provider_id.clone();
            self.config.model_provider = config.model_provider.clone();
        }
        self.update_footer_info();
        self.request_redraw();
    }

    pub(crate) fn dismiss_active_view(&mut self) {
        self.bottom_pane.dismiss_active_view();
        self.request_redraw();
    }

    /// Set the model in the widget's config copy and stored identity.
    pub(crate) fn set_model(&mut self, model: &str) {
        self.current_identity =
            self.current_identity
                .with_updates(Some(model.to_string()), None, None);
        if self.identities_enabled()
            && let Some(mask) = self.active_identity_mask.as_mut()
        {
            mask.model = Some(model.to_string());
        }
        self.refresh_model_display();
        self.update_footer_info();
        self.request_redraw();
    }

    pub(crate) fn current_model(&self) -> &str {
        if !self.identities_enabled() {
            return self.current_identity.model();
        }
        self.active_identity_mask
            .as_ref()
            .and_then(|mask| mask.model.as_deref())
            .unwrap_or_else(|| self.current_identity.model())
    }

    fn sync_personality_command_enabled(&mut self) {
        self.sync_personality_command_enabled_with_redraw(true);
    }

    fn sync_personality_command_enabled_with_redraw(&mut self, request_redraw: bool) {
        let enabled = self.config.features.enabled(Feature::Personality);
        if request_redraw {
            self.bottom_pane.set_personality_command_enabled(enabled);
        } else {
            self.bottom_pane
                .set_personality_command_enabled_without_redraw(enabled);
        }
    }

    fn current_model_supports_personality(&self) -> bool {
        let model = self.current_model();
        self.thread_manager
            .try_list_picker_models(&self.config)
            .ok()
            .and_then(|models| {
                models
                    .into_iter()
                    .find(|preset| preset.model == model)
                    .map(|preset| preset.supports_personality)
            })
            .unwrap_or(false)
    }

    #[allow(dead_code)] // Used in tests
    pub(crate) fn current_identity(&self) -> &Identity {
        &self.current_identity
    }

    #[cfg(test)]
    pub(crate) fn current_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        self.effective_reasoning_effort()
    }

    pub(crate) fn no_modal_or_popup_active(&self) -> bool {
        self.bottom_pane.no_modal_or_popup_active()
    }

    #[cfg(test)]
    pub(crate) fn has_initial_user_message(&self) -> bool {
        self.initial_user_message.is_some()
    }

    pub(crate) fn is_session_configured(&self) -> bool {
        self.thread_id.is_some()
    }

    fn identities_enabled(&self) -> bool {
        self.config.features.enabled(Feature::Identities)
    }

    fn initial_custom_mode(
        model: String,
        reasoning_effort: Option<ReasoningEffortConfig>,
    ) -> Identity {
        Identity {
            kind: IdentityKind::Nobody,
            settings: Settings {
                model,
                reasoning_effort,
                developer_instructions: None,
            },
        }
    }

    fn initial_identity_mask(
        config: &Config,
        thread_manager: &ThreadManager,
        model_override: Option<&str>,
    ) -> Option<IdentityMask> {
        if !config.features.enabled(Feature::Identities) {
            return None;
        }
        let mut mask = match config.last_selected_identity {
            Some(kind) => identities::mask_for_kind(thread_manager, kind)
                .or_else(|| identities::default_mask(thread_manager))?,
            None => identities::default_mask(thread_manager)?,
        };
        if let Some(model_override) = model_override {
            mask.model = Some(model_override.to_string());
        }
        Some(mask)
    }

    fn set_restored_identity_kind_from_session(
        &mut self,
        kind: IdentityKind,
        request_redraw: bool,
    ) {
        self.current_identity.kind = kind;
        self.active_identity_mask = None;
        if request_redraw {
            self.bottom_pane.set_buddy_identity_kind(kind);
        } else {
            self.bottom_pane
                .set_buddy_identity_kind_without_redraw(kind);
        }
        self.update_identity_indicator_with_redraw(request_redraw);
        self.refresh_model_display();
        self.update_footer_info_with_redraw(request_redraw);
        if request_redraw {
            self.request_redraw();
        }
    }

    fn active_identity_kind(&self) -> IdentityKind {
        self.active_identity_mask
            .as_ref()
            .and_then(|mask| mask.kind)
            .unwrap_or(self.current_identity.kind)
    }

    pub(crate) fn active_identity_kind_for_ui(&self) -> IdentityKind {
        self.active_identity_kind()
    }

    fn stores_effort_override_for(mode: IdentityKind) -> bool {
        matches!(mode, IdentityKind::Planner | IdentityKind::Programmer)
    }

    fn apply_reasoning_effort_override(&self, mut mask: IdentityMask) -> IdentityMask {
        if let Some(effort) = self.reasoning_effort_override_for_mask(&mask) {
            mask.reasoning_effort = Some(effort);
        }
        mask
    }

    fn reasoning_effort_override_for_mask(
        &self,
        mask: &IdentityMask,
    ) -> Option<Option<ReasoningEffortConfig>> {
        let mode = mask
            .kind
            .filter(|mode| Self::stores_effort_override_for(*mode))?;
        self.reasoning_effort_overrides.get(&mode).copied()
    }

    fn effective_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        if !self.identities_enabled() {
            return self.current_identity.reasoning_effort();
        }
        let current_effort = self.current_identity.reasoning_effort();
        self.active_identity_mask
            .as_ref()
            .and_then(|mask| mask.reasoning_effort)
            .unwrap_or(current_effort)
    }

    fn identity_without_preset(&self) -> Identity {
        let mut identity = self.current_identity.clone();
        identity.kind = IdentityKind::Nobody;
        identity.settings.developer_instructions = None;
        identity
    }

    fn effective_identity(&self) -> Identity {
        if !self.identities_enabled() {
            return self.identity_without_preset();
        }
        self.active_identity_mask.as_ref().map_or_else(
            || self.current_identity.clone(),
            |mask| self.current_identity.apply_mask(mask),
        )
    }

    fn refresh_model_display(&mut self) {
        let effective = self.effective_identity();
        self.session_header.set_model(effective.model());
    }

    fn model_display_name(&self) -> &str {
        let model = self.current_model();
        if model.is_empty() {
            DEFAULT_MODEL_DISPLAY_NAME
        } else {
            model
        }
    }

    /// Get the label for the current identity.
    fn identity_label(&self) -> Option<&'static str> {
        if !self.identities_enabled() {
            return None;
        }
        match self.active_identity_kind() {
            IdentityKind::Nobody => Some("nobody"),
            IdentityKind::Planner => Some("planner"),
            IdentityKind::Programmer => Some("programmer"),
            IdentityKind::Explorer => Some("explorer"),
            IdentityKind::Reviewer => Some("reviewer"),
        }
    }

    fn identity_indicator(&self) -> Option<IdentityIndicator> {
        if !self.identities_enabled() {
            return None;
        }
        match self.active_identity_kind() {
            IdentityKind::Nobody => Some(IdentityIndicator::Nobody),
            IdentityKind::Planner => Some(IdentityIndicator::Planner),
            IdentityKind::Programmer => Some(IdentityIndicator::Programmer),
            IdentityKind::Explorer => Some(IdentityIndicator::Explorer),
            IdentityKind::Reviewer => Some(IdentityIndicator::Reviewer),
        }
    }

    fn update_identity_indicator(&mut self) {
        self.update_identity_indicator_with_redraw(true);
    }

    fn update_identity_indicator_with_redraw(&mut self, request_redraw: bool) {
        let indicator = self.identity_indicator();
        if request_redraw {
            self.bottom_pane.set_identity_indicator(indicator);
        } else {
            self.bottom_pane
                .set_identity_indicator_without_redraw(indicator);
        }
    }

    fn update_footer_info(&mut self) {
        self.update_footer_info_with_redraw(true);
    }

    fn update_footer_info_with_redraw(&mut self, request_redraw: bool) {
        let reasoning_effort = self
            .effective_reasoning_effort()
            .map(Self::reasoning_effort_label)
            .map(str::to_string);
        if request_redraw {
            self.bottom_pane.set_footer_info(
                self.model_display_name().to_string(),
                reasoning_effort,
                format_directory_display(&self.config.cwd, None),
                self.git_branch.clone(),
            );
        } else {
            self.bottom_pane.set_footer_info_without_redraw(
                self.model_display_name().to_string(),
                reasoning_effort,
                format_directory_display(&self.config.cwd, None),
                self.git_branch.clone(),
            );
        }
    }

    pub(crate) fn set_git_branch(&mut self, git_branch: Option<String>) {
        if self.git_branch == git_branch {
            return;
        }
        self.git_branch = git_branch;
        self.update_footer_info();
    }

    #[cfg(test)]
    pub(crate) fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Update the active identity mask.
    ///
    /// When identities are enabled, the current identity is attached to submissions as
    /// `Op::UserTurn { identity: Some(...) }`.
    pub(crate) fn set_identity_mask(&mut self, mask: IdentityMask) {
        if !self.identities_enabled() {
            return;
        }
        let mask = identities::normalize_mask(mask);
        self.active_identity_mask = Some(self.apply_reasoning_effort_override(mask));
        self.bottom_pane
            .set_buddy_identity_kind(self.active_identity_kind());
        self.update_identity_indicator();
        self.refresh_model_display();
        self.update_footer_info();
        self.request_redraw();
    }

    fn sync_identity_to_runtime(&mut self, identity: Identity) {
        if !self.is_session_configured() {
            return;
        }

        self.submit_op(Op::OverrideTurnContext {
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
    }

    pub(crate) fn sync_active_identity_to_runtime(&mut self) {
        if !self.is_session_configured() || !self.identities_enabled() {
            return;
        }
        if self.active_identity_mask.is_none() {
            return;
        }

        self.sync_identity_to_runtime(self.effective_identity());
    }

    pub(crate) fn request_redraw_for_ui_change(&self) {
        self.request_redraw_with_risky_row_repair();
    }

    pub(crate) fn frame_requester(&self) -> FrameRequester {
        self.frame_requester.clone()
    }

    fn connectors_enabled(&self) -> bool {
        self.config.features.enabled(Feature::Apps)
    }

    fn connectors_for_mentions(&self) -> Option<&[connectors::AppInfo]> {
        if !self.connectors_enabled() {
            return None;
        }

        match &self.connectors_cache {
            ConnectorsCacheState::Ready(snapshot) => Some(snapshot.connectors.as_slice()),
            _ => None,
        }
    }

    /// Build a placeholder header cell while the session is configuring.
    fn placeholder_session_header_cell(config: &Config) -> Box<dyn HistoryCell> {
        let placeholder_style = Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC);
        let placeholder_model = if config.provider_config_required {
            "No provider configured".to_string()
        } else {
            DEFAULT_MODEL_DISPLAY_NAME.to_string()
        };
        Box::new(history_cell::SessionHeaderHistoryCell::new_with_style(
            placeholder_model,
            placeholder_style,
            None,
            config.cwd.clone(),
            CODEX_CLI_VERSION,
        ))
    }

    /// Merge the real session info cell with any placeholder header to avoid double boxes.
    fn apply_session_info_cell(&mut self, cell: history_cell::SessionInfoCell) {
        let mut session_info_cell = Some(Box::new(cell) as Box<dyn HistoryCell>);
        let merged_header = if let Some(active) = self.active_cell.take() {
            if active
                .as_any()
                .is::<history_cell::SessionHeaderHistoryCell>()
            {
                // Reuse the existing placeholder header to avoid rendering two boxes.
                if let Some(cell) = session_info_cell.take() {
                    self.active_cell = Some(cell);
                }
                true
            } else {
                self.active_cell = Some(active);
                false
            }
        } else {
            false
        };

        self.flush_active_cell();

        if !merged_header && let Some(cell) = session_info_cell {
            self.add_boxed_history(cell);
        }
    }

    pub(crate) fn add_info_message(&mut self, message: String, hint: Option<String>) {
        self.add_to_history(history_cell::new_info_event(message, hint));
        self.request_redraw();
    }

    pub(crate) fn add_plain_history_lines(&mut self, lines: Vec<Line<'static>>) {
        self.add_boxed_history(Box::new(PlainHistoryCell::new(lines)));
        self.request_redraw();
    }

    pub(crate) fn add_error_message(&mut self, message: String) {
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
    }

    fn rename_confirmation_cell(name: &str, thread_id: Option<ThreadId>) -> PlainHistoryCell {
        let resume_cmd = crate::product::agent::util::resume_command(Some(name), thread_id)
            .unwrap_or_else(|| format!("lha resume {name}"));
        let name = name.to_string();
        let line = vec![
            "• ".into(),
            "Thread renamed to ".into(),
            name.cyan(),
            ", to resume this thread run ".into(),
            resume_cmd.cyan(),
        ];
        PlainHistoryCell::new(vec![line.into()])
    }

    #[allow(dead_code)]
    pub(crate) fn add_connectors_output(&mut self) {
        if !self.connectors_enabled() {
            self.add_info_message(
                "Apps are disabled.".to_string(),
                Some("Enable the apps feature to use $ or /apps.".to_string()),
            );
            return;
        }

        match self.connectors_cache.clone() {
            ConnectorsCacheState::Ready(snapshot) => {
                if snapshot.connectors.is_empty() {
                    self.add_info_message("No apps available.".to_string(), None);
                } else {
                    self.open_connectors_popup(&snapshot.connectors);
                }
            }
            ConnectorsCacheState::Failed(err) => {
                self.add_to_history(history_cell::new_error_event(err));
                // Retry on demand so `/apps` can recover after transient failures.
                self.prefetch_connectors();
            }
            ConnectorsCacheState::Loading => {
                self.add_to_history(history_cell::new_info_event(
                    "Apps are still loading.".to_string(),
                    Some("Try again in a moment.".to_string()),
                ));
            }
            ConnectorsCacheState::Uninitialized => {
                self.prefetch_connectors();
                self.add_to_history(history_cell::new_info_event(
                    "Apps are still loading.".to_string(),
                    Some("Try again in a moment.".to_string()),
                ));
            }
        }
        self.request_redraw();
    }

    #[allow(dead_code)]
    fn open_connectors_popup(&mut self, connectors: &[connectors::AppInfo]) {
        let total = connectors.len();
        let installed = connectors
            .iter()
            .filter(|connector| connector.is_accessible)
            .count();
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Apps".bold()));
        header.push(Line::from(
            "Use $ to insert an installed app into your prompt.".dim(),
        ));
        header.push(Line::from(
            format!("Installed {installed} of {total} available apps.").dim(),
        ));
        let mut items: Vec<SelectionItem> = Vec::with_capacity(connectors.len());
        for connector in connectors {
            let connector_label = connectors::connector_display_label(connector);
            let connector_title = connector_label.clone();
            let link_description = Self::connector_description(connector);
            let description = Self::connector_brief_description(connector);
            let search_value = format!("{connector_label} {}", connector.id);
            let mut item = SelectionItem {
                name: connector_label,
                description: Some(description),
                search_value: Some(search_value),
                ..Default::default()
            };
            let is_installed = connector.is_accessible;
            let (selected_label, missing_label, instructions) = if connector.is_accessible {
                (
                    "Press Enter to view the app link.",
                    "App link unavailable.",
                    "Manage this app in your browser.",
                )
            } else {
                (
                    "Press Enter to view the install link.",
                    "Install link unavailable.",
                    "Install this app in your browser, then reload LHA.",
                )
            };
            if let Some(install_url) = connector.install_url.clone() {
                let title = connector_title.clone();
                let instructions = instructions.to_string();
                let description = link_description.clone();
                item.actions = vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenAppLink {
                        title: title.clone(),
                        description: description.clone(),
                        instructions: instructions.clone(),
                        url: install_url.clone(),
                        is_installed,
                    });
                })];
                item.dismiss_on_select = true;
                item.selected_description = Some(selected_label.to_string());
            } else {
                item.actions = vec![Box::new(move |tx| {
                    tx.send_history_cell(Box::new(history_cell::new_info_event(
                        missing_label.to_string(),
                        None,
                    )));
                })];
                item.dismiss_on_select = true;
                item.selected_description = Some(missing_label.to_string());
            }
            items.push(item);
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(Self::connectors_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search apps".to_string()),
            ..Default::default()
        });
    }

    #[allow(dead_code)]
    fn connectors_popup_hint_line() -> Line<'static> {
        Line::from(vec![
            "Press ".into(),
            key_hint::plain(KeyCode::Esc).into(),
            " to close.".into(),
        ])
    }

    #[allow(dead_code)]
    fn connector_brief_description(connector: &connectors::AppInfo) -> String {
        let status_label = if connector.is_accessible {
            "Connected"
        } else {
            "Can be installed"
        };
        match Self::connector_description(connector) {
            Some(description) => format!("{status_label} · {description}"),
            None => status_label.to_string(),
        }
    }

    #[allow(dead_code)]
    fn connector_description(connector: &connectors::AppInfo) -> Option<String> {
        connector
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_string)
    }

    /// Forward file-search results to the bottom pane.
    pub(crate) fn apply_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        self.bottom_pane.on_file_search_result(query, matches);
    }

    /// Handles a Ctrl+C press at the chat-widget layer.
    ///
    /// The first press arms a time-bounded quit shortcut and shows a footer hint via the bottom
    /// pane. If cancellable work is active, Ctrl+C also submits `Op::Interrupt` after the shortcut
    /// is armed.
    ///
    /// If the same quit shortcut is pressed again before expiry, this requests a shutdown-first
    /// quit.
    fn on_ctrl_c(&mut self) {
        if self.config.provider_config_required {
            self.quit_shortcut_expires_at = None;
            self.quit_shortcut_key = None;
            self.request_immediate_exit();
            return;
        }

        let key = key_hint::ctrl(KeyCode::Char('c'));
        let modal_or_popup_active = !self.bottom_pane.no_modal_or_popup_active();
        if self.bottom_pane.on_ctrl_c() == CancellationEvent::Handled {
            if DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
                if modal_or_popup_active {
                    self.quit_shortcut_expires_at = None;
                    self.quit_shortcut_key = None;
                    self.bottom_pane.clear_quit_shortcut_hint();
                } else {
                    self.arm_quit_shortcut(key);
                }
            }
            return;
        }

        if !DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
            if self.is_cancellable_work_active() {
                self.submit_op(Op::Interrupt);
            } else {
                self.request_quit_without_confirmation();
            }
            return;
        }

        if self.quit_shortcut_active_for(key) {
            self.quit_shortcut_expires_at = None;
            self.quit_shortcut_key = None;
            self.request_quit_without_confirmation();
            return;
        }

        self.arm_quit_shortcut(key);

        if self.is_cancellable_work_active() {
            self.submit_op(Op::Interrupt);
        }
    }

    /// Handles a Ctrl+D press at the chat-widget layer.
    ///
    /// Ctrl-D only participates in quit when the composer is empty and no modal/popup is active.
    /// Otherwise it should be routed to the active view and not attempt to quit.
    fn on_ctrl_d(&mut self) -> bool {
        let key = key_hint::ctrl(KeyCode::Char('d'));
        if !DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
            if !self.bottom_pane.composer_is_empty() || !self.bottom_pane.no_modal_or_popup_active()
            {
                return false;
            }

            self.request_quit_without_confirmation();
            return true;
        }

        if self.quit_shortcut_active_for(key) {
            self.quit_shortcut_expires_at = None;
            self.quit_shortcut_key = None;
            self.request_quit_without_confirmation();
            return true;
        }

        if !self.bottom_pane.composer_is_empty() || !self.bottom_pane.no_modal_or_popup_active() {
            return false;
        }

        self.arm_quit_shortcut(key);
        true
    }

    /// True if `key` matches the armed quit shortcut and the window has not expired.
    fn quit_shortcut_active_for(&self, key: KeyBinding) -> bool {
        self.quit_shortcut_key == Some(key)
            && self
                .quit_shortcut_expires_at
                .is_some_and(|expires_at| Instant::now() < expires_at)
    }

    /// Arm the double-press quit shortcut and show the footer hint.
    ///
    /// This keeps the state machine (`quit_shortcut_*`) in `ChatWidget`, since
    /// it is the component that interprets Ctrl+C vs Ctrl+D and decides whether
    /// quitting is currently allowed, while delegating rendering to `BottomPane`.
    fn arm_quit_shortcut(&mut self, key: KeyBinding) {
        self.quit_shortcut_expires_at = Instant::now()
            .checked_add(QUIT_SHORTCUT_TIMEOUT)
            .or_else(|| Some(Instant::now()));
        self.quit_shortcut_key = Some(key);
        self.bottom_pane.show_quit_shortcut_hint(key);
    }

    // Review mode counts as cancellable work so Ctrl+C interrupts instead of quitting.
    fn is_cancellable_work_active(&self) -> bool {
        self.bottom_pane.is_task_running() || self.is_review_mode
    }

    pub(crate) fn composer_is_empty(&self) -> bool {
        self.bottom_pane.composer_is_empty()
    }

    pub(crate) fn submit_user_message_with_mode(&mut self, text: String, identity: IdentityMask) {
        self.set_identity_mask(identity);
        self.submit_user_message(text.into());
    }

    pub(crate) fn start_goal_from_proposed_plan(
        &mut self,
        plan_text: String,
        identity: IdentityMask,
    ) {
        self.set_identity_mask(identity);
        self.sync_active_identity_to_runtime();
        self.submit_op(Op::ThreadGoalStartFromProposedPlan { plan_text });
    }

    pub(crate) fn clear_goal_and_start_from_proposed_plan(
        &mut self,
        plan_text: String,
        expected_goal_id: String,
        identity: IdentityMask,
    ) {
        self.set_identity_mask(identity);
        self.submit_op(Op::ThreadGoalClearAndStartFromProposedPlan {
            plan_text,
            expected_goal_id,
            identity: self.effective_identity(),
        });
    }

    /// True when the UI is in the regular composer state with no running task,
    /// no modal overlay (e.g. approvals or status indicator), and no composer popups.
    /// In this state Esc-Esc backtracking is enabled.
    pub(crate) fn is_normal_backtrack_mode(&self) -> bool {
        self.bottom_pane.is_normal_backtrack_mode()
    }

    pub(crate) fn insert_str(&mut self, text: &str) {
        self.bottom_pane.insert_str(text);
    }

    /// Replace the composer content with the provided text and reset cursor.
    pub(crate) fn set_composer_text(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.bottom_pane
            .set_composer_text(text, text_elements, local_image_paths);
    }

    pub(crate) fn show_esc_backtrack_hint(&mut self) {
        self.bottom_pane.show_esc_backtrack_hint();
    }

    pub(crate) fn clear_esc_backtrack_hint(&mut self) {
        self.bottom_pane.clear_esc_backtrack_hint();
    }

    pub(crate) fn agent_shutdown_complete(&self) -> bool {
        self.agent_shutdown_complete
    }

    /// Forward an `Op` directly to codex.
    pub(crate) fn submit_op(&mut self, op: Op) {
        if self.agent_shutdown_complete {
            tracing::debug!("ignoring op submitted after agent shutdown completed");
            return;
        }

        // Record outbound operation for session replay fidelity.
        crate::product::tui_app::session_log::log_outbound_op(&op);
        if let Err(e) = self.codex_op_tx.send(op) {
            tracing::error!("failed to submit op: {e}");
        }
    }

    fn on_list_mcp_tools(&mut self, ev: McpListToolsResponseEvent) {
        self.add_to_history(history_cell::new_mcp_tools_output(
            &self.config,
            ev.tools,
            ev.resources,
            ev.resource_templates,
            &ev.auth_statuses,
        ));
    }

    fn on_list_custom_prompts(&mut self, ev: ListCustomPromptsResponseEvent) {
        let len = ev.custom_prompts.len();
        debug!("received {len} custom prompts");
        // Forward to bottom pane so the slash popup can show them now.
        self.bottom_pane.set_custom_prompts(ev.custom_prompts);
    }

    fn on_list_skills(&mut self, ev: ListSkillsResponseEvent) {
        self.set_skills_from_response(&ev);
    }

    pub(crate) fn on_connectors_loaded(&mut self, result: Result<ConnectorsSnapshot, String>) {
        self.connectors_cache = match result {
            Ok(connectors) => ConnectorsCacheState::Ready(connectors),
            Err(err) => ConnectorsCacheState::Failed(err),
        };
        if let ConnectorsCacheState::Ready(snapshot) = &self.connectors_cache {
            self.bottom_pane
                .set_connectors_snapshot(Some(snapshot.clone()));
        } else {
            self.bottom_pane.set_connectors_snapshot(None);
        }
    }

    pub(crate) fn token_usage(&self) -> TokenUsage {
        self.token_info
            .as_ref()
            .map(|ti| ti.total_token_usage.clone())
            .unwrap_or_default()
    }

    pub(crate) fn input_slimming_exit_summary(&self) -> Option<InputSlimmingExitSummary> {
        let input_slimming = self.input_slimming.as_ref()?;
        (input_slimming.total_saved_tokens > 0).then_some(InputSlimmingExitSummary {
            tokens_saved: input_slimming.total_saved_tokens,
            saved_usd_micros: input_slimming.total_saved_usd_micros,
        })
    }

    pub(crate) fn thread_id(&self) -> Option<ThreadId> {
        self.thread_id
    }

    pub(crate) fn thread_name(&self) -> Option<String> {
        self.thread_name.clone()
    }
    pub(crate) fn rollout_path(&self) -> Option<PathBuf> {
        self.current_rollout_path.clone()
    }

    /// Returns a cache key describing the current in-flight active cell for the transcript overlay.
    ///
    /// `Ctrl+T` renders committed transcript cells plus a render-only live tail derived from the
    /// current active cell, and the overlay caches that tail; this key is what it uses to decide
    /// whether it must recompute. When there is no active cell, this returns `None` so the overlay
    /// can drop the tail entirely.
    ///
    /// If callers mutate the active cell's transcript output without bumping the revision (or
    /// providing an appropriate animation tick), the overlay will keep showing a stale tail while
    /// the main viewport updates.
    pub(crate) fn active_cell_transcript_key(&self) -> Option<ActiveCellTranscriptKey> {
        let cell = self.active_cell.as_ref()?;
        Some(ActiveCellTranscriptKey {
            revision: self.active_cell_revision,
            is_stream_continuation: cell.is_stream_continuation(),
            animation_tick: cell.transcript_animation_tick(),
        })
    }

    pub(crate) fn transcript_live_tail_key(&self) -> Option<TranscriptLiveTailKey> {
        let active_key = self.active_cell_transcript_key();
        let has_plan_stream = self.plan_stream_has_visible_tail();
        let plan_revision = self.plan_stream_live_tail_revision();

        match (active_key, has_plan_stream) {
            (Some(key), true) => Some(TranscriptLiveTailKey::new(
                TranscriptLiveTailSource::Composite,
                0,
                key.revision.wrapping_mul(31).wrapping_add(plan_revision),
                key.is_stream_continuation,
                key.animation_tick,
            )),
            (Some(key), false) => Some(TranscriptLiveTailKey::new(
                TranscriptLiveTailSource::ActiveCell,
                0,
                key.revision,
                key.is_stream_continuation,
                key.animation_tick,
            )),
            (None, true) => Some(TranscriptLiveTailKey::new(
                TranscriptLiveTailSource::PlanStream,
                0,
                plan_revision,
                false,
                None,
            )),
            (None, false) => None,
        }
    }

    pub(crate) fn transcript_live_tail_for_mode(
        &self,
        width: u16,
        mode: TranscriptRenderMode,
    ) -> Option<TranscriptLiveTail> {
        let mut lines = Vec::new();
        let mut fill_line_backgrounds = false;
        if let Some(cell) = self.active_cell.as_ref() {
            fill_line_backgrounds |= cell.fill_line_backgrounds();
            lines.extend(match mode {
                TranscriptRenderMode::Display => cell.display_lines(width),
                TranscriptRenderMode::Transcript => cell.transcript_lines(width),
            });
        }

        let plan_lines = self.plan_stream_lines_for_mode(width, mode);
        if !plan_lines.is_empty() {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(plan_lines);
            fill_line_backgrounds = true;
        }

        if lines.is_empty() {
            return None;
        }

        if fill_line_backgrounds {
            TranscriptView::live_tail_from_lines_with_backgrounds(lines, true)
        } else {
            TranscriptView::live_tail_from_lines(lines)
        }
    }

    /// Return a reference to the widget's current config (includes any
    /// runtime overrides applied via TUI, e.g., model or approval policy).
    pub(crate) fn config_ref(&self) -> &Config {
        &self.config
    }

    pub(crate) fn clear_token_usage(&mut self) {
        self.set_token_info(None);
    }

    pub(crate) fn insert_transcript_cell(&mut self, cell: Arc<dyn HistoryCell>) {
        self.transcript.borrow_mut().insert_cell(cell);
        self.request_redraw_with_risky_row_repair();
    }

    pub(crate) fn replace_transcript_cells(&mut self, cells: Vec<Arc<dyn HistoryCell>>) {
        self.transcript = RefCell::new(TranscriptView::new(cells, TranscriptRenderMode::Display));
        self.request_redraw_with_risky_row_repair();
    }

    #[cfg(test)]
    pub(crate) fn transcript_scroll_offset(&self) -> usize {
        self.transcript.borrow().scroll_offset()
    }

    pub(crate) fn scroll_transcript_to_bottom(&mut self) {
        if self.transcript.borrow_mut().scroll_to_bottom() {
            self.request_redraw();
        }
    }

    fn sync_transcript_live_tail_for_width(&self, width: u16) {
        self.transcript.borrow_mut().sync_live_tail(
            width.max(1),
            self.transcript_live_tail_key(),
            |tail_width| {
                self.transcript_live_tail_for_mode(tail_width, TranscriptRenderMode::Display)
            },
        );
    }

    pub(crate) fn prepare_transcript_terminal_repaint(&self, area_width: u16) -> bool {
        let sidebar_width = crate::product::tui_app::sidebar::sidebar_width(area_width);
        let main_width = sidebar_width.map_or(area_width, |sidebar_width| {
            area_width.saturating_sub(sidebar_width)
        });
        let mut transcript = self.transcript.borrow_mut();
        let main_width = main_width.max(1);
        transcript.prepare_terminal_repaint_for_width(main_width);
        transcript.sync_live_tail(main_width, self.transcript_live_tail_key(), |tail_width| {
            self.transcript_live_tail_for_mode(tail_width, TranscriptRenderMode::Display)
        });
        let needs_repaint = transcript.take_terminal_repaint_request();
        if transcript.has_pending_terminal_repaint() {
            self.frame_requester.schedule_frame();
        }
        needs_repaint
    }

    fn handle_transcript_scroll_key(&mut self, key_event: KeyEvent) -> bool {
        if !self.bottom_pane.no_modal_or_popup_active()
            && !self.bottom_pane.allow_background_transcript_interaction()
        {
            return false;
        }

        let command = match key_event {
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => Some(TranscriptScroll::PageUp),
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => Some(TranscriptScroll::PageDown),
            KeyEvent {
                code: KeyCode::Home,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => Some(TranscriptScroll::Home),
            KeyEvent {
                code: KeyCode::End,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => Some(TranscriptScroll::End),
            _ => None,
        };

        let Some(command) = command else {
            return false;
        };

        self.transcript.borrow_mut().apply_scroll(command);
        self.request_redraw();
        true
    }

    fn sidebar_snapshot(&self) -> SidebarSnapshot {
        let task = self
            .latest_proposed_plan_title
            .as_ref()
            .map(|title| TaskPanelSnapshot {
                title: title.clone(),
            });
        let todo = self.latest_update_plan.as_ref().and_then(|plan| {
            (!plan.plan.is_empty()).then(|| TodoPanelSnapshot {
                items: plan
                    .plan
                    .iter()
                    .map(|item| TodoPanelItem {
                        step: item.step.clone(),
                        status: item.status.clone(),
                    })
                    .collect(),
            })
        });

        let mcp = self.mcp_startup_status.as_ref().map(|statuses| {
            let mut snapshot = McpPanelSnapshot {
                starting: 0,
                ready: 0,
                failed: Vec::new(),
                cancelled: 0,
            };
            for (server, status) in statuses {
                match status {
                    McpStartupStatus::Starting => snapshot.starting += 1,
                    McpStartupStatus::Ready => snapshot.ready += 1,
                    McpStartupStatus::Failed { .. } => snapshot.failed.push(server.clone()),
                    McpStartupStatus::Cancelled => snapshot.cancelled += 1,
                }
            }
            snapshot.failed.sort();
            snapshot
        });

        let status = Some(StatusPanelSnapshot {
            model: self.current_model().to_string(),
            provider: format_model_provider_name(&self.config),
            identity: format!("{:?}", self.active_identity_kind()),
            left_context_tokens: self.token_info.as_ref().and_then(|info| {
                info.model_context_window.map(|window| {
                    window.saturating_sub(info.last_token_usage.tokens_in_context_window().max(0))
                })
            }),
            total_usage_tokens: self
                .token_info
                .as_ref()
                .map(|info| info.total_token_usage.blended_total())
                .unwrap_or_default(),
            cache_hit_percent: self
                .token_info
                .as_ref()
                .and_then(|info| cache_hit_percent(&info.total_token_usage)),
            input_slimming: self.input_slimming.clone(),
            context_compact_count: self.context_compact_count,
        });

        SidebarSnapshot {
            task,
            todo,
            files: self
                .changed_files
                .iter()
                .take(SIDEBAR_VISIBLE_FILES_LIMIT)
                .cloned()
                .collect(),
            files_more_count: self
                .changed_files
                .len()
                .saturating_sub(SIDEBAR_VISIBLE_FILES_LIMIT),
            agents: self.sidebar_agent_entries(),
            skills: self
                .loaded_skills
                .iter()
                .map(|skill| SkillPanelEntry {
                    name: skill.name.clone(),
                    path: skill.path.clone(),
                })
                .collect(),
            mcp,
            status,
        }
    }

    fn sidebar_agent_entries(&self) -> Vec<AgentPanelEntry> {
        let mut entries = self.cli_agent_jobs.iter().collect::<Vec<_>>();
        entries.sort_by(|(_, left), (_, right)| {
            match (
                matches!(left.status, AgentJobDisplayStatus::Running),
                matches!(right.status, AgentJobDisplayStatus::Running),
            ) {
                (true, true) => left.display_order.cmp(&right.display_order),
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                (false, false) => right.display_order.cmp(&left.display_order),
            }
        });
        entries
            .into_iter()
            .map(|(job_id, entry)| AgentPanelEntry {
                job_id: job_id.clone(),
                label: agent_job_sidebar_label(entry),
                status: entry.status,
            })
            .collect()
    }
}

fn agent_job_kind_label(agent_type: AgentJobKind) -> &'static str {
    match agent_type {
        AgentJobKind::Explorer => "Explorer",
        AgentJobKind::Reviewer => "Reviewer",
    }
}

fn agent_job_kind_role_label(agent_type: AgentJobKind) -> &'static str {
    match agent_type {
        AgentJobKind::Explorer => "explorer",
        AgentJobKind::Reviewer => "reviewer",
    }
}

fn normalize_agent_job_name(name: Option<String>) -> Option<String> {
    name.map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn agent_job_sidebar_label(entry: &CliAgentJobEntry) -> String {
    match entry.name.as_deref() {
        Some(name) => {
            let role = agent_job_kind_role_label(entry.agent_type);
            format!("{name} [{role}]")
        }
        None => {
            let kind = agent_job_kind_label(entry.agent_type);
            let display_order = entry.display_order;
            format!("{kind} #{display_order}")
        }
    }
}

fn is_copy_shortcut(key: KeyEvent) -> bool {
    let KeyEvent {
        code: KeyCode::Char(c),
        modifiers,
        kind: KeyEventKind::Press,
        ..
    } = key
    else {
        return false;
    };

    c.eq_ignore_ascii_case(&'c')
        && (modifiers.contains(KeyModifiers::SUPER)
            || (modifiers.contains(KeyModifiers::CONTROL)
                && modifiers.contains(KeyModifiers::SHIFT)))
}

impl Renderable for ChatWidget {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let sidebar_width = crate::product::tui_app::sidebar::sidebar_width(area.width);
        self.bottom_pane.set_buddy_external(sidebar_width.is_some());
        let [main_area, sidebar_area] = if let Some(sidebar_width) = sidebar_width {
            Layout::horizontal([Constraint::Min(1), Constraint::Length(sidebar_width)]).areas(area)
        } else {
            [area, Rect::ZERO]
        };

        self.sync_transcript_live_tail_for_width(main_area.width);
        let bottom_height = self.bottom_pane.desired_height(main_area.width);
        let transcript_height = self
            .transcript
            .borrow()
            .desired_height(main_area.width.max(1));
        let has_transcript = transcript_height > 0 || self.active_cell.is_some();
        let top_inset = u16::from(has_transcript);
        let separator_height = u16::from(has_transcript && bottom_height > 0);
        let top_height = main_area
            .height
            .saturating_sub(bottom_height.saturating_add(separator_height));
        let [transcript_area, _separator_area, bottom_area] = Layout::vertical([
            Constraint::Length(top_height),
            Constraint::Length(separator_height),
            Constraint::Length(bottom_height),
        ])
        .areas(main_area);
        let transcript_area = Rect::new(
            transcript_area.x,
            transcript_area.y.saturating_add(top_inset),
            transcript_area.width,
            transcript_area.height.saturating_sub(top_inset),
        );
        self.last_transcript_area.set(Some(transcript_area));
        self.last_bottom_area.set(Some(bottom_area));

        {
            let mut transcript = self.transcript.borrow_mut();
            transcript.advance_drag_autoscroll(transcript_area);
            if transcript.drag_autoscroll_active() {
                self.frame_requester
                    .schedule_frame_in(DRAG_AUTOSCROLL_INTERVAL);
            }
            transcript.render_inline(transcript_area, buf);
        }
        self.bottom_pane.render(bottom_area, buf);
        if sidebar_width.is_some() {
            let snapshot = self.sidebar_snapshot();
            SidebarWidget {
                snapshot: &snapshot,
                buddy_state: Some(self.bottom_pane.buddy_state()),
                animations_enabled: self.config.animations,
            }
            .render(sidebar_area, buf);
        }
        self.last_rendered_width.set(Some(main_area.width as usize));
    }

    fn desired_height(&self, width: u16) -> u16 {
        let sidebar_width = crate::product::tui_app::sidebar::sidebar_width(width);
        let sidebar_visible = sidebar_width.is_some();
        let main_width =
            sidebar_width.map_or(width, |sidebar_width| width.saturating_sub(sidebar_width));
        self.bottom_pane.set_buddy_external(sidebar_visible);
        self.sync_transcript_live_tail_for_width(main_width);
        let transcript_height = self.transcript.borrow().desired_height(main_width.max(1));
        let top_inset = u16::from(transcript_height > 0 || self.active_cell.is_some());
        let bottom_height = self.bottom_pane.desired_height(main_width);
        let separator_height =
            u16::from((transcript_height > 0 || self.active_cell.is_some()) && bottom_height > 0);
        let main_height = transcript_height
            .saturating_add(top_inset)
            .saturating_add(separator_height)
            .saturating_add(bottom_height);
        if sidebar_visible {
            main_height.max(
                crate::product::tui_app::sidebar::external_buddy_desired_height(Some(
                    self.bottom_pane.buddy_state(),
                )),
            )
        } else {
            main_height
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let main_width = crate::product::tui_app::sidebar::sidebar_width(area.width)
            .map_or(area.width, |width| area.width.saturating_sub(width));
        let bottom_height = self.bottom_pane.desired_height(main_width);
        let bottom_y = area.bottom().saturating_sub(bottom_height);
        self.bottom_pane
            .cursor_pos(Rect::new(area.x, bottom_y, main_width, bottom_height))
    }
}

enum Notification {
    AgentTurnComplete { response: String },
    ExecApprovalRequested { command: String },
    EditApprovalRequested { cwd: PathBuf, changes: Vec<PathBuf> },
    ElicitationRequested { server_name: String },
}

impl Notification {
    fn display(&self) -> String {
        match self {
            Notification::AgentTurnComplete { response } => {
                Notification::agent_turn_preview(response)
                    .unwrap_or_else(|| "Agent turn complete".to_string())
            }
            Notification::ExecApprovalRequested { command } => {
                format!("Approval requested: {}", truncate_text(command, 30))
            }
            Notification::EditApprovalRequested { cwd, changes } => {
                format!(
                    "LHA wants to edit {}",
                    if changes.len() == 1 {
                        #[allow(clippy::unwrap_used)]
                        display_path_for(changes.first().unwrap(), cwd)
                    } else {
                        format!("{} files", changes.len())
                    }
                )
            }
            Notification::ElicitationRequested { server_name } => {
                format!("Approval requested by {server_name}")
            }
        }
    }

    fn type_name(&self) -> &str {
        match self {
            Notification::AgentTurnComplete { .. } => "agent-turn-complete",
            Notification::ExecApprovalRequested { .. }
            | Notification::EditApprovalRequested { .. }
            | Notification::ElicitationRequested { .. } => "approval-requested",
        }
    }

    fn allowed_for(&self, settings: &Notifications) -> bool {
        match settings {
            Notifications::Enabled(enabled) => *enabled,
            Notifications::Custom(allowed) => allowed.iter().any(|a| a == self.type_name()),
        }
    }

    fn agent_turn_preview(response: &str) -> Option<String> {
        let mut normalized = String::new();
        for part in response.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push_str(part);
        }
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(truncate_text(trimmed, AGENT_NOTIFICATION_PREVIEW_GRAPHEMES))
        }
    }
}

const AGENT_NOTIFICATION_PREVIEW_GRAPHEMES: usize = 200;

const PLACEHOLDERS: [&str; 8] = [
    "Explain this codebase",
    "Summarize recent commits",
    "Implement {feature}",
    "Find and fix a bug in @filename",
    "Write tests for @filename",
    "Improve documentation in @filename",
    "Run /review on my current changes",
    "Use /skills to manage skills",
];

#[derive(Debug, PartialEq, Eq)]
struct ReasoningPresentation {
    latest_status_title: Option<String>,
    transcript_markdown: String,
}

fn split_reasoning_presentation(markdown: &str) -> ReasoningPresentation {
    let mut presentation = split_reasoning_presentation_untrimmed(markdown);
    presentation.transcript_markdown = presentation.transcript_markdown.trim().to_string();
    presentation
}

fn split_reasoning_presentation_untrimmed(markdown: &str) -> ReasoningPresentation {
    let mut latest_status_title = None;
    let mut transcript_lines: Vec<String> = Vec::new();

    for raw_line in markdown.lines() {
        let (had_leading_comment, without_leading_comment) = strip_leading_html_comments(raw_line);
        let (had_trailing_comment, without_comments) =
            strip_trailing_html_comments(without_leading_comment);
        let normalized = without_comments.trim();

        if let Some(title) = standalone_reasoning_title(normalized) {
            latest_status_title = Some(title);
            continue;
        }

        if normalized.is_empty() && (had_leading_comment || had_trailing_comment) {
            continue;
        }

        if had_leading_comment || had_trailing_comment {
            transcript_lines.push(normalized.to_string());
        } else {
            transcript_lines.push(raw_line.to_string());
        }
    }

    ReasoningPresentation {
        latest_status_title,
        transcript_markdown: transcript_lines.join("\n"),
    }
}

fn reasoning_buffer_presentation(entries: &[ReasoningBufferEntry]) -> ReasoningPresentation {
    let mut latest_status_title = None;
    let mut transcript_markdown = String::new();

    for entry in entries {
        match entry {
            ReasoningBufferEntry::Content {
                source: ReasoningContentSource::Summary,
                markdown,
                ..
            } => {
                let presentation = split_reasoning_presentation_untrimmed(markdown);
                if let Some(title) = presentation.latest_status_title {
                    latest_status_title = Some(title);
                }
                transcript_markdown.push_str(&presentation.transcript_markdown);
            }
            ReasoningBufferEntry::Content {
                source: ReasoningContentSource::Raw,
                markdown,
                ..
            } => transcript_markdown.push_str(markdown),
            ReasoningBufferEntry::SectionBreak { .. } => transcript_markdown.push_str("\n\n"),
        }
    }

    ReasoningPresentation {
        latest_status_title,
        transcript_markdown: transcript_markdown.trim().to_string(),
    }
}

fn reasoning_buffer_markdown(entries: &[ReasoningBufferEntry]) -> String {
    let mut markdown = String::new();
    for entry in entries {
        match entry {
            ReasoningBufferEntry::Content {
                markdown: entry_markdown,
                ..
            } => markdown.push_str(entry_markdown),
            ReasoningBufferEntry::SectionBreak { .. } => markdown.push_str("\n\n"),
        }
    }
    markdown
}

fn canonical_reasoning_item_entries(
    item: &ReasoningItem,
    state: &ReasoningItemStreamState,
    show_raw_agent_reasoning: bool,
) -> Vec<ReasoningBufferEntry> {
    let mut entries = Vec::new();
    append_reconciled_reasoning_entries(
        &mut entries,
        &item.id,
        ReasoningContentSource::Summary,
        &item.summary_text,
        &state.summary_deltas,
    );
    if show_raw_agent_reasoning {
        append_reconciled_reasoning_entries(
            &mut entries,
            &item.id,
            ReasoningContentSource::Raw,
            &item.raw_content,
            &state.raw_deltas,
        );
    }
    entries
}

fn append_reconciled_reasoning_entries(
    entries: &mut Vec<ReasoningBufferEntry>,
    item_id: &str,
    source: ReasoningContentSource,
    completed_parts: &[String],
    received_deltas: &BTreeMap<i64, String>,
) {
    for markdown in reconciled_reasoning_parts(completed_parts, received_deltas) {
        if !entries.is_empty() {
            entries.push(ReasoningBufferEntry::SectionBreak {
                item_id: Some(item_id.to_string()),
            });
        }
        entries.push(ReasoningBufferEntry::Content {
            item_id: Some(item_id.to_string()),
            source,
            markdown,
        });
    }
}

fn reconciled_reasoning_parts(
    completed_parts: &[String],
    received_deltas: &BTreeMap<i64, String>,
) -> Vec<String> {
    let received_len = received_deltas
        .last_key_value()
        .and_then(|(index, _)| usize::try_from(*index).ok())
        .and_then(|index| index.checked_add(1))
        .unwrap_or_default();
    let parts_len = completed_parts.len().max(received_len);

    (0..parts_len)
        .filter_map(|index| {
            let received = i64::try_from(index)
                .ok()
                .and_then(|index| received_deltas.get(&index))
                .map(String::as_str)
                .unwrap_or_default();
            let completed = completed_parts
                .get(index)
                .map(String::as_str)
                .unwrap_or_default();
            let mut markdown = received.to_string();
            markdown.push_str(missing_reasoning_suffix(received, completed));
            (!markdown.is_empty()).then_some(markdown)
        })
        .collect()
}

fn standalone_reasoning_title(line: &str) -> Option<String> {
    let title = line
        .strip_prefix("**")
        .and_then(|rest| rest.strip_suffix("**"))?
        .trim();
    (!title.is_empty() && !title.contains("**")).then(|| title.to_string())
}

fn missing_reasoning_suffix<'a>(received: &str, completed: &'a str) -> &'a str {
    if completed.is_empty() || received.ends_with(completed) {
        return "";
    }

    let mut overlap_len = 0;
    for (index, _) in completed.char_indices().skip(1) {
        if received.ends_with(&completed[..index]) {
            overlap_len = index;
        }
    }
    &completed[overlap_len..]
}

fn strip_leading_html_comments(mut line: &str) -> (bool, &str) {
    let mut stripped = false;
    loop {
        line = line.trim_start();
        if line.is_empty() {
            return (stripped, "");
        }
        if !line.starts_with("<!--") {
            return (stripped, line.trim());
        }
        let Some(end) = line.find("-->").map(|index| index + "-->".len()) else {
            return (stripped, line.trim());
        };
        stripped = true;
        line = &line[end..];
    }
}

fn strip_trailing_html_comments(mut line: &str) -> (bool, &str) {
    let mut stripped = false;
    loop {
        line = line.trim_end();
        if line.is_empty() {
            return (stripped, "");
        }
        if !line.ends_with("-->") {
            return (stripped, line.trim());
        }
        let Some(start) = line.rfind("<!--") else {
            return (stripped, line.trim());
        };
        if !line[start..].trim_end().ends_with("-->") {
            return (stripped, line.trim());
        }
        stripped = true;
        line = &line[..start];
    }
}

fn format_duration_short(seconds: u64) -> String {
    if seconds < 60 {
        "less than a minute".to_string()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

#[cfg(test)]
pub(crate) mod tests;
