//! Exercises `ChatWidget` event handling and rendering invariants.
//!
//! These tests treat the widget as the adapter between `adam_agent::protocol::EventMsg` inputs and
//! the TUI output. Many assertions are snapshot-based so that layout regressions and status/header
//! changes show up as stable, reviewable diffs.

use super::*;
use crate::app_event::AppEvent;
use crate::app_event::ExitMode;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::LocalImageAttachment;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::style::proposed_plan_style;
use crate::test_backend::VT100Backend;
use crate::transcript_selection::TranscriptSelectionPoint;
use crate::transcript_view::TranscriptRenderMode;
use crate::transcript_view::TranscriptView;
use crate::tui::FrameRequester;
use adam_agent::AuthManager;
use adam_agent::CodexAuth;
use adam_agent::auth::AuthCredentialsStoreMode;
use adam_agent::config::Config;
use adam_agent::config::ConfigBuilder;
use adam_agent::config::Constrained;
use adam_agent::config::ConstraintError;
use adam_agent::config::model_ref::ModelRef;
use adam_agent::config::state_json::AdamStateStore;
use adam_agent::config::types::BuddyObserverConfig;
use adam_agent::config::types::TuiBuddy;
use adam_agent::config_loader::RequirementSource;
use adam_agent::features::Feature;
use adam_agent::models_manager::manager::ModelsManager;
use adam_agent::protocol::AgentMessageDeltaEvent;
use adam_agent::protocol::AgentMessageEvent;
use adam_agent::protocol::AgentReasoningDeltaEvent;
use adam_agent::protocol::AgentReasoningEvent;
use adam_agent::protocol::ApplyPatchApprovalRequestEvent;
use adam_agent::protocol::BackgroundEventEvent;
use adam_agent::protocol::Event;
use adam_agent::protocol::EventMsg;
use adam_agent::protocol::ExecApprovalRequestEvent;
use adam_agent::protocol::ExecCommandBeginEvent;
use adam_agent::protocol::ExecCommandEndEvent;
use adam_agent::protocol::ExecCommandOutputDeltaEvent;
use adam_agent::protocol::ExecCommandSource;
use adam_agent::protocol::ExecOutputStream;
use adam_agent::protocol::ExecPolicyAmendment;
use adam_agent::protocol::ExitedReviewModeEvent;
use adam_agent::protocol::FileChange;
use adam_agent::protocol::ItemCompletedEvent;
use adam_agent::protocol::McpStartupCompleteEvent;
use adam_agent::protocol::McpStartupStatus;
use adam_agent::protocol::McpStartupUpdateEvent;
use adam_agent::protocol::Op;
use adam_agent::protocol::PatchApplyBeginEvent;
use adam_agent::protocol::PatchApplyEndEvent;
use adam_agent::protocol::ReviewRequest;
use adam_agent::protocol::ReviewTarget;
use adam_agent::protocol::SessionSource;
use adam_agent::protocol::StreamErrorEvent;
use adam_agent::protocol::TerminalInteractionEvent;
use adam_agent::protocol::TokenCountEvent;
use adam_agent::protocol::TokenUsage;
use adam_agent::protocol::TokenUsageInfo;
use adam_agent::protocol::TurnCompleteEvent;
use adam_agent::protocol::TurnStartedEvent;
use adam_agent::protocol::UndoCompletedEvent;
use adam_agent::protocol::UndoStartedEvent;
use adam_agent::protocol::ViewImageToolCallEvent;
use adam_agent::protocol::WarningEvent;
use adam_common::approval_presets::builtin_approval_presets;
use adam_otel::OtelManager;
use adam_protocol::ThreadId;
use adam_protocol::config_types::Identity;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::config_types::Personality;
use adam_protocol::config_types::Settings;
use adam_protocol::items::ContextCompactionItem;
use adam_protocol::items::TurnItem;
use adam_protocol::openai_models::ModelPreset;
use adam_protocol::openai_models::ReasoningEffortPreset;
use adam_protocol::parse_command::ParsedCommand;
use adam_protocol::plan_tool::PlanItemArg;
use adam_protocol::plan_tool::StepStatus;
use adam_protocol::plan_tool::UpdatePlanArgs;
use adam_protocol::protocol::CodexErrorInfo;
use adam_protocol::user_input::TextElement;
use adam_protocol::user_input::UserInput;
use adam_utils_absolute_path::AbsolutePathBuf;
use adam_utils_sleep_inhibitor::SleepInhibitor;
use assert_matches::assert_matches;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use dirs::home_dir;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
#[cfg(target_os = "windows")]
use serial_test::serial;
use std::collections::HashSet;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use tempfile::tempdir;
use tokio::sync::broadcast;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::unbounded_channel;
use toml::Value as TomlValue;

use crate::provider_config::CustomProviderConfig;
use crate::provider_config::custom_provider_ref;
use crate::provider_config::persist_custom_provider_files;

async fn test_config() -> Config {
    // Use base defaults to avoid depending on host state.
    let adam_home = tempdir().expect("tempdir");
    let adam_home_path = adam_home.path().to_path_buf();
    std::mem::forget(adam_home);
    ConfigBuilder::default()
        .adam_home(adam_home_path)
        .provider_config_required(false)
        .build()
        .await
        .expect("config")
}

fn stable_snapshot_cwd() -> PathBuf {
    let mut cwd = home_dir().expect("home directory");
    cwd.push("Workspace/adam/src/tui/app");
    cwd
}

fn apply_stable_snapshot_cwd(chat: &mut ChatWidget) {
    chat.config.cwd = stable_snapshot_cwd();
    chat.update_footer_info();
}

fn invalid_value(candidate: impl Into<String>, allowed: impl Into<String>) -> ConstraintError {
    ConstraintError::InvalidValue {
        field_name: "<unknown>",
        candidate: candidate.into(),
        allowed: allowed.into(),
        requirement_source: RequirementSource::Unknown,
    }
}

#[tokio::test]
async fn resumed_initial_messages_render_history() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().unwrap();
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: Some(vec![
            EventMsg::UserMessage(UserMessageEvent {
                message: "hello from user".to_string(),
                images: None,
                text_elements: Vec::new(),
                local_images: Vec::new(),
            }),
            EventMsg::AgentMessage(AgentMessageEvent {
                message: "assistant reply".to_string(),
            }),
        ]),
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });

    let cells = drain_insert_history(&mut rx);
    let mut merged_lines = Vec::new();
    for lines in cells {
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.clone())
            .collect::<String>();
        merged_lines.push(text);
    }

    let text_blob = merged_lines.join("\n");
    assert!(
        text_blob.contains("hello from user"),
        "expected replayed user message",
    );
    assert!(
        text_blob.contains("assistant reply"),
        "expected replayed agent message",
    );
}

#[tokio::test]
async fn session_configured_updates_footer_reasoning_effort_immediately() {
    let adam_home = tempdir().expect("tempdir");
    let model_ref = ModelRef::new("openai", "main", "gpt-5");
    AdamStateStore::new(adam_home.path())
        .set_last_selected_model(&model_ref, Some(ReasoningEffortConfig::High), None)
        .expect("persist model effort");
    let cfg = ConfigBuilder::default()
        .adam_home(adam_home.path().to_path_buf())
        .provider_config_required(false)
        .cli_overrides(vec![(
            "features.identities".to_string(),
            TomlValue::Boolean(true),
        )])
        .build()
        .await
        .expect("config");
    let resolved_model = ModelsManager::get_model_offline(cfg.model.as_deref());
    let otel_manager = test_otel_manager(&cfg, resolved_model.as_str());
    let thread_manager = Arc::new(ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        cfg.model_provider.clone(),
    ));
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let init = ChatWidgetInit {
        config: cfg,
        thread_manager,
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(unbounded_channel::<AppEvent>().0),
        initial_user_message: None,
        enhanced_keys_supported: false,
        auth_manager,
        feedback: adam_feedback::CodexFeedback::new(),
        is_first_run: true,
        startup: ChatWidgetStartup::Configured {
            model: Some(resolved_model.clone()),
        },
        otel_manager,
    };

    let mut chat = ChatWidget::new(init);
    let width = 120;
    let height = chat.desired_height(width);
    let mut terminal =
        ratatui::Terminal::new(VT100Backend::new(width, height)).expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));

    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("render initial state");
    let initial_footer = terminal
        .backend()
        .vt100()
        .screen()
        .contents()
        .lines()
        .last()
        .unwrap_or_default()
        .to_string();
    assert!(
        initial_footer.contains("Identity nobody"),
        "expected initial identity footer: {initial_footer:?}"
    );
    assert!(
        initial_footer.contains("High"),
        "expected startup footer to restore reasoning effort from state: {initial_footer:?}"
    );

    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: ThreadId::new(),
        forked_from_id: None,
        thread_name: None,
        model: resolved_model,
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::Medium),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "session-configured".into(),
        msg: EventMsg::SessionConfigured(configured),
    });

    let height = chat.desired_height(width);
    let mut terminal =
        ratatui::Terminal::new(VT100Backend::new(width, height)).expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("render configured state");

    let footer = terminal
        .backend()
        .vt100()
        .screen()
        .contents()
        .lines()
        .last()
        .unwrap_or_default()
        .to_string();
    let footer_lower = footer.to_ascii_lowercase();
    assert!(
        footer.contains("Identity nobody"),
        "expected identity footer: {footer:?}"
    );
    assert!(
        footer_lower.contains(" medium "),
        "expected footer to show reasoning effort immediately after session configuration: {footer:?}"
    );
}

#[tokio::test]
async fn desired_height_includes_live_tail_before_render() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.active_cell = Some(Box::new(PlainHistoryCell::new(vec![
        "this live tail should wrap across several lines before render".into(),
    ])));
    chat.transcript = RefCell::new(TranscriptView::new(
        Vec::new(),
        TranscriptRenderMode::Display,
    ));

    let width = 12;
    let bottom_height = chat.bottom_pane.desired_height(width);
    let height = chat.desired_height(width);

    assert!(height > bottom_height + 2);
}

#[derive(Debug)]
struct WidthSensitiveTranscriptCell;

impl HistoryCell for WidthSensitiveTranscriptCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width < 20 {
            vec![
                "narrow one".into(),
                "narrow two".into(),
                "narrow three".into(),
            ]
        } else {
            vec!["wide".into()]
        }
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width < 20 {
            vec!["transcript narrow".into()]
        } else {
            vec!["transcript wide".into()]
        }
    }
}

#[derive(Debug)]
struct SplitActiveCell;

impl HistoryCell for SplitActiveCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width < 20 {
            vec![
                "narrow one".into(),
                "narrow two".into(),
                "narrow three".into(),
            ]
        } else {
            vec!["wide".into()]
        }
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec!["transcript".into()]
    }
}

#[derive(Debug)]
struct MaxHeightTranscriptCell;

impl HistoryCell for MaxHeightTranscriptCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec!["transcript".into()]
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::MAX
    }

    fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec!["transcript".into()]
    }

    fn desired_transcript_height(&self, _width: u16) -> u16 {
        u16::MAX
    }
}

#[derive(Debug)]
struct TallTranscriptCell(usize);

impl HistoryCell for TallTranscriptCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        (0..self.0)
            .map(|idx| Line::from(format!("transcript line {idx}")))
            .collect()
    }
}

#[derive(Debug)]
struct CountingHistoryCell {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl HistoryCell for CountingHistoryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vec!["counted".into()]
    }
}

#[tokio::test]
async fn desired_height_rewraps_live_tail_on_width_change_before_render() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.active_cell = Some(Box::new(WidthSensitiveTranscriptCell));
    chat.transcript = RefCell::new(TranscriptView::new(
        Vec::new(),
        TranscriptRenderMode::Display,
    ));

    let wide_height = chat.desired_height(40);
    let narrow_height = chat.desired_height(12);

    assert!(narrow_height > wide_height);
}

#[tokio::test]
async fn active_cell_live_tail_respects_render_mode() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.active_cell = Some(Box::new(SplitActiveCell));

    assert_eq!(
        live_tail_text(chat.transcript_live_tail_for_mode(80, TranscriptRenderMode::Display)),
        "wide\n"
    );
    assert_eq!(
        live_tail_text(chat.transcript_live_tail_for_mode(80, TranscriptRenderMode::Transcript)),
        "transcript\n"
    );
}

#[tokio::test]
async fn assistant_delta_without_newline_stays_hidden_until_finalize() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_agent_message_delta("partial answer".to_string());

    assert!(
        chat.transcript_live_tail_for_mode(80, TranscriptRenderMode::Display)
            .is_none()
    );

    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    chat.render(buf.area, &mut buf);
    let rendered = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("partial answer"));
    assert!(drain_insert_history(&mut rx).is_empty());

    chat.flush_answer_stream_with_separator();
    let cells = drain_insert_history(&mut rx);
    let rendered = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(rendered.contains("partial answer"));
}

#[tokio::test]
async fn assistant_stream_exposes_only_completed_lines_before_finalize() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.on_agent_message_delta("one\ntwo\npartial".to_string());
    assert!(
        chat.transcript_live_tail_for_mode(80, TranscriptRenderMode::Display)
            .is_none()
    );
    chat.on_commit_tick();
    let before_finalize = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(before_finalize.contains("one"));
    assert!(before_finalize.contains("two"));
    assert!(!before_finalize.contains("partial"));

    assert!(
        chat.transcript_live_tail_for_mode(80, TranscriptRenderMode::Display)
            .is_none()
    );

    chat.flush_answer_stream_with_separator();
    let after_finalize = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(after_finalize.contains("partial"));
}

#[tokio::test]
async fn active_exec_transcript_key_changes_for_spinner_tick() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    let event = begin_exec_with_source(&mut chat, "call-id", "sleep 1", ExecCommandSource::Agent);
    chat.handle_exec_begin_now(event);

    let first = chat
        .active_cell_transcript_key()
        .expect("expected active exec key");
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    let second = chat
        .active_cell_transcript_key()
        .expect("expected active exec key");

    assert_ne!(first.animation_tick, second.animation_tick);
}

#[tokio::test]
async fn desired_height_saturates_after_adding_bottom_pane_rows() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.transcript = RefCell::new(TranscriptView::new(
        vec![Arc::new(MaxHeightTranscriptCell)],
        TranscriptRenderMode::Display,
    ));

    let width = 80;
    let bottom_height = chat.bottom_pane.desired_height(width);

    assert!(bottom_height > 0, "expected bottom pane to contribute rows");
    assert_eq!(chat.desired_height(width), u16::MAX);
}

#[tokio::test]
async fn desired_height_with_sidebar_reserves_external_buddy() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.set_buddy_config(TuiBuddy {
        enabled: true,
        muted: false,
        ..TuiBuddy::default()
    });

    let width = crate::sidebar::SIDEBAR_MIN_TERMINAL_WIDTH;
    let height = chat.desired_height(width);
    let buddy_height =
        crate::sidebar::external_buddy_desired_height(Some(chat.bottom_pane.buddy_state()));

    assert!(buddy_height > 0, "expected visible external buddy");
    assert!(height >= buddy_height);
}

#[tokio::test]
async fn render_with_sidebar_records_main_width() {
    let (chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    let width = crate::sidebar::SIDEBAR_MIN_TERMINAL_WIDTH;
    let sidebar_width = crate::sidebar::sidebar_width(width).expect("sidebar visible");
    let expected_main_width = width.saturating_sub(sidebar_width);
    let height = chat.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);

    chat.render(area, &mut buf);

    assert_eq!(
        chat.last_rendered_width.get(),
        Some(usize::from(expected_main_width))
    );
}

#[tokio::test]
async fn history_insertions_are_thread_scoped_after_session_configuration() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let thread_id = ThreadId::new();
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: thread_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(PathBuf::new()),
    };

    chat.handle_codex_event(Event {
        id: "session-configured".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    let _ = drain_insert_history(&mut rx);

    chat.add_boxed_history(Box::new(PlainHistoryCell::new(vec!["scoped".into()])));

    let event = rx.try_recv().expect("history event");
    match event {
        AppEvent::InsertThreadHistoryCell {
            thread_id: actual,
            cell,
        } => {
            assert_eq!(actual, thread_id);
            assert_eq!(lines_to_single_string(&cell.display_lines(80)), "scoped\n");
        }
        other => panic!("expected thread-scoped history event, got {other:?}"),
    }
}

#[tokio::test]
async fn session_configured_suppression_updates_footer_without_requesting_redraw() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let frame_requester = FrameRequester::new(draw_tx);
    let (mut chat, _rx, _ops) =
        make_chatwidget_manual_with_frame_requester(Some("gpt-5.2-codex"), frame_requester).await;
    chat.suppress_session_configured_redraw = true;
    drain_draw_requests(&mut draw_rx).await;

    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: ThreadId::new(),
        forked_from_id: None,
        thread_name: None,
        model: "gpt-5.2-codex".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::High),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: None,
    };

    chat.handle_codex_event(Event {
        id: "session-configured-suppressed".into(),
        msg: EventMsg::SessionConfigured(configured),
    });

    let redraw = tokio::time::timeout(std::time::Duration::from_millis(50), draw_rx.recv()).await;
    assert!(
        redraw.is_err(),
        "suppressed session configured should not request redraw"
    );

    let width = 120;
    let height = chat.desired_height(width);
    let mut terminal =
        ratatui::Terminal::new(VT100Backend::new(width, height)).expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("render configured state");

    let footer = terminal
        .backend()
        .vt100()
        .screen()
        .contents()
        .lines()
        .last()
        .unwrap_or_default()
        .to_string()
        .to_ascii_lowercase();
    assert!(
        footer.contains(" high "),
        "expected suppressed session configured to update footer state: {footer:?}"
    );
}

#[tokio::test]
async fn replayed_user_message_preserves_text_elements_and_local_images() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let placeholder = "[Image #1]";
    let message = format!("{placeholder} replayed");
    let text_elements = vec![TextElement::new(
        (0..placeholder.len()).into(),
        Some(placeholder.to_string()),
    )];
    let local_images = vec![PathBuf::from("/tmp/replay.png")];

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().unwrap();
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: Some(vec![EventMsg::UserMessage(UserMessageEvent {
            message: message.clone(),
            images: None,
            text_elements: text_elements.clone(),
            local_images: local_images.clone(),
        })]),
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });

    let mut user_cell = None;
    while let Ok(ev) = rx.try_recv() {
        if let Some(cell) = into_insert_history_cell(ev)
            && let Some(cell) = cell.as_any().downcast_ref::<UserHistoryCell>()
        {
            user_cell = Some((
                cell.message.clone(),
                cell.text_elements.clone(),
                cell.local_image_paths.clone(),
            ));
            break;
        }
    }

    let (stored_message, stored_elements, stored_images) =
        user_cell.expect("expected a replayed user history cell");
    assert_eq!(stored_message, message);
    assert_eq!(stored_elements, text_elements);
    assert_eq!(stored_images, local_images);
}

#[tokio::test]
async fn status_hides_zero_context_compact_count() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        !status.lines().any(|line| line.contains("Context compact:")),
        "expected zero context compact count to be hidden, got {status:?}"
    );
}

#[tokio::test]
async fn status_shows_live_context_compact_count() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    chat.handle_codex_event(Event {
        id: "compact-1".into(),
        msg: EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: conversation_id,
            turn_id: "turn-1".to_string(),
            item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
        }),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("1")),
        "expected context compact count in status, got {status:?}"
    );
}

#[tokio::test]
async fn status_counts_multiple_live_context_compactions_in_same_turn() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: conversation_id,
            turn_id: "turn-1".to_string(),
            item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
        }),
    });
    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: conversation_id,
            turn_id: "turn-1".to_string(),
            item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
        }),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("2")),
        "expected same-turn compact count in status, got {status:?}"
    );
}

#[tokio::test]
async fn status_resume_restores_context_compact_count_from_replay() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: Some(vec![
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: conversation_id,
                turn_id: "turn-1".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: conversation_id,
                turn_id: "turn-2".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
        ]),
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("2")),
        "expected replayed compact count in status, got {status:?}"
    );
}

#[tokio::test]
async fn status_resume_restores_multiple_same_turn_context_compactions_from_replay() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: Some(vec![
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: conversation_id,
                turn_id: "turn-1".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: conversation_id,
                turn_id: "turn-1".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
        ]),
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("2")),
        "expected replayed same-turn compact count in status, got {status:?}"
    );
}

#[tokio::test]
async fn status_fork_resume_counts_only_child_thread_compactions() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let parent_thread_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: Some(parent_thread_id),
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: Some(vec![
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: parent_thread_id,
                turn_id: "turn-parent-1".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
            EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: parent_thread_id,
                turn_id: "turn-parent-2".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
            EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: conversation_id,
                turn_id: "turn-child-1".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
            EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
        ]),
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("1")),
        "expected only child-thread compactions to be counted, got {status:?}"
    );
}

#[tokio::test]
async fn legacy_context_compacted_event_increments_count() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    chat.handle_codex_event(Event {
        id: "compact-1".into(),
        msg: EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
    });

    let cells = drain_insert_history(&mut rx);
    let transcript = lines_to_single_string(cells.last().expect("expected compacted message"));
    assert!(
        transcript.contains("Context compacted"),
        "expected transcript message, got {transcript:?}"
    );

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("1")),
        "expected legacy event to increment count, got {status:?}"
    );
}

#[tokio::test]
async fn legacy_context_compacted_event_does_not_double_count_after_structured_event() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: conversation_id,
            turn_id: "turn-1".to_string(),
            item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
        }),
    });
    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("1")),
        "expected legacy + structured events to count once, got {status:?}"
    );
}

#[tokio::test]
async fn legacy_context_compacted_event_does_not_double_count_multiple_same_turn_compactions() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    for _ in 0..2 {
        chat.handle_codex_event(Event {
            id: "turn-1".into(),
            msg: EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: conversation_id,
                turn_id: "turn-1".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
        });
        chat.handle_codex_event(Event {
            id: "turn-1".into(),
            msg: EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
        });
    }
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("2")),
        "expected same-turn legacy + structured events to count twice, got {status:?}"
    );
}

#[tokio::test]
async fn status_resume_does_not_double_count_structured_and_legacy_compactions() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: Some(vec![
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: conversation_id,
                turn_id: "turn-1".to_string(),
                item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
            }),
            EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
        ]),
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("1")),
        "expected replayed structured + legacy events to count once, got {status:?}"
    );
}

#[tokio::test]
async fn status_resume_restores_legacy_only_context_compactions() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().expect("rollout file");
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: Some(vec![
            EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
            EventMsg::ContextCompacted(adam_agent::protocol::ContextCompactedEvent {}),
        ]),
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };

    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    let status = status_output_text(&mut chat, &mut rx);
    assert!(
        status
            .lines()
            .any(|line| line.contains("Context compact:") && line.contains("2")),
        "expected replayed legacy compact count in status, got {status:?}"
    );
}

#[tokio::test]
async fn submission_preserves_text_elements_and_local_images() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;

    let conversation_id = ThreadId::new();
    let rollout_file = NamedTempFile::new().unwrap();
    let configured = adam_agent::protocol::SessionConfiguredEvent {
        session_id: conversation_id,
        forked_from_id: None,
        thread_name: None,
        model: "test-model".to_string(),
        identity_kind: IdentityKind::Nobody,
        model_provider_id: "test-provider".to_string(),
        approval_policy: AskForApproval::Never,
        sandbox_policy: SandboxPolicy::ReadOnly,
        cwd: PathBuf::from("/home/user/project"),
        reasoning_effort: Some(ReasoningEffortConfig::default()),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: Some(rollout_file.path().to_path_buf()),
    };
    chat.handle_codex_event(Event {
        id: "initial".into(),
        msg: EventMsg::SessionConfigured(configured),
    });
    drain_insert_history(&mut rx);

    let placeholder = "[Image #1]";
    let text = format!("{placeholder} submit");
    let text_elements = vec![TextElement::new(
        (0..placeholder.len()).into(),
        Some(placeholder.to_string()),
    )];
    let local_images = vec![PathBuf::from("/tmp/submitted.png")];

    chat.bottom_pane
        .set_composer_text(text.clone(), text_elements.clone(), local_images.clone());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let items = match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => items,
        other => panic!("expected Op::UserTurn, got {other:?}"),
    };
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        UserInput::LocalImage {
            path: local_images[0].clone()
        }
    );
    assert_eq!(
        items[1],
        UserInput::Text {
            text: text.clone(),
            text_elements: text_elements.clone(),
        }
    );

    let mut user_cell = None;
    while let Ok(ev) = rx.try_recv() {
        if let Some(cell) = into_insert_history_cell(ev)
            && let Some(cell) = cell.as_any().downcast_ref::<UserHistoryCell>()
        {
            user_cell = Some((
                cell.message.clone(),
                cell.text_elements.clone(),
                cell.local_image_paths.clone(),
            ));
            break;
        }
    }

    let (stored_message, stored_elements, stored_images) =
        user_cell.expect("expected submitted user history cell");
    assert_eq!(stored_message, text);
    assert_eq!(stored_elements, text_elements);
    assert_eq!(stored_images, local_images);
}

#[tokio::test]
async fn interrupted_turn_restores_queued_messages_with_images_and_elements() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    let first_placeholder = "[Image #1]";
    let first_text = format!("{first_placeholder} first");
    let first_elements = vec![TextElement::new(
        (0..first_placeholder.len()).into(),
        Some(first_placeholder.to_string()),
    )];
    let first_images = [PathBuf::from("/tmp/first.png")];

    let second_placeholder = "[Image #1]";
    let second_text = format!("{second_placeholder} second");
    let second_elements = vec![TextElement::new(
        (0..second_placeholder.len()).into(),
        Some(second_placeholder.to_string()),
    )];
    let second_images = [PathBuf::from("/tmp/second.png")];

    let existing_placeholder = "[Image #1]";
    let existing_text = format!("{existing_placeholder} existing");
    let existing_elements = vec![TextElement::new(
        (0..existing_placeholder.len()).into(),
        Some(existing_placeholder.to_string()),
    )];
    let existing_images = vec![PathBuf::from("/tmp/existing.png")];

    chat.queued_user_messages.push_back(UserMessage {
        text: first_text,
        local_images: vec![LocalImageAttachment {
            placeholder: first_placeholder.to_string(),
            path: first_images[0].clone(),
        }],
        text_elements: first_elements,
        mention_paths: HashMap::new(),
    });
    chat.queued_user_messages.push_back(UserMessage {
        text: second_text,
        local_images: vec![LocalImageAttachment {
            placeholder: second_placeholder.to_string(),
            path: second_images[0].clone(),
        }],
        text_elements: second_elements,
        mention_paths: HashMap::new(),
    });
    chat.refresh_queued_user_messages();

    chat.bottom_pane
        .set_composer_text(existing_text, existing_elements, existing_images.clone());

    // When interrupted, queued messages are merged into the composer; image placeholders
    // must be renumbered to match the combined local image list.
    chat.handle_codex_event(Event {
        id: "interrupt".into(),
        msg: EventMsg::TurnAborted(adam_agent::protocol::TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }),
    });

    let first = "[Image #1] first".to_string();
    let second = "[Image #2] second".to_string();
    let third = "[Image #3] existing".to_string();
    let expected_text = format!("{first}\n{second}\n{third}");
    assert_eq!(chat.bottom_pane.composer_text(), expected_text);

    let first_start = 0;
    let second_start = first.len() + 1;
    let third_start = second_start + second.len() + 1;
    let expected_elements = vec![
        TextElement::new(
            (first_start..first_start + "[Image #1]".len()).into(),
            Some("[Image #1]".to_string()),
        ),
        TextElement::new(
            (second_start..second_start + "[Image #2]".len()).into(),
            Some("[Image #2]".to_string()),
        ),
        TextElement::new(
            (third_start..third_start + "[Image #3]".len()).into(),
            Some("[Image #3]".to_string()),
        ),
    ];
    assert_eq!(chat.bottom_pane.composer_text_elements(), expected_elements);
    assert_eq!(
        chat.bottom_pane.composer_local_image_paths(),
        vec![
            first_images[0].clone(),
            second_images[0].clone(),
            existing_images[0].clone(),
        ]
    );
}

#[tokio::test]
async fn remap_placeholders_uses_attachment_labels() {
    let placeholder_one = "[Image #1]";
    let placeholder_two = "[Image #2]";
    let text = format!("{placeholder_two} before {placeholder_one}");
    let elements = vec![
        TextElement::new(
            (0..placeholder_two.len()).into(),
            Some(placeholder_two.to_string()),
        ),
        TextElement::new(
            ("[Image #2] before ".len().."[Image #2] before [Image #1]".len()).into(),
            Some(placeholder_one.to_string()),
        ),
    ];

    let attachments = vec![
        LocalImageAttachment {
            placeholder: placeholder_one.to_string(),
            path: PathBuf::from("/tmp/one.png"),
        },
        LocalImageAttachment {
            placeholder: placeholder_two.to_string(),
            path: PathBuf::from("/tmp/two.png"),
        },
    ];
    let message = UserMessage {
        text,
        text_elements: elements,
        local_images: attachments,
        mention_paths: HashMap::new(),
    };
    let mut next_label = 3usize;
    let remapped = remap_placeholders_for_message(message, &mut next_label);

    assert_eq!(remapped.text, "[Image #4] before [Image #3]");
    assert_eq!(
        remapped.text_elements,
        vec![
            TextElement::new(
                (0.."[Image #4]".len()).into(),
                Some("[Image #4]".to_string()),
            ),
            TextElement::new(
                ("[Image #4] before ".len().."[Image #4] before [Image #3]".len()).into(),
                Some("[Image #3]".to_string()),
            ),
        ]
    );
    assert_eq!(
        remapped.local_images,
        vec![
            LocalImageAttachment {
                placeholder: "[Image #3]".to_string(),
                path: PathBuf::from("/tmp/one.png"),
            },
            LocalImageAttachment {
                placeholder: "[Image #4]".to_string(),
                path: PathBuf::from("/tmp/two.png"),
            },
        ]
    );
}

#[tokio::test]
async fn remap_placeholders_uses_byte_ranges_when_placeholder_missing() {
    let placeholder_one = "[Image #1]";
    let placeholder_two = "[Image #2]";
    let text = format!("{placeholder_two} before {placeholder_one}");
    let elements = vec![
        TextElement::new((0..placeholder_two.len()).into(), None),
        TextElement::new(
            ("[Image #2] before ".len().."[Image #2] before [Image #1]".len()).into(),
            None,
        ),
    ];

    let attachments = vec![
        LocalImageAttachment {
            placeholder: placeholder_one.to_string(),
            path: PathBuf::from("/tmp/one.png"),
        },
        LocalImageAttachment {
            placeholder: placeholder_two.to_string(),
            path: PathBuf::from("/tmp/two.png"),
        },
    ];
    let message = UserMessage {
        text,
        text_elements: elements,
        local_images: attachments,
        mention_paths: HashMap::new(),
    };
    let mut next_label = 3usize;
    let remapped = remap_placeholders_for_message(message, &mut next_label);

    assert_eq!(remapped.text, "[Image #4] before [Image #3]");
    assert_eq!(
        remapped.text_elements,
        vec![
            TextElement::new(
                (0.."[Image #4]".len()).into(),
                Some("[Image #4]".to_string()),
            ),
            TextElement::new(
                ("[Image #4] before ".len().."[Image #4] before [Image #3]".len()).into(),
                Some("[Image #3]".to_string()),
            ),
        ]
    );
    assert_eq!(
        remapped.local_images,
        vec![
            LocalImageAttachment {
                placeholder: "[Image #3]".to_string(),
                path: PathBuf::from("/tmp/one.png"),
            },
            LocalImageAttachment {
                placeholder: "[Image #4]".to_string(),
                path: PathBuf::from("/tmp/two.png"),
            },
        ]
    );
}

/// Entering review mode uses the hint provided by the review request.
#[tokio::test]
async fn entered_review_mode_uses_request_hint() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "review-start".into(),
        msg: EventMsg::EnteredReviewMode(ReviewRequest {
            target: ReviewTarget::BaseBranch {
                branch: "feature".to_string(),
            },
            user_facing_hint: Some("feature branch".to_string()),
        }),
    });

    let cells = drain_insert_history(&mut rx);
    let banner = lines_to_single_string(cells.last().expect("review banner"));
    assert_eq!(banner, ">> Code review started: feature branch <<\n");
    assert!(chat.is_review_mode);
}

/// Entering review mode renders the current changes banner when requested.
#[tokio::test]
async fn entered_review_mode_defaults_to_current_changes_banner() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "review-start".into(),
        msg: EventMsg::EnteredReviewMode(ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    let banner = lines_to_single_string(cells.last().expect("review banner"));
    assert_eq!(banner, ">> Code review started: current changes <<\n");
    assert!(chat.is_review_mode);
}

/// Exiting review restores the pre-review context window indicator.
#[tokio::test]
async fn review_restores_context_window_indicator() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;

    let context_window = 13_000;
    let pre_review_tokens = 12_700; // ~2% remaining in the real context window.
    let review_tokens = 12_030; // ~7% remaining in the real context window.

    chat.handle_codex_event(Event {
        id: "token-before".into(),
        msg: EventMsg::TokenCount(TokenCountEvent {
            info: Some(make_token_info(pre_review_tokens, context_window)),
        }),
    });
    assert_eq!(chat.bottom_pane.context_window_percent(), Some(2));

    chat.handle_codex_event(Event {
        id: "review-start".into(),
        msg: EventMsg::EnteredReviewMode(ReviewRequest {
            target: ReviewTarget::BaseBranch {
                branch: "feature".to_string(),
            },
            user_facing_hint: Some("feature branch".to_string()),
        }),
    });

    chat.handle_codex_event(Event {
        id: "token-review".into(),
        msg: EventMsg::TokenCount(TokenCountEvent {
            info: Some(make_token_info(review_tokens, context_window)),
        }),
    });
    assert_eq!(chat.bottom_pane.context_window_percent(), Some(7));

    chat.handle_codex_event(Event {
        id: "review-end".into(),
        msg: EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
            review_output: None,
        }),
    });
    let _ = drain_insert_history(&mut rx);

    assert_eq!(chat.bottom_pane.context_window_percent(), Some(2));
    assert!(!chat.is_review_mode);
}

/// Receiving a TokenCount event without usage clears the context indicator.
#[tokio::test]
async fn token_count_none_resets_context_indicator() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(None).await;

    let context_window = 13_000;
    let pre_compact_tokens = 12_700;

    chat.handle_codex_event(Event {
        id: "token-before".into(),
        msg: EventMsg::TokenCount(TokenCountEvent {
            info: Some(make_token_info(pre_compact_tokens, context_window)),
        }),
    });
    assert_eq!(chat.bottom_pane.context_window_percent(), Some(2));

    chat.handle_codex_event(Event {
        id: "token-cleared".into(),
        msg: EventMsg::TokenCount(TokenCountEvent { info: None }),
    });
    assert_eq!(chat.bottom_pane.context_window_percent(), None);
}

#[tokio::test]
async fn context_indicator_shows_used_tokens_when_window_unknown() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(Some("unknown-model")).await;

    chat.config.model_context_window = None;
    let auto_compact_limit = 200_000;
    chat.config.model_auto_compact_token_limit = Some(auto_compact_limit);

    // No model window, so the indicator should fall back to showing tokens used.
    let total_tokens = 106_000;
    let token_usage = TokenUsage {
        total_tokens,
        ..TokenUsage::default()
    };
    let token_info = TokenUsageInfo {
        total_token_usage: token_usage.clone(),
        last_token_usage: token_usage,
        model_context_window: None,
    };

    chat.handle_codex_event(Event {
        id: "token-usage".into(),
        msg: EventMsg::TokenCount(TokenCountEvent {
            info: Some(token_info),
        }),
    });

    assert_eq!(chat.bottom_pane.context_window_percent(), None);
    assert_eq!(
        chat.bottom_pane.context_window_used_tokens(),
        Some(total_tokens)
    );
}

#[tokio::test]
async fn context_indicator_uses_real_remaining_percentage() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "token-usage".into(),
        msg: EventMsg::TokenCount(TokenCountEvent {
            info: Some(make_token_info(10_600, 30_400)),
        }),
    });

    assert_eq!(chat.bottom_pane.context_window_percent(), Some(65));
}

#[tokio::test]
async fn sidebar_status_uses_status_token_semantics() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(None).await;
    let total_usage = TokenUsage {
        input_tokens: 20_000,
        cached_input_tokens: 5_000,
        output_tokens: 2_000,
        total_tokens: 30_000,
        ..TokenUsage::default()
    };
    let last_usage = TokenUsage {
        total_tokens: 10_600,
        ..TokenUsage::default()
    };
    chat.set_token_info(Some(TokenUsageInfo {
        total_token_usage: total_usage,
        last_token_usage: last_usage,
        model_context_window: Some(30_400),
    }));

    let status = chat
        .sidebar_snapshot()
        .status
        .expect("sidebar status should be present");

    assert_eq!(
        status,
        crate::sidebar::StatusPanelSnapshot {
            model: chat.current_model().to_string(),
            identity: format!("{:?}", chat.active_identity_kind()),
            left_context_tokens: Some(19_800),
            total_usage_tokens: 17_000,
            cache_hit_percent: Some(25),
        }
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "system configuration APIs are blocked under macOS seatbelt"
)]
#[tokio::test]
async fn helpers_are_available_and_do_not_panic() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let cfg = test_config().await;
    let resolved_model = ModelsManager::get_model_offline(cfg.model.as_deref());
    let otel_manager = test_otel_manager(&cfg, resolved_model.as_str());
    let thread_manager = Arc::new(ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        cfg.model_provider.clone(),
    ));
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let init = ChatWidgetInit {
        config: cfg,
        thread_manager,
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: tx,
        initial_user_message: None,
        enhanced_keys_supported: false,
        auth_manager,
        feedback: adam_feedback::CodexFeedback::new(),
        is_first_run: true,
        startup: ChatWidgetStartup::Configured {
            model: Some(resolved_model),
        },
        otel_manager,
    };
    let mut w = ChatWidget::new(init);
    // Basic construction sanity.
    let _ = &mut w;
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

// --- Helpers for tests that need direct construction and event draining ---
async fn make_chatwidget_manual(
    model_override: Option<&str>,
) -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    make_chatwidget_manual_inner(model_override).await
}

async fn make_chatwidget_manual_inner(
    model_override: Option<&str>,
) -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (tx_raw, rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(tx_raw);
    let (op_tx, op_rx) = unbounded_channel::<Op>();
    let mut cfg = test_config().await;
    cfg.provider_config_required = false;
    let resolved_model = model_override
        .map(str::to_owned)
        .unwrap_or_else(|| ModelsManager::get_model_offline(cfg.model.as_deref()));
    if let Some(model) = model_override {
        cfg.model = Some(model.to_string());
    }
    let otel_manager = test_otel_manager(&cfg, resolved_model.as_str());
    let mut bottom = BottomPane::new(BottomPaneParams {
        app_event_tx: app_event_tx.clone(),
        frame_requester: FrameRequester::test_dummy(),
        has_input_focus: true,
        enhanced_keys_supported: false,
        placeholder_text: "Ask Adam to do anything".to_string(),
        disable_paste_burst: false,
        animations_enabled: cfg.animations,
        skills: None,
    });
    bottom.set_steer_enabled(true);
    bottom.set_identities_enabled(cfg.features.enabled(Feature::Identities));
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let adam_home = cfg.adam_home.clone();
    let thread_manager = Arc::new(ThreadManager::new(
        adam_home,
        auth_manager.clone(),
        cfg.model_provider_id.as_str(),
        cfg.model_provider.clone(),
        SessionSource::Cli,
    ));
    let reasoning_effort = None;
    let base_mode = Identity {
        kind: IdentityKind::Nobody,
        settings: Settings {
            model: resolved_model.clone(),
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let current_identity = base_mode;
    let prevent_idle_sleep = cfg.features.enabled(Feature::PreventIdleSleep);
    let mut widget = ChatWidget {
        app_event_tx,
        codex_op_tx: op_tx,
        bottom_pane: bottom,
        transcript: std::cell::RefCell::new(TranscriptView::new(
            Vec::new(),
            TranscriptRenderMode::Display,
        )),
        mouse_scroll: crate::mouse::MouseScrollState::default(),
        active_cell: None,
        active_cell_revision: 0,
        config: cfg,
        current_identity,
        active_identity_mask: None,
        reasoning_effort_overrides: HashMap::new(),
        auth_manager,
        thread_manager,
        otel_manager,
        session_header: SessionHeader::new(resolved_model.clone()),
        initial_user_message: None,
        token_info: None,
        stream_controller: None,
        plan_stream_controller: None,
        running_commands: HashMap::new(),
        suppressed_exec_calls: HashSet::new(),
        pending_collab_spawn_requests: HashMap::new(),
        skills_all: Vec::new(),
        skills_have_loaded: false,
        skills_request_in_flight: false,
        skills_refresh_pending: None,
        skills_initial_state: None,
        loaded_skills: Vec::new(),
        last_unified_wait: None,
        unified_exec_wait_streak: None,
        turn_sleep_inhibitor: SleepInhibitor::new(prevent_idle_sleep),
        task_complete_pending: false,
        unified_exec_processes: Vec::new(),
        changed_files: VecDeque::new(),
        agent_turn_running: false,
        mcp_startup_status: None,
        connectors_cache: ConnectorsCacheState::default(),
        interrupts: InterruptManager::new(),
        reasoning_buffer: String::new(),
        full_reasoning_buffer: String::new(),
        current_status: StatusIndicatorState::working(),
        retry_status: None,
        thread_id: None,
        thread_name: None,
        forked_from: None,
        context_compact_count: 0,
        counted_context_compaction_item_ids: HashSet::new(),
        pending_live_legacy_context_compactions: HashMap::new(),
        pending_replay_legacy_context_compactions: 0,
        frame_requester: FrameRequester::test_dummy(),
        show_welcome_banner: true,
        queued_user_messages: VecDeque::new(),
        suppress_session_configured_redraw: false,
        pending_notification: None,
        quit_shortcut_expires_at: None,
        quit_shortcut_key: None,
        is_review_mode: false,
        pre_review_token_info: None,
        needs_final_message_separator: false,
        had_work_activity: false,
        saw_plan_update_this_turn: false,
        saw_plan_item_this_turn: false,
        plan_delta_buffer: String::new(),
        plan_item_active: false,
        latest_proposed_plan_title: None,
        latest_update_plan: None,
        sidebar_agents: Vec::new(),
        last_separator_elapsed_secs: None,
        last_rendered_width: std::cell::Cell::new(None),
        last_transcript_area: std::cell::Cell::new(None),
        last_bottom_area: std::cell::Cell::new(None),
        feedback: adam_feedback::CodexFeedback::new(),
        current_rollout_path: None,
        external_editor_state: ExternalEditorState::Closed,
    };
    widget.set_model(&resolved_model);
    (widget, rx, op_rx)
}

async fn make_provider_required_chatwidget(
    auto_open: bool,
) -> (ChatWidget, tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    let adam_home = tempdir().expect("tempdir");
    let mut cfg = ConfigBuilder::default()
        .adam_home(adam_home.path().to_path_buf())
        .provider_config_required(false)
        .build()
        .await
        .expect("config");
    cfg.provider_config_required = true;
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let thread_manager = Arc::new(ThreadManager::new(
        cfg.adam_home.clone(),
        auth_manager.clone(),
        cfg.model_provider_id.as_str(),
        cfg.model_provider.clone(),
        SessionSource::Cli,
    ));
    let (tx_raw, rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(tx_raw);
    let init = ChatWidgetInit {
        config: cfg.clone(),
        thread_manager,
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx,
        initial_user_message: None,
        enhanced_keys_supported: false,
        auth_manager,
        feedback: adam_feedback::CodexFeedback::new(),
        is_first_run: true,
        startup: ChatWidgetStartup::NeedsProviderConfig { auto_open },
        otel_manager: test_otel_manager(&cfg, "No provider configured"),
    };

    (ChatWidget::new(init), rx)
}

async fn make_chatwidget_manual_with_frame_requester(
    model_override: Option<&str>,
    frame_requester: FrameRequester,
) -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (tx_raw, rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(tx_raw);
    let (op_tx, op_rx) = unbounded_channel::<Op>();
    let mut cfg = test_config().await;
    let resolved_model = model_override
        .map(str::to_owned)
        .unwrap_or_else(|| ModelsManager::get_model_offline(cfg.model.as_deref()));
    if let Some(model) = model_override {
        cfg.model = Some(model.to_string());
    }
    let otel_manager = test_otel_manager(&cfg, resolved_model.as_str());
    let mut bottom = BottomPane::new(BottomPaneParams {
        app_event_tx: app_event_tx.clone(),
        frame_requester: frame_requester.clone(),
        has_input_focus: true,
        enhanced_keys_supported: false,
        placeholder_text: "Ask Adam to do anything".to_string(),
        disable_paste_burst: false,
        animations_enabled: cfg.animations,
        skills: None,
    });
    bottom.set_steer_enabled(true);
    bottom.set_identities_enabled(cfg.features.enabled(Feature::Identities));
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let adam_home = cfg.adam_home.clone();
    let thread_manager = Arc::new(ThreadManager::new(
        adam_home,
        auth_manager.clone(),
        cfg.model_provider_id.as_str(),
        cfg.model_provider.clone(),
        SessionSource::Cli,
    ));
    let current_identity = Identity {
        kind: IdentityKind::Nobody,
        settings: Settings {
            model: resolved_model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };
    let prevent_idle_sleep = cfg.features.enabled(Feature::PreventIdleSleep);
    let mut widget = ChatWidget {
        app_event_tx,
        codex_op_tx: op_tx,
        bottom_pane: bottom,
        transcript: std::cell::RefCell::new(TranscriptView::new(
            Vec::new(),
            TranscriptRenderMode::Display,
        )),
        mouse_scroll: crate::mouse::MouseScrollState::default(),
        active_cell: None,
        active_cell_revision: 0,
        config: cfg,
        current_identity,
        active_identity_mask: None,
        reasoning_effort_overrides: HashMap::new(),
        auth_manager,
        thread_manager,
        otel_manager,
        session_header: SessionHeader::new(resolved_model.clone()),
        initial_user_message: None,
        token_info: None,
        stream_controller: None,
        plan_stream_controller: None,
        running_commands: HashMap::new(),
        suppressed_exec_calls: HashSet::new(),
        pending_collab_spawn_requests: HashMap::new(),
        skills_all: Vec::new(),
        skills_have_loaded: false,
        skills_request_in_flight: false,
        skills_refresh_pending: None,
        skills_initial_state: None,
        loaded_skills: Vec::new(),
        last_unified_wait: None,
        unified_exec_wait_streak: None,
        turn_sleep_inhibitor: SleepInhibitor::new(prevent_idle_sleep),
        task_complete_pending: false,
        unified_exec_processes: Vec::new(),
        changed_files: VecDeque::new(),
        agent_turn_running: false,
        mcp_startup_status: None,
        connectors_cache: ConnectorsCacheState::default(),
        interrupts: InterruptManager::new(),
        reasoning_buffer: String::new(),
        full_reasoning_buffer: String::new(),
        current_status: StatusIndicatorState::working(),
        retry_status: None,
        thread_id: None,
        thread_name: None,
        forked_from: None,
        context_compact_count: 0,
        counted_context_compaction_item_ids: HashSet::new(),
        pending_live_legacy_context_compactions: HashMap::new(),
        pending_replay_legacy_context_compactions: 0,
        frame_requester,
        show_welcome_banner: true,
        queued_user_messages: VecDeque::new(),
        suppress_session_configured_redraw: false,
        pending_notification: None,
        quit_shortcut_expires_at: None,
        quit_shortcut_key: None,
        is_review_mode: false,
        pre_review_token_info: None,
        needs_final_message_separator: false,
        had_work_activity: false,
        saw_plan_update_this_turn: false,
        saw_plan_item_this_turn: false,
        plan_delta_buffer: String::new(),
        plan_item_active: false,
        latest_proposed_plan_title: None,
        latest_update_plan: None,
        sidebar_agents: Vec::new(),
        last_separator_elapsed_secs: None,
        last_rendered_width: std::cell::Cell::new(None),
        last_transcript_area: std::cell::Cell::new(None),
        last_bottom_area: std::cell::Cell::new(None),
        feedback: adam_feedback::CodexFeedback::new(),
        current_rollout_path: None,
        external_editor_state: ExternalEditorState::Closed,
    };
    widget.set_model(&resolved_model);
    (widget, rx, op_rx)
}

async fn drain_draw_requests(draw_rx: &mut broadcast::Receiver<()>) {
    while tokio::time::timeout(std::time::Duration::from_millis(20), draw_rx.recv())
        .await
        .is_ok()
    {}
}

// ChatWidget may emit other `Op`s (e.g. history/logging updates) on the same channel; this helper
// filters until we see a submission op.
fn next_submit_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Op {
    loop {
        match op_rx.try_recv() {
            Ok(op @ Op::UserTurn { .. }) => return op,
            Ok(_) => continue,
            Err(TryRecvError::Empty) => panic!("expected a submit op but queue was empty"),
            Err(TryRecvError::Disconnected) => panic!("expected submit op but channel closed"),
        }
    }
}

async fn reload_chat_config_with_saved_providers(
    chat: &mut ChatWidget,
    configs: Vec<CustomProviderConfig>,
) {
    let mut last_selection = None;
    for config in &configs {
        persist_custom_provider_files(&chat.config.adam_home, config)
            .expect("persist provider config");
        last_selection = Some((custom_provider_ref(config), config.model.clone()));
    }
    if let Some((provider_id, model)) = last_selection {
        let model_ref = ModelRef::parse(&format!("{provider_id}:{model}")).expect("model ref");
        AdamStateStore::new(&chat.config.adam_home)
            .set_last_selected_model(&model_ref, None, None)
            .expect("persist active model selection");
    }

    chat.config = ConfigBuilder::default()
        .adam_home(chat.config.adam_home.clone())
        .build()
        .await
        .expect("reload config");
    chat.thread_manager = Arc::new(ThreadManager::new(
        chat.config.adam_home.clone(),
        chat.auth_manager.clone(),
        chat.config.model_provider_id.as_str(),
        chat.config.model_provider.clone(),
        SessionSource::Cli,
    ));
}

pub(crate) async fn make_chatwidget_manual_with_sender() -> (
    ChatWidget,
    AppEventSender,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (widget, rx, op_rx) = make_chatwidget_manual(None).await;
    let app_event_tx = widget.app_event_tx.clone();
    (widget, app_event_tx, rx, op_rx)
}

fn into_insert_history_cell(event: AppEvent) -> Option<Box<dyn HistoryCell>> {
    match event {
        AppEvent::InsertHistoryCell(cell) | AppEvent::InsertThreadHistoryCell { cell, .. } => {
            Some(cell)
        }
        _ => None,
    }
}

fn drain_insert_history(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> Vec<Vec<ratatui::text::Line<'static>>> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let Some(cell) = into_insert_history_cell(ev) {
            let mut lines = cell.display_lines(80);
            if !cell.is_stream_continuation() && !out.is_empty() && !lines.is_empty() {
                lines.insert(0, "".into());
            }
            out.push(lines)
        }
    }
    out
}

fn drain_events(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) -> Vec<AppEvent> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    out
}

fn lines_to_single_string(lines: &[ratatui::text::Line<'static>]) -> String {
    let mut s = String::new();
    for line in lines {
        for span in &line.spans {
            s.push_str(&span.content);
        }
        s.push('\n');
    }
    s
}

fn buffer_to_string(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn live_tail_text(tail: Option<crate::transcript_view::TranscriptLiveTail>) -> String {
    tail.map(|tail| lines_to_single_string(&tail.lines))
        .unwrap_or_default()
}

fn status_output_text(
    chat: &mut ChatWidget,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> String {
    chat.add_status_output();
    let cells = drain_insert_history(rx);
    let lines = cells.last().expect("expected /status history cell");
    lines_to_single_string(lines)
}

fn make_token_info(total_tokens: i64, context_window: i64) -> TokenUsageInfo {
    fn usage(total_tokens: i64) -> TokenUsage {
        TokenUsage {
            total_tokens,
            ..TokenUsage::default()
        }
    }

    TokenUsageInfo {
        total_token_usage: usage(total_tokens),
        last_token_usage: usage(total_tokens),
        model_context_window: Some(context_window),
    }
}

#[tokio::test]
async fn plan_implementation_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.open_plan_implementation_prompt();

    let popup = render_bottom_popup(&chat, 80);
    assert_snapshot!("plan_implementation_popup", popup);
}

#[tokio::test]
async fn plan_implementation_popup_no_selected_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.open_plan_implementation_prompt();
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));

    let popup = render_bottom_popup(&chat, 80);
    assert_snapshot!("plan_implementation_popup_no_selected", popup);
}

#[tokio::test]
async fn plan_implementation_popup_yes_emits_submit_message_event() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.open_plan_implementation_prompt();

    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let event = rx.try_recv().expect("expected AppEvent");
    let AppEvent::SubmitUserMessageWithMode { text, identity } = event else {
        panic!("expected SubmitUserMessageWithMode, got {event:?}");
    };
    assert_eq!(text, PLAN_IMPLEMENTATION_CODING_MESSAGE);
    assert_eq!(identity.kind, Some(IdentityKind::Programmer));
}

#[tokio::test]
async fn submit_user_message_with_mode_sets_coding_identity() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_feature_enabled(Feature::Identities, true);

    let code_mode = identities::programmer_mask(chat.thread_manager.as_ref())
        .expect("expected programmer identity");
    chat.submit_user_message_with_mode("Implement the plan.".to_string(), code_mode);

    match next_submit_op(&mut op_rx) {
        Op::UserTurn {
            identity:
                Some(Identity {
                    kind: IdentityKind::Programmer,
                    ..
                }),
            ..
        } => {}
        other => {
            panic!("expected Op::UserTurn with programmer identity, got {other:?}")
        }
    }
}

#[tokio::test]
async fn plan_implementation_popup_allows_transcript_page_scroll() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.open_plan_implementation_prompt();
    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));

    assert!(chat.transcript_scroll_offset() < at_tail);
}

#[tokio::test]
async fn plan_implementation_popup_keeps_arrow_keys_for_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.open_plan_implementation_prompt();
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));

    assert_eq!(chat.transcript_scroll_offset(), at_tail);
    let popup = render_bottom_popup(&chat, 80);
    assert!(popup.contains("› 2. No, stay in planner identity"));
}

#[tokio::test]
async fn plan_implementation_popup_allows_transcript_mouse_scroll_in_transcript_area() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);

    chat.open_plan_implementation_prompt();
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });

    assert!(chat.transcript_scroll_offset() < at_tail);
}

#[tokio::test]
async fn plan_implementation_popup_ignores_mouse_scroll_over_prompt() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);

    chat.open_plan_implementation_prompt();
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 1,
        row: area.height.saturating_sub(1),
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(chat.transcript_scroll_offset(), at_tail);
}

#[tokio::test]
async fn submitting_user_message_scrolls_transcript_to_bottom() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));
    assert!(chat.transcript_scroll_offset() < at_tail);

    chat.bottom_pane
        .set_composer_text("hello".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    assert_eq!(chat.transcript_scroll_offset(), at_tail);
}

#[tokio::test]
async fn turn_complete_scrolls_transcript_to_bottom() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));
    assert!(chat.transcript_scroll_offset() < at_tail);

    chat.handle_codex_event(Event {
        id: "turn-complete".to_string(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: Some("done".to_string()),
        }),
    });
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    assert_eq!(chat.transcript_scroll_offset(), at_tail);
}

#[tokio::test]
async fn replay_turn_complete_does_not_force_transcript_to_bottom() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));
    let scrolled_up = chat.transcript_scroll_offset();
    assert!(scrolled_up < at_tail);

    chat.handle_codex_event_replay(Event {
        id: "turn-complete".to_string(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: Some("done".to_string()),
        }),
    });
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    assert_eq!(chat.transcript_scroll_offset(), scrolled_up);
}

#[tokio::test]
async fn slash_status_scrolls_transcript_to_bottom() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));
    assert!(chat.transcript_scroll_offset() < at_tail);

    chat.bottom_pane
        .set_composer_text("/status".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    assert_eq!(chat.transcript_scroll_offset(), at_tail);
}

#[tokio::test]
async fn slash_plan_scrolls_to_latest_proposed_plan() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![
        Arc::new(TallTranscriptCell(30)) as Arc<dyn HistoryCell>,
        Arc::new(crate::history_cell::new_proposed_plan(
            "unique slash plan body".to_string(),
        )),
        Arc::new(TallTranscriptCell(30)),
    ]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.bottom_pane
        .set_composer_text("/plan".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let rendered = buffer_to_string(&buf);

    assert!(chat.transcript_scroll_offset() < at_tail);
    assert!(
        rendered.contains("unique slash plan body"),
        "expected latest proposed plan to be visible, got {rendered:?}"
    );
}

#[tokio::test]
async fn slash_plan_available_during_task() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.bottom_pane.set_task_running(true);
    chat.replace_transcript_cells(vec![
        Arc::new(TallTranscriptCell(30)) as Arc<dyn HistoryCell>,
        Arc::new(crate::history_cell::new_proposed_plan(
            "task running plan body".to_string(),
        )),
        Arc::new(TallTranscriptCell(30)),
    ]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    chat.bottom_pane
        .set_composer_text("/plan".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let rendered = buffer_to_string(&buf);
    let inserted = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();

    assert!(
        rendered.contains("task running plan body"),
        "expected proposed plan to be visible while task is running, got {rendered:?}"
    );
    assert!(
        !inserted.contains("disabled while a task is in progress"),
        "expected /plan to stay available during tasks, got {inserted:?}"
    );
}

#[tokio::test]
async fn slash_plan_without_plan_inserts_info_message() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(20))]);

    chat.bottom_pane
        .set_composer_text("/plan".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected one info message");
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("No proposed plan found in this session."),
        "info message should explain that no plan exists: {rendered:?}"
    );
}

#[tokio::test]
async fn slash_model_does_not_scroll_transcript_to_bottom() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));
    let scrolled_up = chat.transcript_scroll_offset();
    assert!(scrolled_up < at_tail);

    chat.bottom_pane
        .set_composer_text("/model".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    assert_eq!(chat.transcript_scroll_offset(), scrolled_up);
}

#[tokio::test]
async fn slash_rename_with_args_scrolls_transcript_to_bottom() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));
    assert!(chat.transcript_scroll_offset() < at_tail);

    chat.dispatch_command_with_args(SlashCommand::Rename, "new-name".to_string());
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    assert_eq!(chat.transcript_scroll_offset(), at_tail);
    let cells = drain_insert_history(&mut rx);
    assert!(
        !cells.is_empty(),
        "expected rename command to insert a confirmation cell"
    );
}

#[tokio::test]
async fn disabled_slash_command_error_scrolls_transcript_to_bottom() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.bottom_pane.set_task_running(true);
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));
    assert!(chat.transcript_scroll_offset() < at_tail);

    chat.bottom_pane
        .set_composer_text("/model".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    assert_eq!(chat.transcript_scroll_offset(), at_tail);
    let cells = drain_insert_history(&mut rx);
    assert!(
        !cells.is_empty(),
        "expected disabled slash command to insert an error cell"
    );
}

#[tokio::test]
async fn mouse_down_in_bottom_pane_does_not_start_transcript_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let bottom_area = chat.cached_bottom_area().expect("bottom area cached");
    let header_before = chat.current_status.header.clone();

    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: bottom_area.x,
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: bottom_area.x.saturating_add(10),
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: bottom_area.x.saturating_add(10),
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(chat.current_status.header, header_before);
}

#[tokio::test]
async fn mouse_down_in_bottom_pane_clears_stale_transcript_selection_without_copying() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let transcript_area = chat
        .cached_transcript_area()
        .expect("transcript area cached");
    let bottom_area = chat.cached_bottom_area().expect("bottom area cached");

    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: transcript_area.x,
        row: transcript_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: transcript_area.x.saturating_add(5),
        row: transcript_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: bottom_area.x,
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: bottom_area.x.saturating_add(5),
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(chat.current_status.header, "Working");
}

#[tokio::test]
async fn plan_implementation_popup_skips_replayed_turn_complete() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.replay_initial_messages(vec![EventMsg::TurnComplete(TurnCompleteEvent {
        last_agent_message: Some("Plan details".to_string()),
    })]);

    let popup = render_bottom_popup(&chat, 80);
    assert!(
        !popup.contains(PLAN_IMPLEMENTATION_TITLE),
        "expected no plan popup for replayed turn, got {popup:?}"
    );
}

#[tokio::test]
async fn plan_implementation_popup_skips_when_messages_queued() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);
    chat.bottom_pane.set_task_running(true);
    chat.queue_user_message("Queued message".into());

    chat.on_task_complete(Some("Plan details".to_string()), false);

    let popup = render_bottom_popup(&chat, 80);
    assert!(
        !popup.contains(PLAN_IMPLEMENTATION_TITLE),
        "expected no plan popup with queued messages, got {popup:?}"
    );
}

#[tokio::test]
async fn plan_implementation_popup_skips_without_proposed_plan() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.on_task_started();
    chat.on_plan_update(UpdatePlanArgs {
        explanation: None,
        plan: vec![PlanItemArg {
            step: "First".to_string(),
            status: StepStatus::Pending,
        }],
    });
    chat.on_task_complete(None, false);

    let popup = render_bottom_popup(&chat, 80);
    assert!(
        !popup.contains(PLAN_IMPLEMENTATION_TITLE),
        "expected no plan popup without proposed plan output, got {popup:?}"
    );
}

#[tokio::test]
async fn plan_implementation_popup_shows_after_proposed_plan_output() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.on_task_started();
    chat.on_plan_delta("- Step 1\n- Step 2\n".to_string());
    chat.on_plan_item_completed("- Step 1\n- Step 2\n".to_string());
    chat.on_task_complete(None, false);

    let popup = render_bottom_popup(&chat, 80);
    assert!(
        popup.contains(PLAN_IMPLEMENTATION_TITLE),
        "expected plan popup after proposed plan output, got {popup:?}"
    );
}

#[tokio::test]
async fn streamed_proposed_plan_background_fills_rendered_row() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.on_task_started();
    chat.on_plan_delta("- Step 1\n".to_string());
    chat.on_plan_item_completed("- Step 1\n".to_string());
    chat.on_task_complete(None, false);

    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    let Some(plan_bg) = proposed_plan_style().bg else {
        return;
    };
    let plan_row = (0..area.height)
        .find(|y| {
            (0..area.width)
                .map(|x| buf[(x, *y)].symbol())
                .collect::<String>()
                .contains("Step 1")
        })
        .expect("expected rendered proposed plan row");

    for x in 0..area.width {
        assert_eq!(
            buf[(x, plan_row)].style().bg,
            Some(plan_bg),
            "expected proposed plan background at x={x}, y={plan_row}"
        );
    }
}

#[tokio::test]
async fn sidebar_task_waits_for_completed_proposed_plan_heading() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.on_task_started();
    chat.on_plan_delta("Intro\n".to_string());
    assert!(chat.sidebar_snapshot().task.is_none());

    chat.on_plan_delta("# Build Sidebar #\n- Step 1\n".to_string());
    assert!(chat.sidebar_snapshot().task.is_none());

    chat.on_plan_item_completed("# Build Sidebar #\n- Step 1\n".to_string());

    assert_eq!(
        chat.sidebar_snapshot().task.map(|task| task.title),
        Some("Build Sidebar".to_string())
    );
}

#[tokio::test]
async fn sidebar_task_uses_completed_proposed_plan_heading() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.on_task_started();
    chat.on_plan_delta("# Streaming Title\n".to_string());
    assert!(chat.sidebar_snapshot().task.is_none());

    chat.on_plan_item_completed("## Final Title\n- Step 1\n".to_string());

    assert_eq!(
        chat.sidebar_snapshot().task.map(|task| task.title),
        Some("Final Title".to_string())
    );
}

#[tokio::test]
async fn sidebar_task_hides_proposed_plan_without_heading() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.on_task_started();
    chat.on_plan_delta("- Step 1\n".to_string());
    chat.on_plan_item_completed("- Step 1\n".to_string());

    assert!(chat.sidebar_snapshot().task.is_none());
}

#[tokio::test]
async fn sidebar_task_keeps_existing_title_while_next_plan_streams() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.on_task_started();
    chat.on_plan_item_completed("# Existing Task\n- Step 1\n".to_string());
    assert_eq!(
        chat.sidebar_snapshot().task.map(|task| task.title),
        Some("Existing Task".to_string())
    );

    chat.on_plan_delta("# Partial New Task\n- Step 1\n".to_string());
    assert_eq!(
        chat.sidebar_snapshot().task.map(|task| task.title),
        Some("Existing Task".to_string())
    );

    chat.on_plan_item_completed("# Final New Task\n- Step 1\n".to_string());
    assert_eq!(
        chat.sidebar_snapshot().task.map(|task| task.title),
        Some("Final New Task".to_string())
    );
}

#[tokio::test]
async fn sidebar_task_clears_existing_title_when_completed_plan_has_no_heading() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.on_task_started();
    chat.on_plan_item_completed("# Existing Task\n- Step 1\n".to_string());
    chat.on_plan_delta("- New Step 1\n".to_string());
    assert_eq!(
        chat.sidebar_snapshot().task.map(|task| task.title),
        Some("Existing Task".to_string())
    );

    chat.on_plan_item_completed("- New Step 1\n".to_string());

    assert!(chat.sidebar_snapshot().task.is_none());
}

#[tokio::test]
async fn prevent_idle_sleep_syncs_with_turn_lifecycle() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::PreventIdleSleep, true);

    chat.on_task_started();

    assert!(chat.agent_turn_running);
    assert!(chat.turn_sleep_inhibitor.is_turn_running());
    assert!(chat.bottom_pane.is_task_running());

    chat.on_task_complete(None, false);

    assert!(!chat.agent_turn_running);
    assert!(!chat.turn_sleep_inhibitor.is_turn_running());
    assert!(!chat.bottom_pane.is_task_running());
}

#[tokio::test]
async fn prevent_idle_sleep_resets_when_turn_is_finalized() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::PreventIdleSleep, true);
    chat.on_task_started();

    chat.finalize_turn();

    assert!(!chat.agent_turn_running);
    assert!(!chat.turn_sleep_inhibitor.is_turn_running());
    assert!(!chat.bottom_pane.is_task_running());
}

#[tokio::test]
async fn replayed_turn_started_syncs_sleep_inhibitor_state() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::PreventIdleSleep, true);

    chat.replay_initial_messages(vec![EventMsg::TurnStarted(TurnStartedEvent {
        model_context_window: None,
        identity_kind: IdentityKind::Nobody,
    })]);

    assert!(chat.agent_turn_running);
    assert!(chat.turn_sleep_inhibitor.is_turn_running());
    assert!(chat.bottom_pane.is_task_running());
}

#[tokio::test]
async fn toggling_prevent_idle_sleep_resyncs_inhibitor_with_running_state() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.agent_turn_running = true;
    chat.turn_sleep_inhibitor
        .set_turn_running(/* turn_running */ false);

    chat.set_feature_enabled(Feature::PreventIdleSleep, true);
    assert!(chat.turn_sleep_inhibitor.is_turn_running());

    chat.turn_sleep_inhibitor
        .set_turn_running(/* turn_running */ false);
    chat.set_feature_enabled(Feature::PreventIdleSleep, false);
    assert!(chat.turn_sleep_inhibitor.is_turn_running());
}

// (removed experimental resize snapshot test)

#[tokio::test]
async fn exec_approval_emits_proposed_command_and_decision_history() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Trigger an exec approval request with a short, single-line command
    let ev = ExecApprovalRequestEvent {
        call_id: "call-short".into(),
        turn_id: "turn-short".into(),
        command: vec!["bash".into(), "-lc".into(), "echo hello world".into()],
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        reason: Some(
            "this is a test reason such as one that would be produced by the model".into(),
        ),
        proposed_execpolicy_amendment: None,
        parsed_cmd: vec![],
    };
    chat.handle_codex_event(Event {
        id: "sub-short".into(),
        msg: EventMsg::ExecApprovalRequest(ev),
    });

    let proposed_cells = drain_insert_history(&mut rx);
    assert!(
        proposed_cells.is_empty(),
        "expected approval request to render via modal without emitting history cells"
    );

    // The approval modal should display the command snippet for user confirmation.
    let area = Rect::new(0, 0, 80, chat.desired_height(80));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    chat.render(area, &mut buf);
    assert_snapshot!("exec_approval_modal_exec", format!("{buf:?}"));

    // Approve via keyboard and verify a concise decision history line is added
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let decision = drain_insert_history(&mut rx)
        .pop()
        .expect("expected decision cell in history");
    assert_snapshot!(
        "exec_approval_history_decision_approved_short",
        lines_to_single_string(&decision)
    );
}

#[tokio::test]
async fn exec_approval_decision_truncates_multiline_and_long_commands() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Multiline command: modal should show full command, history records decision only
    let ev_multi = ExecApprovalRequestEvent {
        call_id: "call-multi".into(),
        turn_id: "turn-multi".into(),
        command: vec!["bash".into(), "-lc".into(), "echo line1\necho line2".into()],
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        reason: Some(
            "this is a test reason such as one that would be produced by the model".into(),
        ),
        proposed_execpolicy_amendment: None,
        parsed_cmd: vec![],
    };
    chat.handle_codex_event(Event {
        id: "sub-multi".into(),
        msg: EventMsg::ExecApprovalRequest(ev_multi),
    });
    let proposed_multi = drain_insert_history(&mut rx);
    assert!(
        proposed_multi.is_empty(),
        "expected multiline approval request to render via modal without emitting history cells"
    );

    let area = Rect::new(0, 0, 80, chat.desired_height(80));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    chat.render(area, &mut buf);
    let mut saw_first_line = false;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        if row.contains("echo line1") {
            saw_first_line = true;
            break;
        }
    }
    assert!(
        saw_first_line,
        "expected modal to show first line of multiline snippet"
    );

    // Deny via keyboard; decision snippet should be single-line and elided with " ..."
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    let aborted_multi = drain_insert_history(&mut rx)
        .pop()
        .expect("expected aborted decision cell (multiline)");
    assert_snapshot!(
        "exec_approval_history_decision_aborted_multiline",
        lines_to_single_string(&aborted_multi)
    );

    // Very long single-line command: decision snippet should be truncated <= 80 chars with trailing ...
    let long = format!("echo {}", "a".repeat(200));
    let ev_long = ExecApprovalRequestEvent {
        call_id: "call-long".into(),
        turn_id: "turn-long".into(),
        command: vec!["bash".into(), "-lc".into(), long],
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        reason: None,
        proposed_execpolicy_amendment: None,
        parsed_cmd: vec![],
    };
    chat.handle_codex_event(Event {
        id: "sub-long".into(),
        msg: EventMsg::ExecApprovalRequest(ev_long),
    });
    let proposed_long = drain_insert_history(&mut rx);
    assert!(
        proposed_long.is_empty(),
        "expected long approval request to avoid emitting history cells before decision"
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    let aborted_long = drain_insert_history(&mut rx)
        .pop()
        .expect("expected aborted decision cell (long)");
    assert_snapshot!(
        "exec_approval_history_decision_aborted_long",
        lines_to_single_string(&aborted_long)
    );
}

// --- Small helpers to tersely drive exec begin/end and snapshot active cell ---
fn begin_exec_with_source(
    chat: &mut ChatWidget,
    call_id: &str,
    raw_cmd: &str,
    source: ExecCommandSource,
) -> ExecCommandBeginEvent {
    // Build the full command vec and parse it using core's parser,
    // then convert to protocol variants for the event payload.
    let command = vec!["bash".to_string(), "-lc".to_string(), raw_cmd.to_string()];
    let parsed_cmd: Vec<ParsedCommand> = adam_agent::parse_command::parse_command(&command);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let interaction_input = None;
    let event = ExecCommandBeginEvent {
        call_id: call_id.to_string(),
        process_id: None,
        turn_id: "turn-1".to_string(),
        command,
        cwd,
        parsed_cmd,
        source,
        interaction_input,
    };
    chat.handle_codex_event(Event {
        id: call_id.to_string(),
        msg: EventMsg::ExecCommandBegin(event.clone()),
    });
    event
}

fn begin_unified_exec_startup(
    chat: &mut ChatWidget,
    call_id: &str,
    process_id: &str,
    raw_cmd: &str,
) -> ExecCommandBeginEvent {
    let command = vec!["bash".to_string(), "-lc".to_string(), raw_cmd.to_string()];
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let event = ExecCommandBeginEvent {
        call_id: call_id.to_string(),
        process_id: Some(process_id.to_string()),
        turn_id: "turn-1".to_string(),
        command,
        cwd,
        parsed_cmd: Vec::new(),
        source: ExecCommandSource::UnifiedExecStartup,
        interaction_input: None,
    };
    chat.handle_codex_event(Event {
        id: call_id.to_string(),
        msg: EventMsg::ExecCommandBegin(event.clone()),
    });
    event
}

fn terminal_interaction(chat: &mut ChatWidget, call_id: &str, process_id: &str, stdin: &str) {
    chat.handle_codex_event(Event {
        id: call_id.to_string(),
        msg: EventMsg::TerminalInteraction(TerminalInteractionEvent {
            call_id: call_id.to_string(),
            process_id: process_id.to_string(),
            stdin: stdin.to_string(),
        }),
    });
}

fn begin_exec(chat: &mut ChatWidget, call_id: &str, raw_cmd: &str) -> ExecCommandBeginEvent {
    begin_exec_with_source(chat, call_id, raw_cmd, ExecCommandSource::Agent)
}

fn end_exec(
    chat: &mut ChatWidget,
    begin_event: ExecCommandBeginEvent,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) {
    let aggregated = if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{stdout}{stderr}")
    };
    let ExecCommandBeginEvent {
        call_id,
        turn_id,
        command,
        cwd,
        parsed_cmd,
        source,
        interaction_input,
        process_id,
    } = begin_event;
    chat.handle_codex_event(Event {
        id: call_id.clone(),
        msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id,
            process_id,
            turn_id,
            command,
            cwd,
            parsed_cmd,
            source,
            interaction_input,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            aggregated_output: aggregated.clone(),
            exit_code,
            duration: std::time::Duration::from_millis(5),
            formatted_output: aggregated,
        }),
    });
}

fn output_delta(chat: &mut ChatWidget, call_id: &str, chunk: &str) {
    chat.handle_codex_event(Event {
        id: call_id.to_string(),
        msg: EventMsg::ExecCommandOutputDelta(ExecCommandOutputDeltaEvent {
            call_id: call_id.to_string(),
            stream: ExecOutputStream::Stdout,
            chunk: chunk.as_bytes().to_vec(),
        }),
    });
}

fn active_blob(chat: &ChatWidget) -> String {
    let lines = chat
        .active_cell
        .as_ref()
        .expect("active cell present")
        .display_lines(80);
    lines_to_single_string(&lines)
}

#[tokio::test]
async fn add_boxed_history_does_not_render_to_check_visibility() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    chat.add_boxed_history(Box::new(CountingHistoryCell {
        calls: calls.clone(),
    }));

    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[tokio::test]
async fn exec_events_render_while_agent_message_stream_is_active() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "msg".into(),
        msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
            delta: "thinking without newline".into(),
        }),
    });
    let begin = begin_exec(&mut chat, "call-1", "printf hello");

    assert!(chat.active_cell.is_some());

    output_delta(&mut chat, "call-1", "hello");
    assert!(active_blob(&chat).contains("hello"));

    end_exec(&mut chat, begin, "hello", "", 0);
    assert!(chat.active_cell.is_none());
}

fn get_available_model(chat: &ChatWidget, model: &str) -> ModelPreset {
    let models = chat
        .thread_manager
        .try_list_models(&chat.config)
        .expect("models lock available");
    models
        .iter()
        .find(|&preset| preset.model == model)
        .cloned()
        .unwrap_or_else(|| panic!("{model} preset not found"))
}

#[tokio::test]
async fn empty_enter_during_task_does_not_queue() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    // Simulate running task so submissions would normally be queued.
    chat.bottom_pane.set_task_running(true);

    // Press Enter with an empty composer.
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Ensure nothing was queued.
    assert!(chat.queued_user_messages.is_empty());
}

#[tokio::test]
async fn alt_up_edits_most_recent_queued_message() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    // Simulate a running task so messages would normally be queued.
    chat.bottom_pane.set_task_running(true);

    // Seed two queued messages.
    chat.queued_user_messages
        .push_back(UserMessage::from("first queued".to_string()));
    chat.queued_user_messages
        .push_back(UserMessage::from("second queued".to_string()));
    chat.refresh_queued_user_messages();

    // Press Alt+Up to edit the most recent (last) queued message.
    chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));

    // Composer should now contain the last queued message.
    assert_eq!(
        chat.bottom_pane.composer_text(),
        "second queued".to_string()
    );
    // And the queue should now contain only the remaining (older) item.
    assert_eq!(chat.queued_user_messages.len(), 1);
    assert_eq!(
        chat.queued_user_messages.front().unwrap().text,
        "first queued"
    );
}

/// Pressing Up to recall the most recent history entry and immediately queuing
/// it while a task is running should always enqueue the same text, even when it
/// is queued repeatedly.
#[tokio::test]
async fn enqueueing_history_prompt_multiple_times_is_stable() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(ThreadId::new());

    // Submit an initial prompt to seed history.
    chat.bottom_pane
        .set_composer_text("repeat me".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Simulate an active task so further submissions are queued.
    chat.bottom_pane.set_task_running(true);

    for _ in 0..3 {
        // Recall the prompt from history and ensure it is what we expect.
        chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(chat.bottom_pane.composer_text(), "repeat me");

        // Queue the prompt while the task is running.
        chat.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }

    assert_eq!(chat.queued_user_messages.len(), 3);
    for message in chat.queued_user_messages.iter() {
        assert_eq!(message.text, "repeat me");
    }
}

#[tokio::test]
async fn streaming_final_answer_keeps_task_running_state() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(ThreadId::new());

    chat.on_task_started();
    chat.on_agent_message_delta("Final answer line\n".to_string());
    chat.on_commit_tick();

    assert!(chat.bottom_pane.is_task_running());
    assert!(chat.bottom_pane.status_widget().is_none());

    chat.bottom_pane
        .set_composer_text("queued submission".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    assert_eq!(chat.queued_user_messages.len(), 1);
    assert_eq!(
        chat.queued_user_messages.front().unwrap().text,
        "queued submission"
    );
    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    match op_rx.try_recv() {
        Ok(Op::Interrupt) => {}
        other => panic!("expected Op::Interrupt, got {other:?}"),
    }
    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());
}

#[tokio::test]
async fn ctrl_c_shutdown_works_with_caps_lock() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL));

    assert_matches!(rx.try_recv(), Ok(AppEvent::Exit(ExitMode::Immediate)));
}

#[tokio::test]
async fn ctrl_shift_c_copies_selection_without_quit() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(1))]);

    let area = Rect::new(0, 0, 40, 8);
    let mut buffer = Buffer::empty(area);
    chat.render(area, &mut buffer);
    chat.transcript.borrow_mut().set_selection_for_test(
        TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        },
        TranscriptSelectionPoint {
            line_index: 0,
            column: 5,
        },
    );

    chat.handle_key_event(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));

    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
    assert_ne!(chat.current_status.header, "No selection to copy");
}

#[tokio::test]
async fn transcript_copy_success_clears_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(1))]);

    let area = Rect::new(0, 0, 40, 8);
    let mut buffer = Buffer::empty(area);
    chat.render(area, &mut buffer);
    chat.transcript.borrow_mut().set_selection_for_test(
        TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        },
        TranscriptSelectionPoint {
            line_index: 0,
            column: 5,
        },
    );

    chat.copy_transcript_selection_with(|text, _config| {
        assert_eq!(text, "trans");
        Ok(())
    });

    assert_eq!(chat.current_status.header, "Selection copied");
    assert!(!chat.transcript.borrow().selection_active_for_test());
}

#[tokio::test]
async fn completed_mouse_selection_copy_success_clears_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(1))]);

    let area = Rect::new(0, 0, 40, 8);
    let mut buffer = Buffer::empty(area);
    chat.render(area, &mut buffer);
    chat.transcript.borrow_mut().set_selection_for_test(
        TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        },
        TranscriptSelectionPoint {
            line_index: 0,
            column: 5,
        },
    );

    chat.copy_completed_transcript_selection_with(Some("trans".to_string()), |text, _config| {
        assert_eq!(text, "trans");
        Ok(())
    });

    assert_eq!(chat.current_status.header, "Selection copied");
    assert!(!chat.transcript.borrow().selection_active_for_test());
}

#[tokio::test]
async fn transcript_copy_failure_keeps_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(1))]);

    let area = Rect::new(0, 0, 40, 8);
    let mut buffer = Buffer::empty(area);
    chat.render(area, &mut buffer);
    chat.transcript.borrow_mut().set_selection_for_test(
        TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        },
        TranscriptSelectionPoint {
            line_index: 0,
            column: 5,
        },
    );

    chat.copy_transcript_selection_with(|text, _config| {
        assert_eq!(text, "trans");
        Err("clipboard unavailable".to_string())
    });

    assert_eq!(
        chat.current_status.header,
        "Copy failed: clipboard unavailable"
    );
    assert!(chat.transcript.borrow().selection_active_for_test());
}

#[tokio::test]
async fn ctrl_shift_c_without_selection_does_not_quit() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;

    chat.handle_key_event(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));

    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(chat.current_status.header, "No selection to copy");
}

#[tokio::test]
async fn ctrl_d_quits_without_prompt() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_matches!(rx.try_recv(), Ok(AppEvent::Exit(ExitMode::Immediate)));
}

#[tokio::test]
async fn ctrl_d_with_modal_open_does_not_quit() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.open_approvals_popup();
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));

    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn ctrl_c_cleared_prompt_is_recoverable_via_history() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;

    chat.bottom_pane.insert_str("draft message ");
    chat.bottom_pane
        .attach_image(PathBuf::from("/tmp/preview.png"));
    let placeholder = "[Image #1]";
    assert!(
        chat.bottom_pane.composer_text().ends_with(placeholder),
        "expected placeholder {placeholder:?} in composer text"
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(chat.bottom_pane.composer_text().is_empty());
    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());

    chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    let restored_text = chat.bottom_pane.composer_text();
    assert!(
        restored_text.ends_with(placeholder),
        "expected placeholder {placeholder:?} after history recall"
    );
    assert!(restored_text.starts_with("draft message "));
    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());

    let images = chat.bottom_pane.take_recent_submission_images();
    assert_eq!(vec![PathBuf::from("/tmp/preview.png")], images);
}

#[tokio::test]
async fn exec_history_cell_shows_working_then_completed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Begin command
    let begin = begin_exec(&mut chat, "call-1", "echo done");

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 0, "no exec cell should have been flushed yet");

    // End command successfully
    end_exec(&mut chat, begin, "done", "", 0);

    let cells = drain_insert_history(&mut rx);
    // Exec end now finalizes and flushes the exec cell immediately.
    assert_eq!(cells.len(), 1, "expected finalized exec cell to flush");
    // Inspect the flushed exec cell rendering.
    let lines = &cells[0];
    let blob = lines_to_single_string(lines);
    // New behavior: no glyph markers; ensure command is shown and no panic.
    assert!(
        blob.contains("• Ran"),
        "expected summary header present: {blob:?}"
    );
    assert!(
        blob.contains("echo done"),
        "expected command text to be present: {blob:?}"
    );
}

#[tokio::test]
async fn exec_history_cell_shows_working_then_failed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Begin command
    let begin = begin_exec(&mut chat, "call-2", "false");
    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 0, "no exec cell should have been flushed yet");

    // End command with failure
    end_exec(&mut chat, begin, "", "Bloop", 2);

    let cells = drain_insert_history(&mut rx);
    // Exec end with failure should also flush immediately.
    assert_eq!(cells.len(), 1, "expected finalized exec cell to flush");
    let lines = &cells[0];
    let blob = lines_to_single_string(lines);
    assert!(
        blob.contains("• Ran false"),
        "expected command and header text present: {blob:?}"
    );
    assert!(blob.to_lowercase().contains("bloop"), "expected error text");
}

#[tokio::test]
async fn exec_end_without_begin_uses_event_command() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let command = vec![
        "bash".to_string(),
        "-lc".to_string(),
        "echo orphaned".to_string(),
    ];
    let parsed_cmd = adam_agent::parse_command::parse_command(&command);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    chat.handle_codex_event(Event {
        id: "call-orphan".to_string(),
        msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "call-orphan".to_string(),
            process_id: None,
            turn_id: "turn-1".to_string(),
            command,
            cwd,
            parsed_cmd,
            source: ExecCommandSource::Agent,
            interaction_input: None,
            stdout: "done".to_string(),
            stderr: String::new(),
            aggregated_output: "done".to_string(),
            exit_code: 0,
            duration: std::time::Duration::from_millis(5),
            formatted_output: "done".to_string(),
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected finalized exec cell to flush");
    let blob = lines_to_single_string(&cells[0]);
    assert!(
        blob.contains("• Ran echo orphaned"),
        "expected command text to come from event: {blob:?}"
    );
    assert!(
        !blob.contains("call-orphan"),
        "call id should not be rendered when event has the command: {blob:?}"
    );
}

#[tokio::test]
async fn exec_history_shows_unified_exec_startup_commands() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    let begin = begin_exec_with_source(
        &mut chat,
        "call-startup",
        "echo unified exec startup",
        ExecCommandSource::UnifiedExecStartup,
    );
    assert!(
        drain_insert_history(&mut rx).is_empty(),
        "exec begin should not flush until completion"
    );

    end_exec(&mut chat, begin, "echo unified exec startup\n", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected finalized exec cell to flush");
    let blob = lines_to_single_string(&cells[0]);
    assert!(
        blob.contains("• Ran echo unified exec startup"),
        "expected startup command to render: {blob:?}"
    );
}

#[tokio::test]
async fn exec_history_shows_unified_exec_tool_calls() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    let begin = begin_exec_with_source(
        &mut chat,
        "call-startup",
        "ls",
        ExecCommandSource::UnifiedExecStartup,
    );
    end_exec(&mut chat, begin, "", "", 0);

    let blob = active_blob(&chat);
    assert_eq!(blob, "• Exploring\n  └ List ls\n");
}

#[tokio::test]
async fn unified_exec_end_after_task_complete_is_suppressed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    let begin = begin_exec_with_source(
        &mut chat,
        "call-startup",
        "echo unified exec startup",
        ExecCommandSource::UnifiedExecStartup,
    );
    drain_insert_history(&mut rx);

    chat.on_task_complete(None, false);
    end_exec(&mut chat, begin, "", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected unified exec end after task complete to be suppressed"
    );
}

#[tokio::test]
async fn unified_exec_interaction_after_task_complete_is_suppressed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    chat.on_task_complete(None, false);

    chat.handle_codex_event(Event {
        id: "call-1".to_string(),
        msg: EventMsg::TerminalInteraction(TerminalInteractionEvent {
            call_id: "call-1".to_string(),
            process_id: "proc-1".to_string(),
            stdin: "ls\n".to_string(),
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected unified exec interaction after task complete to be suppressed"
    );
}

#[tokio::test]
async fn unified_exec_wait_after_final_agent_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });

    begin_unified_exec_startup(&mut chat, "call-wait", "proc-1", "cargo test -p adam-agent");
    terminal_interaction(&mut chat, "call-wait-stdin", "proc-1", "");

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::AgentMessage(AgentMessageEvent {
            message: "Final response.".into(),
        }),
    });
    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: Some("Final response.".into()),
        }),
    });

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_snapshot!("unified_exec_wait_after_final_agent_message", combined);
}

#[tokio::test]
async fn unified_exec_wait_before_streamed_agent_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });

    begin_unified_exec_startup(
        &mut chat,
        "call-wait-stream",
        "proc-1",
        "cargo test -p adam-agent",
    );
    terminal_interaction(&mut chat, "call-wait-stream-stdin", "proc-1", "");

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
            delta: "Streaming response.".into(),
        }),
    });
    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: None,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_snapshot!("unified_exec_wait_before_streamed_agent_message", combined);
}

#[tokio::test]
async fn unified_exec_wait_status_header_updates_on_late_command_display() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    chat.unified_exec_processes.push(UnifiedExecProcessSummary {
        key: "proc-1".to_string(),
        call_id: "call-1".to_string(),
        command_display: "sleep 5".to_string(),
        started_at: Instant::now(),
        visible: false,
        recent_chunks: Vec::new(),
    });

    chat.on_terminal_interaction(TerminalInteractionEvent {
        call_id: "call-1".to_string(),
        process_id: "proc-1".to_string(),
        stdin: String::new(),
    });

    assert!(chat.active_cell.is_none());
    assert_eq!(
        chat.current_status.header,
        "Waiting for background terminal"
    );
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), "Waiting for background terminal");
    assert_eq!(status.details(), Some("sleep 5"));
}

#[tokio::test]
async fn unified_exec_waiting_multiple_empty_snapshots() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(&mut chat, "call-wait-1", "proc-1", "just fix");

    terminal_interaction(&mut chat, "call-wait-1a", "proc-1", "");
    terminal_interaction(&mut chat, "call-wait-1b", "proc-1", "");
    assert_eq!(
        chat.current_status.header,
        "Waiting for background terminal"
    );

    chat.handle_codex_event(Event {
        id: "turn-wait-1".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: None,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_snapshot!("unified_exec_waiting_multiple_empty_after", combined);
}

#[tokio::test]
async fn unified_exec_wait_status_renders_command_in_single_details_row() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(
        &mut chat,
        "call-wait-ui",
        "proc-ui",
        "cargo test -p adam-agent -- --exact",
    );

    terminal_interaction(&mut chat, "call-wait-ui-stdin", "proc-ui", "");

    let rendered = render_bottom_popup(&chat, 48);
    assert_snapshot!(
        "unified_exec_wait_status_renders_command_in_single_details_row",
        rendered
    );
}

#[tokio::test]
async fn short_unified_exec_does_not_show_footer_or_sidebar() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    let begin = begin_unified_exec_startup(&mut chat, "call-short", "proc-short", "true");
    end_exec(&mut chat, begin, "", "", 0);

    assert!(chat.unified_exec_processes.is_empty());
    assert!(chat.bottom_pane.unified_exec_processes().is_empty());

    let sidebar = chat.sidebar_snapshot();
    assert!(sidebar.task.is_none());
}

#[tokio::test]
async fn long_unified_exec_promotes_to_footer_only() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();

    begin_unified_exec_startup(&mut chat, "call-long", "proc-long", "sleep 5");
    assert!(chat.bottom_pane.unified_exec_processes().is_empty());
    assert!(chat.sidebar_snapshot().task.is_none());

    let process = chat
        .unified_exec_processes
        .iter_mut()
        .find(|process| process.key == "proc-long")
        .expect("process should be tracked");
    process.started_at = Instant::now() - UNIFIED_EXEC_VISIBILITY_DELAY - Duration::from_millis(1);

    chat.prepare_for_draw();

    assert_eq!(
        chat.bottom_pane.unified_exec_processes(),
        &["sleep 5".to_string()]
    );
    assert!(chat.sidebar_snapshot().task.is_none());
}

#[tokio::test]
async fn terminal_interaction_promotes_unified_exec_immediately() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(&mut chat, "call-interaction", "proc-interaction", "sleep 5");

    terminal_interaction(&mut chat, "call-interaction-stdin", "proc-interaction", "");

    assert_eq!(
        chat.bottom_pane.unified_exec_processes(),
        &["sleep 5".to_string()]
    );
    assert_eq!(
        chat.current_status.header,
        "Waiting for background terminal"
    );
    assert_eq!(chat.current_status.details, Some("sleep 5".to_string()));
    assert!(chat.sidebar_snapshot().task.is_none());
}

#[tokio::test]
async fn unified_exec_empty_then_non_empty_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(&mut chat, "call-wait-2", "proc-2", "just fix");

    terminal_interaction(&mut chat, "call-wait-2a", "proc-2", "");
    terminal_interaction(&mut chat, "call-wait-2b", "proc-2", "ls\n");

    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_snapshot!("unified_exec_empty_then_non_empty_after", combined);
}

#[tokio::test]
async fn unified_exec_non_empty_then_empty_snapshots() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(&mut chat, "call-wait-3", "proc-3", "just fix");

    terminal_interaction(&mut chat, "call-wait-3a", "proc-3", "pwd\n");
    terminal_interaction(&mut chat, "call-wait-3b", "proc-3", "");
    assert_eq!(
        chat.current_status.header,
        "Waiting for background terminal"
    );
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), "Waiting for background terminal");
    assert_eq!(status.details(), Some("just fix"));
    let pre_cells = drain_insert_history(&mut rx);
    let active_combined = pre_cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_snapshot!("unified_exec_non_empty_then_empty_active", active_combined);

    chat.handle_codex_event(Event {
        id: "turn-wait-3".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: None,
        }),
    });

    let post_cells = drain_insert_history(&mut rx);
    let mut combined = pre_cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    let post = post_cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    if !combined.is_empty() && !post.is_empty() {
        combined.push('\n');
    }
    combined.push_str(&post);
    assert_snapshot!("unified_exec_non_empty_then_empty_after", combined);
}

/// /review opens the centered App-owned review modal.
#[tokio::test]
async fn review_command_opens_review_modal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Review);

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenReviewModal));
    assert!(!render_bottom_popup(&chat, 80).contains("Select a review preset"));
}

#[tokio::test]
async fn experimental_command_opens_experimental_features_modal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Experimental);

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenExperimentalFeaturesModal));
    assert!(!render_bottom_popup(&chat, 80).contains("Experimental features"));
}

#[tokio::test]
async fn skills_command_opens_centered_skills_modal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Skills);

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenSkillsModal));
    assert!(!render_bottom_popup(&chat, 80).contains("Skills"));
}

#[tokio::test]
async fn mcp_command_opens_centered_mcp_modal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Mcp);

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenMcpToolsModal));
    assert!(
        rx.try_recv().is_err(),
        "expected /mcp to avoid writing history directly"
    );
}

#[tokio::test]
async fn skills_modal_items_reports_loading_before_first_response() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    assert_matches!(chat.skills_modal_items(), SkillsModalItems::Loading);
}

#[tokio::test]
async fn skills_modal_items_reports_empty_after_empty_response() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_skills_from_response(&adam_agent::protocol::ListSkillsResponseEvent {
        skills: vec![adam_agent::protocol::SkillsListEntry {
            cwd: chat.config.cwd.clone(),
            skills: Vec::new(),
            errors: Vec::new(),
        }],
    });

    assert_matches!(chat.skills_modal_items(), SkillsModalItems::Empty);
}

#[tokio::test]
async fn skills_modal_items_returns_cached_items_while_refreshing() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_skills_from_response(&adam_agent::protocol::ListSkillsResponseEvent {
        skills: vec![adam_agent::protocol::SkillsListEntry {
            cwd: chat.config.cwd.clone(),
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
    chat.skills_request_in_flight = true;

    assert_matches!(
        chat.skills_modal_items(),
        SkillsModalItems::Ready(items) if items.len() == 1
    );
}

#[tokio::test]
async fn skills_refresh_queues_while_request_in_flight() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.skills_request_in_flight = true;

    chat.request_skills_refresh(true);

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(chat.skills_refresh_pending, Some(true));
}

#[tokio::test]
async fn skills_refresh_if_idle_skips_when_request_in_flight() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.skills_request_in_flight = true;

    chat.request_skills_refresh_if_idle(true);

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(chat.skills_refresh_pending, None);
}

#[tokio::test]
async fn skills_refresh_if_idle_requests_when_idle() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;

    chat.request_skills_refresh_if_idle(true);

    match op_rx.try_recv() {
        Ok(Op::ListSkills { cwds, force_reload }) => {
            assert!(cwds.is_empty());
            assert!(force_reload);
        }
        other => panic!("expected skills refresh op, got {other:?}"),
    }
    assert!(chat.skills_request_in_flight);
    assert_eq!(chat.skills_refresh_pending, None);
}

#[tokio::test]
async fn skills_request_in_flight_accessor_tracks_refresh_state() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    assert!(!chat.skills_request_in_flight());

    chat.request_skills_refresh(true);
    assert!(chat.skills_request_in_flight());

    chat.set_skills_from_response(&adam_agent::protocol::ListSkillsResponseEvent {
        skills: vec![adam_agent::protocol::SkillsListEntry {
            cwd: chat.config.cwd.clone(),
            skills: Vec::new(),
            errors: Vec::new(),
        }],
    });
    assert!(!chat.skills_request_in_flight());
}

#[tokio::test]
async fn skills_response_sends_pending_refresh() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.skills_request_in_flight = true;

    chat.request_skills_refresh(true);
    chat.set_skills_from_response(&adam_agent::protocol::ListSkillsResponseEvent {
        skills: vec![adam_agent::protocol::SkillsListEntry {
            cwd: chat.config.cwd.clone(),
            skills: Vec::new(),
            errors: Vec::new(),
        }],
    });

    match op_rx.try_recv() {
        Ok(Op::ListSkills { cwds, force_reload }) => {
            assert!(cwds.is_empty());
            assert!(force_reload);
        }
        other => panic!("expected queued skills refresh, got {other:?}"),
    }
    assert!(chat.skills_request_in_flight);
    assert_eq!(chat.skills_refresh_pending, None);
}

#[tokio::test]
async fn skills_refresh_pending_preserves_force_reload() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.skills_request_in_flight = true;

    chat.request_skills_refresh(false);
    chat.request_skills_refresh(true);

    assert_eq!(chat.skills_refresh_pending, Some(true));
}

#[test]
fn skills_placeholder_copy_describes_management_flow() {
    assert!(PLACEHOLDERS.contains(&"Use /skills to manage skills"));
    assert!(!PLACEHOLDERS.contains(&"Use /skills to list available skills"));
}

#[tokio::test]
async fn agent_command_requests_centered_agent_selection_modal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Agent);

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenAgentPicker));
    assert!(!render_bottom_popup(&chat, 80).contains("Multi-agents"));
}

#[tokio::test]
async fn slash_init_skips_when_project_doc_exists() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    let tempdir = tempdir().unwrap();
    let existing_path = tempdir.path().join(DEFAULT_PROJECT_DOC_FILENAME);
    std::fs::write(&existing_path, "existing instructions").unwrap();
    chat.config.cwd = tempdir.path().to_path_buf();

    chat.dispatch_command(SlashCommand::Init);

    match op_rx.try_recv() {
        Err(TryRecvError::Empty) => {}
        other => panic!("expected no Adam op to be sent, got {other:?}"),
    }

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected one info message");
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains(DEFAULT_PROJECT_DOC_FILENAME),
        "info message should mention the existing file: {rendered:?}"
    );
    assert!(
        rendered.contains("Skipping /init"),
        "info message should explain why /init was skipped: {rendered:?}"
    );
    assert_eq!(
        std::fs::read_to_string(existing_path).unwrap(),
        "existing instructions"
    );
}

#[tokio::test]
async fn identity_slash_command_opens_identity_modal_event() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::Identities, true);

    chat.dispatch_command(SlashCommand::Identity);

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenIdentityModal));
}

#[tokio::test]
async fn shift_tab_opens_identity_modal_event_when_idle() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::Identities, true);

    chat.handle_key_event(KeyEvent::from(KeyCode::BackTab));

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenIdentityModal));
}

#[tokio::test]
async fn shift_tab_does_not_open_identity_modal_while_task_running() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::Identities, true);
    chat.bottom_pane.set_task_running(true);

    chat.handle_key_event(KeyEvent::from(KeyCode::BackTab));

    let cells = drain_insert_history(&mut rx);
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("Cannot change identity while a task is running."),
        "expected identity warning, got: {rendered}"
    );
    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn shift_tab_does_not_open_identity_modal_with_popup_active() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::Identities, true);
    chat.bottom_pane
        .set_composer_text("/".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::from(KeyCode::BackTab));

    let cells = drain_insert_history(&mut rx);
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("Cannot change identity while another picker is open."),
        "expected identity warning, got: {rendered}"
    );
    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn identity_mask_updates_identity_for_subsequent_messages() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_feature_enabled(Feature::Identities, true);

    chat.dispatch_command(SlashCommand::Identity);
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenIdentityModal));
    let selected_mask =
        match identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Programmer) {
            Some(mask) => mask,
            None => panic!("expected programmer identity preset"),
        };
    chat.set_identity_mask(selected_mask);

    chat.bottom_pane
        .set_composer_text("hello".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    match next_submit_op(&mut op_rx) {
        Op::UserTurn {
            identity:
                Some(Identity {
                    kind: IdentityKind::Programmer,
                    ..
                }),
            ..
        } => {}
        other => {
            panic!("expected Op::UserTurn with programmer identity, got {other:?}")
        }
    }

    chat.bottom_pane
        .set_composer_text("follow up".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    match next_submit_op(&mut op_rx) {
        Op::UserTurn {
            identity:
                Some(Identity {
                    kind: IdentityKind::Programmer,
                    ..
                }),
            ..
        } => {}
        other => {
            panic!("expected Op::UserTurn with programmer identity, got {other:?}")
        }
    }
}

#[tokio::test]
async fn identity_slash_command_disabled_during_task() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::Identities, true);
    chat.bottom_pane.set_task_running(true);

    chat.dispatch_command(SlashCommand::Identity);

    let cells = drain_insert_history(&mut rx);
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("'/identity' is disabled while a task is in progress."),
        "expected disabled message, got: {rendered}"
    );
}

#[tokio::test]
async fn identities_default_to_nobody_on_startup() {
    let adam_home = tempdir().expect("tempdir");
    let cfg = ConfigBuilder::default()
        .adam_home(adam_home.path().to_path_buf())
        .provider_config_required(false)
        .cli_overrides(vec![(
            "features.identities".to_string(),
            TomlValue::Boolean(true),
        )])
        .build()
        .await
        .expect("config");
    let resolved_model = ModelsManager::get_model_offline(cfg.model.as_deref());
    let otel_manager = test_otel_manager(&cfg, resolved_model.as_str());
    let thread_manager = Arc::new(ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        cfg.model_provider.clone(),
    ));
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let init = ChatWidgetInit {
        config: cfg,
        thread_manager,
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(unbounded_channel::<AppEvent>().0),
        initial_user_message: None,
        enhanced_keys_supported: false,
        auth_manager,
        feedback: adam_feedback::CodexFeedback::new(),
        is_first_run: true,
        startup: ChatWidgetStartup::Configured {
            model: Some(resolved_model.clone()),
        },
        otel_manager,
    };

    let chat = ChatWidget::new(init);
    assert_eq!(chat.active_identity_kind(), IdentityKind::Nobody);
    assert_eq!(chat.current_model(), resolved_model);
}

#[tokio::test]
async fn deferred_startup_does_not_configure_session() {
    let adam_home = tempdir().expect("tempdir");
    let cfg = ConfigBuilder::default()
        .adam_home(adam_home.path().to_path_buf())
        .provider_config_required(false)
        .build()
        .await
        .expect("config");
    let resolved_model = ModelsManager::get_model_offline(cfg.model.as_deref());
    let otel_manager = test_otel_manager(&cfg, resolved_model.as_str());
    let thread_manager = Arc::new(ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        cfg.model_provider.clone(),
    ));
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let (app_event_tx, mut app_event_rx) = unbounded_channel::<AppEvent>();
    let init = ChatWidgetInit {
        config: cfg,
        thread_manager,
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_user_message: None,
        enhanced_keys_supported: false,
        auth_manager,
        feedback: adam_feedback::CodexFeedback::new(),
        is_first_run: true,
        startup: ChatWidgetStartup::Deferred,
        otel_manager,
    };

    let chat = ChatWidget::new(init);

    assert_eq!(chat.thread_id(), None);
    assert!(!chat.is_session_configured());
    assert!(app_event_rx.try_recv().is_err());
}

#[tokio::test]
async fn last_selected_identity_plan_applies_on_startup() {
    let adam_home = tempdir().expect("tempdir");
    AdamStateStore::new(adam_home.path())
        .set_last_selected_identity(IdentityKind::Planner)
        .expect("persist identity");
    let cfg = ConfigBuilder::default()
        .adam_home(adam_home.path().to_path_buf())
        .provider_config_required(false)
        .cli_overrides(vec![(
            "features.identities".to_string(),
            TomlValue::Boolean(true),
        )])
        .build()
        .await
        .expect("config");
    let resolved_model = ModelsManager::get_model_offline(cfg.model.as_deref());
    let otel_manager = test_otel_manager(&cfg, resolved_model.as_str());
    let thread_manager = Arc::new(ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        cfg.model_provider.clone(),
    ));
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let init = ChatWidgetInit {
        config: cfg,
        thread_manager,
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(unbounded_channel::<AppEvent>().0),
        initial_user_message: None,
        enhanced_keys_supported: false,
        auth_manager,
        feedback: adam_feedback::CodexFeedback::new(),
        is_first_run: true,
        startup: ChatWidgetStartup::Configured {
            model: Some(resolved_model.clone()),
        },
        otel_manager,
    };

    let chat = ChatWidget::new(init);
    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(chat.current_model(), resolved_model);
}

#[tokio::test]
async fn last_selected_identity_plan_preserves_configured_effort_on_startup() {
    let adam_home = tempdir().expect("tempdir");
    let model_ref = ModelRef::new("openai", "main", "gpt-5");
    AdamStateStore::new(adam_home.path())
        .set_last_selected_model(&model_ref, Some(ReasoningEffortConfig::High), None)
        .expect("persist model effort");
    AdamStateStore::new(adam_home.path())
        .set_last_selected_identity(IdentityKind::Planner)
        .expect("persist identity");
    let cfg = ConfigBuilder::default()
        .adam_home(adam_home.path().to_path_buf())
        .provider_config_required(false)
        .cli_overrides(vec![(
            "features.identities".to_string(),
            TomlValue::Boolean(true),
        )])
        .build()
        .await
        .expect("config");
    let resolved_model = ModelsManager::get_model_offline(cfg.model.as_deref());
    let otel_manager = test_otel_manager(&cfg, resolved_model.as_str());
    let thread_manager = Arc::new(ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        cfg.model_provider.clone(),
    ));
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));
    let init = ChatWidgetInit {
        config: cfg,
        thread_manager,
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(unbounded_channel::<AppEvent>().0),
        initial_user_message: None,
        enhanced_keys_supported: false,
        auth_manager,
        feedback: adam_feedback::CodexFeedback::new(),
        is_first_run: true,
        startup: ChatWidgetStartup::Configured {
            model: Some(resolved_model.clone()),
        },
        otel_manager,
    };

    let chat = ChatWidget::new(init);
    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(chat.current_model(), resolved_model);
    assert_eq!(
        chat.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn set_model_updates_active_identity_mask() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.set_model("gpt-5.1-codex-mini");

    assert_eq!(chat.current_model(), "gpt-5.1-codex-mini");
    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
}

#[tokio::test]
async fn set_reasoning_effort_updates_active_identity_mask() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1")).await;
    chat.set_feature_enabled(Feature::Identities, true);
    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    chat.set_reasoning_effort(None);

    assert_eq!(chat.current_reasoning_effort(), None);
    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
}

#[tokio::test]
async fn code_effort_is_inherited_when_switching_to_plan() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.2-codex")).await;
    chat.set_feature_enabled(Feature::Identities, true);

    let code_mask =
        identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Programmer)
            .expect("expected programmer identity mask");
    chat.set_identity_mask(code_mask);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::Low));

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(
        chat.current_reasoning_effort(),
        Some(ReasoningEffortConfig::Low)
    );
}

#[tokio::test]
async fn plan_effort_override_survives_mode_switch() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1")).await;
    chat.set_feature_enabled(Feature::Identities, true);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let code_mask =
        identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Programmer)
            .expect("expected programmer identity mask");
    chat.set_identity_mask(code_mask);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(
        chat.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn plan_effort_override_is_restored_for_supported_model() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.2-codex")).await;
    chat.set_feature_enabled(Feature::Identities, true);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);
    chat.set_model("gpt-5.2-codex");
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::Low));

    let code_mask =
        identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Programmer)
            .expect("expected programmer identity mask");
    chat.set_identity_mask(code_mask);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(chat.current_model(), "gpt-5.2-codex");
    assert_eq!(
        chat.current_reasoning_effort(),
        Some(ReasoningEffortConfig::Low)
    );
}

#[tokio::test]
async fn plan_inherited_code_effort_is_preserved_for_unknown_model() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("glm-5.1")).await;
    chat.set_feature_enabled(Feature::Identities, true);

    let code_mask =
        identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Programmer)
            .expect("expected programmer identity mask");
    chat.set_identity_mask(code_mask);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(chat.current_model(), "glm-5.1");
    assert_eq!(
        chat.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn plan_effort_override_is_restored_for_unknown_model() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("glm-5.1")).await;
    chat.set_feature_enabled(Feature::Identities, true);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let code_mask =
        identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Programmer)
            .expect("expected programmer identity mask");
    chat.set_identity_mask(code_mask);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(chat.current_model(), "glm-5.1");
    assert_eq!(
        chat.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn plan_explicit_default_effort_survives_mode_switch() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1")).await;
    chat.set_feature_enabled(Feature::Identities, true);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);
    chat.set_reasoning_effort(None);

    let code_mask =
        identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Programmer)
            .expect("expected programmer identity mask");
    chat.set_identity_mask(code_mask);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(chat.current_reasoning_effort(), None);
}

#[tokio::test]
async fn plan_without_user_override_keeps_current_effort() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1")).await;
    chat.set_feature_enabled(Feature::Identities, true);

    let plan_mask = identities::mask_for_kind(chat.thread_manager.as_ref(), IdentityKind::Planner)
        .expect("expected planner identity mask");
    chat.set_identity_mask(plan_mask);

    assert_eq!(chat.active_identity_kind(), IdentityKind::Planner);
    assert_eq!(chat.current_reasoning_effort(), None);
}

#[tokio::test]
async fn collab_mode_is_not_sent_until_selected() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_feature_enabled(Feature::Identities, true);

    chat.bottom_pane
        .set_composer_text("hello".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    match next_submit_op(&mut op_rx) {
        Op::UserTurn { identity, .. } => {
            assert_eq!(identity, None);
        }
        other => {
            panic!("expected Op::UserTurn, got {other:?}")
        }
    }
}

#[tokio::test]
async fn collab_mode_enabling_keeps_custom_until_selected() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_feature_enabled(Feature::Identities, true);
    assert_eq!(chat.active_identity_kind(), IdentityKind::Nobody);
    assert_eq!(chat.current_identity().kind, IdentityKind::Nobody);
}

#[tokio::test]
async fn user_turn_includes_personality_from_config() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(Some("bengalfox")).await;
    chat.set_feature_enabled(Feature::Personality, true);
    chat.thread_id = Some(ThreadId::new());
    chat.set_model("bengalfox");
    chat.set_personality(Personality::Friendly);

    chat.bottom_pane
        .set_composer_text("hello".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    match next_submit_op(&mut op_rx) {
        Op::UserTurn {
            personality: Some(Personality::Friendly),
            ..
        } => {}
        other => panic!("expected Op::UserTurn with friendly personality, got {other:?}"),
    }
}

#[tokio::test]
async fn user_turn_includes_active_buddy_snapshot() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_buddy_config(TuiBuddy {
        enabled: true,
        muted: false,
        observer: BuddyObserverConfig {
            enabled: true,
            model: Some("buddy-model".to_string()),
            max_reaction_chars: 42,
            ..BuddyObserverConfig::default()
        },
        ..TuiBuddy::default()
    });

    chat.bottom_pane
        .set_composer_text("hello".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match next_submit_op(&mut op_rx) {
        Op::UserTurn {
            tui_buddy: Some(buddy),
            ..
        } => {
            assert!(buddy.enabled);
            assert!(!buddy.muted);
            assert_eq!(buddy.observer_enabled, true);
            assert_eq!(buddy.observer_model, Some("buddy-model".to_string()));
            assert_eq!(buddy.observer_max_reaction_chars, 42);
            assert!(buddy.name.is_some());
            assert!(buddy.species.is_some());
            assert!(buddy.personality.is_some());
        }
        other => panic!("expected Op::UserTurn with buddy snapshot, got {other:?}"),
    }
}

#[tokio::test]
async fn slash_quit_requests_exit() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Quit);

    assert_matches!(rx.try_recv(), Ok(AppEvent::Exit(ExitMode::Immediate)));
}

#[tokio::test]
async fn slash_exit_requests_exit() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Exit);

    assert_matches!(rx.try_recv(), Ok(AppEvent::Exit(ExitMode::Immediate)));
}

#[tokio::test]
async fn slash_resume_opens_picker() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Resume);

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenResumePicker));
}

#[tokio::test]
async fn slash_fork_requests_current_fork() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Fork);

    assert_matches!(rx.try_recv(), Ok(AppEvent::ForkCurrentSession));
}

#[tokio::test]
async fn slash_stop_submits_background_terminal_cleanup() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Stop);

    assert_matches!(op_rx.try_recv(), Ok(Op::CleanBackgroundTerminals));
    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected cleanup confirmation message");
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("Stopping all background terminals."),
        "expected cleanup confirmation, got {rendered:?}"
    );
}

#[tokio::test]
async fn slash_rollout_displays_current_path() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let rollout_path = PathBuf::from("/tmp/codex-test-rollout.jsonl");
    chat.current_rollout_path = Some(rollout_path.clone());

    chat.dispatch_command(SlashCommand::Rollout);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected info message for rollout path");
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains(&rollout_path.display().to_string()),
        "expected rollout path to be shown: {rendered}"
    );
}

#[tokio::test]
async fn slash_rollout_handles_missing_path() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Rollout);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(
        cells.len(),
        1,
        "expected info message explaining missing path"
    );
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("not available"),
        "expected missing rollout path message: {rendered}"
    );
}

#[tokio::test]
async fn undo_success_events_render_info_messages() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "turn-1".to_string(),
        msg: EventMsg::UndoStarted(UndoStartedEvent {
            message: Some("Undo requested for the last turn...".to_string()),
        }),
    });
    assert!(
        chat.bottom_pane.status_indicator_visible(),
        "status indicator should be visible during undo"
    );

    chat.handle_codex_event(Event {
        id: "turn-1".to_string(),
        msg: EventMsg::UndoCompleted(UndoCompletedEvent {
            success: true,
            message: None,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected final status only");
    assert!(
        !chat.bottom_pane.status_indicator_visible(),
        "status indicator should be hidden after successful undo"
    );

    let completed = lines_to_single_string(&cells[0]);
    assert!(
        completed.contains("Undo completed successfully."),
        "expected default success message, got {completed:?}"
    );
}

#[tokio::test]
async fn undo_failure_events_render_error_message() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "turn-2".to_string(),
        msg: EventMsg::UndoStarted(UndoStartedEvent { message: None }),
    });
    assert!(
        chat.bottom_pane.status_indicator_visible(),
        "status indicator should be visible during undo"
    );

    chat.handle_codex_event(Event {
        id: "turn-2".to_string(),
        msg: EventMsg::UndoCompleted(UndoCompletedEvent {
            success: false,
            message: Some("Failed to restore workspace state.".to_string()),
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected final status only");
    assert!(
        !chat.bottom_pane.status_indicator_visible(),
        "status indicator should be hidden after failed undo"
    );

    let completed = lines_to_single_string(&cells[0]);
    assert!(
        completed.contains("Failed to restore workspace state."),
        "expected failure message, got {completed:?}"
    );
}

#[tokio::test]
async fn undo_started_hides_interrupt_hint() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "turn-hint".to_string(),
        msg: EventMsg::UndoStarted(UndoStartedEvent { message: None }),
    });

    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be active");
    assert!(
        !status.interrupt_hint_visible(),
        "undo should hide the interrupt hint because the operation cannot be cancelled"
    );
}

#[tokio::test]
async fn view_image_tool_call_adds_history_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let image_path = chat.config.cwd.join("example.png");

    chat.handle_codex_event(Event {
        id: "sub-image".into(),
        msg: EventMsg::ViewImageToolCall(ViewImageToolCallEvent {
            call_id: "call-image".into(),
            path: image_path,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected a single history cell");
    let combined = lines_to_single_string(&cells[0]);
    assert_snapshot!("local_image_attachment_history_snapshot", combined);
}

// Snapshot test: interrupting a running exec finalizes the active cell with a red ✗
// marker (replacing the spinner) and flushes it into history.
#[tokio::test]
async fn interrupt_exec_marks_failed_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Begin a long-running command so we have an active exec cell with a spinner.
    begin_exec(&mut chat, "call-int", "sleep 1");

    // Simulate the task being aborted (as if ESC was pressed), which should
    // cause the active exec cell to be finalized as failed and flushed.
    chat.handle_codex_event(Event {
        id: "call-int".into(),
        msg: EventMsg::TurnAborted(adam_agent::protocol::TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert!(
        !cells.is_empty(),
        "expected finalized exec cell to be inserted into history"
    );

    // The first inserted cell should be the finalized exec; snapshot its text.
    let exec_blob = lines_to_single_string(&cells[0]);
    assert_snapshot!("interrupt_exec_marks_failed", exec_blob);
}

// Snapshot test: after an interrupted turn, a gentle error message is inserted
// suggesting the user to tell the model what to do differently and to use /feedback.
#[tokio::test]
async fn interrupted_turn_error_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Simulate an in-progress task so the widget is in a running state.
    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });

    // Abort the turn (like pressing Esc) and drain inserted history.
    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::TurnAborted(adam_agent::protocol::TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert!(
        !cells.is_empty(),
        "expected error message to be inserted after interruption"
    );
    let last = lines_to_single_string(cells.last().unwrap());
    assert_snapshot!("interrupted_turn_error_message", last);
}

fn render_bottom_popup(chat: &ChatWidget, width: u16) -> String {
    let height = chat.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    let mut lines: Vec<String> = (0..area.height)
        .map(|row| {
            let mut line = String::new();
            for col in 0..area.width {
                let symbol = buf[(area.x + col, area.y + row)].symbol();
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(symbol);
                }
            }
            line.trim_end().to_string()
        })
        .collect();

    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

#[tokio::test]
async fn model_command_requests_centered_model_selection_modal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5-codex")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.open_model_popup();

    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AppEvent::OpenModelSelectionModal { presets } if !presets.is_empty()
        )),
        "expected /model to request centered model selection modal: {events:?}"
    );
    assert!(
        !render_bottom_popup(&chat, 80).contains("Select Model"),
        "model picker should be owned by App modal, not the bottom pane"
    );
}

#[tokio::test]
async fn personality_command_requests_centered_personality_selection_modal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("bengalfox")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.open_personality_popup();

    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AppEvent::OpenPersonalitySelectionModal {
                current_personality: Personality::Friendly,
            }
        )),
        "expected /personality to request centered personality selection modal: {events:?}"
    );
    assert!(
        !render_bottom_popup(&chat, 80).contains("Select Personality"),
        "personality picker should be owned by App modal, not the bottom pane"
    );
}

#[tokio::test]
async fn model_picker_hides_show_in_picker_false_models_from_cache() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("test-visible-model")).await;
    chat.thread_id = Some(ThreadId::new());
    let preset = |slug: &str, show_in_picker: bool| ModelPreset {
        id: slug.to_string(),
        model: slug.to_string(),
        model_provider_id: None,
        display_name: slug.to_string(),
        description: format!("{slug} description"),
        default_reasoning_effort: ReasoningEffortConfig::Medium,
        supported_reasoning_efforts: vec![ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Medium,
            description: "medium".to_string(),
        }],
        supports_personality: false,
        is_default: false,
        upgrade: None,
        show_in_picker,
        supported_in_api: true,
    };

    chat.open_model_popup_with_presets(vec![
        preset("test-visible-model", true),
        preset("test-hidden-model", false),
    ]);
    let popup = render_bottom_popup(&chat, 80);
    assert_snapshot!("model_picker_filters_hidden_models", popup);
    assert!(
        popup.contains("test-visible-model"),
        "expected visible model to appear in picker:\n{popup}"
    );
    assert!(
        !popup.contains("test-hidden-model"),
        "expected hidden model to be excluded from picker:\n{popup}"
    );
}

#[tokio::test]
async fn model_picker_without_auth_shows_only_configured_custom_model() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("mock-model")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.auth_manager = Arc::new(AuthManager::new(
        chat.config.adam_home.clone(),
        false,
        AuthCredentialsStoreMode::File,
    ));
    chat.thread_manager = Arc::new(ThreadManager::new(
        chat.config.adam_home.clone(),
        chat.auth_manager.clone(),
        chat.config.model_provider_id.as_str(),
        chat.config.model_provider.clone(),
        SessionSource::Cli,
    ));
    reload_chat_config_with_saved_providers(
        &mut chat,
        vec![CustomProviderConfig {
            provider_id: "mock_provider".to_string(),
            dialect: crate::provider_config::ApiProviderDialect::Responses,
            base_url: "https://example.test/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "mock-model".to_string(),
            model_context_window: None,
        }],
    )
    .await;

    chat.open_model_popup();

    let events = drain_events(&mut rx);
    let presets = events
        .iter()
        .find_map(|event| match event {
            AppEvent::OpenModelSelectionModal { presets } => Some(presets),
            _ => None,
        })
        .expect("model selection event");
    assert!(
        presets.iter().any(|preset| preset.model == "mock-model"),
        "expected configured custom model to appear in picker: {presets:?}"
    );
    assert!(
        !presets.iter().any(|preset| preset.model == "gpt-5.2-codex"),
        "expected built-in picker models to be hidden without auth: {presets:?}"
    );
}

#[tokio::test]
async fn model_picker_without_auth_shows_all_models_saved_in_config_toml() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("mock-model")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.auth_manager = Arc::new(AuthManager::new(
        chat.config.adam_home.clone(),
        false,
        AuthCredentialsStoreMode::File,
    ));
    chat.thread_manager = Arc::new(ThreadManager::new(
        chat.config.adam_home.clone(),
        chat.auth_manager.clone(),
        chat.config.model_provider_id.as_str(),
        chat.config.model_provider.clone(),
        SessionSource::Cli,
    ));
    reload_chat_config_with_saved_providers(
        &mut chat,
        vec![
            CustomProviderConfig {
                provider_id: "mock_provider".to_string(),
                dialect: crate::provider_config::ApiProviderDialect::Responses,
                base_url: "https://example.test/v1".to_string(),
                api_key: "sk-test".to_string(),
                model: "mock-model".to_string(),
                model_context_window: None,
            },
            CustomProviderConfig {
                provider_id: "mock_provider".to_string(),
                dialect: crate::provider_config::ApiProviderDialect::Responses,
                base_url: "https://example.test/v1".to_string(),
                api_key: "sk-test".to_string(),
                model: "deepseek-r1".to_string(),
                model_context_window: None,
            },
            CustomProviderConfig {
                provider_id: "other_provider".to_string(),
                dialect: crate::provider_config::ApiProviderDialect::Responses,
                base_url: "https://example.test/v1".to_string(),
                api_key: "sk-test".to_string(),
                model: "claude-sonnet".to_string(),
                model_context_window: None,
            },
        ],
    )
    .await;
    chat.set_model("mock-model");

    chat.open_model_popup();

    let events = drain_events(&mut rx);
    let presets = events
        .iter()
        .find_map(|event| match event {
            AppEvent::OpenModelSelectionModal { presets } => Some(presets),
            _ => None,
        })
        .expect("model selection event");
    assert!(
        presets.iter().any(|preset| preset.model == "mock-model"),
        "expected configured custom model to appear in picker: {presets:?}"
    );
    assert!(
        presets.iter().any(|preset| preset.model == "deepseek-r1"),
        "expected saved model to appear in picker: {presets:?}"
    );
    assert!(
        presets.iter().any(|preset| preset.model == "claude-sonnet"),
        "expected models from other providers in config.toml to appear in picker: {presets:?}"
    );
    assert!(
        !presets.iter().any(|preset| preset.model == "gpt-5.2-codex"),
        "expected built-in picker models to be hidden without auth: {presets:?}"
    );
}

#[tokio::test]
async fn model_picker_without_auth_shows_same_model_for_different_custom_providers() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("glm-5")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.auth_manager = Arc::new(AuthManager::new(
        chat.config.adam_home.clone(),
        false,
        AuthCredentialsStoreMode::File,
    ));
    chat.thread_manager = Arc::new(ThreadManager::new(
        chat.config.adam_home.clone(),
        chat.auth_manager.clone(),
        chat.config.model_provider_id.as_str(),
        chat.config.model_provider.clone(),
        SessionSource::Cli,
    ));
    reload_chat_config_with_saved_providers(
        &mut chat,
        vec![
            CustomProviderConfig {
                provider_id: "provider_a".to_string(),
                dialect: crate::provider_config::ApiProviderDialect::Responses,
                base_url: "https://example.test/a".to_string(),
                api_key: "sk-test-a".to_string(),
                model: "glm-5".to_string(),
                model_context_window: None,
            },
            CustomProviderConfig {
                provider_id: "provider_b".to_string(),
                dialect: crate::provider_config::ApiProviderDialect::Responses,
                base_url: "https://example.test/b".to_string(),
                api_key: "sk-test-b".to_string(),
                model: "glm-5".to_string(),
                model_context_window: None,
            },
        ],
    )
    .await;
    chat.set_model("glm-5");

    chat.open_model_popup();

    let events = drain_events(&mut rx);
    let presets = events
        .iter()
        .find_map(|event| match event {
            AppEvent::OpenModelSelectionModal { presets } => Some(presets),
            _ => None,
        })
        .expect("model selection event");
    assert_eq!(
        presets
            .iter()
            .filter(|preset| preset.model == "glm-5")
            .count(),
        2,
        "expected two glm-5 entries: {presets:?}"
    );
    assert!(
        presets.iter().any(|preset| preset.description
            == "User-defined model from provider_a (responses) provider."),
        "expected provider_a description in picker: {presets:?}"
    );
    assert!(
        presets.iter().any(|preset| preset.description
            == "User-defined model from provider_b (responses) provider."),
        "expected provider_b description in picker: {presets:?}"
    );
}
#[tokio::test]
async fn model_switcher_prefers_exact_provider_for_current_marker() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.set_model("gpt-5.4");
    chat.config.model_provider_id = "provider_a".to_string();

    chat.open_all_models_popup(vec![
        ModelPreset {
            id: "gpt-5.4".to_string(),
            model: "gpt-5.4".to_string(),
            model_provider_id: None,
            display_name: "gpt-5.4".to_string(),
            description: "Configured model from config.toml.".to_string(),
            default_reasoning_effort: ReasoningEffortConfig::Medium,
            supported_reasoning_efforts: Vec::new(),
            supports_personality: false,
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
        },
        ModelPreset {
            id: "provider_a/gpt-5.4".to_string(),
            model: "gpt-5.4".to_string(),
            model_provider_id: Some("provider_a".to_string()),
            display_name: "gpt-5.4".to_string(),
            description: "User-defined model from provider_a provider.".to_string(),
            default_reasoning_effort: ReasoningEffortConfig::Medium,
            supported_reasoning_efforts: Vec::new(),
            supports_personality: false,
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
        },
    ]);

    let popup = render_bottom_popup(&chat, 120);
    assert_eq!(
        popup.matches("(current)").count(),
        1,
        "expected only one current entry:\n{popup}"
    );
    assert!(
        popup.lines().any(|line| {
            line.contains("gpt-5.4 (current)")
                && line.contains("User-defined model from provider_a provider.")
        }),
        "expected exact provider entry to be current:\n{popup}"
    );
    assert!(
        popup.lines().any(|line| {
            line.contains("gpt-5.4")
                && line.contains("Configured model from config.toml.")
                && !line.contains("(current)")
        }),
        "expected generic entry to remain non-current:\n{popup}"
    );
}

#[tokio::test]
async fn model_cap_error_does_not_switch_models() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(Some("boomslang")).await;
    chat.set_model("boomslang");
    while rx.try_recv().is_ok() {}
    while op_rx.try_recv().is_ok() {}

    chat.handle_codex_event(Event {
        id: "err-1".to_string(),
        msg: EventMsg::Error(ErrorEvent {
            message: "model cap".to_string(),
            codex_error_info: Some(CodexErrorInfo::ModelCap {
                model: "boomslang".to_string(),
                reset_after_seconds: Some(120),
            }),
        }),
    });

    while rx.try_recv().is_ok() {}

    while let Ok(event) = op_rx.try_recv() {
        if let Op::OverrideTurnContext { model, .. } = event {
            assert!(
                model.is_none(),
                "did not expect OverrideTurnContext model update on model-cap error"
            );
        }
    }
}

#[tokio::test]
async fn approvals_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.config.notices.hide_full_access_warning = None;
    chat.open_approvals_popup();

    let popup = render_bottom_popup(&chat, 80);
    #[cfg(target_os = "windows")]
    insta::with_settings!({ snapshot_suffix => "windows" }, {
        assert_snapshot!("approvals_selection_popup", popup);
    });
    #[cfg(not(target_os = "windows"))]
    assert_snapshot!("approvals_selection_popup", popup);
}

#[cfg(target_os = "windows")]
#[tokio::test]
#[serial]
async fn approvals_selection_popup_snapshot_windows_degraded_sandbox() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.config.notices.hide_full_access_warning = None;
    chat.set_feature_enabled(Feature::WindowsSandbox, true);
    chat.set_feature_enabled(Feature::WindowsSandboxElevated, false);

    chat.open_approvals_popup();

    let popup = render_bottom_popup(&chat, 80);
    insta::with_settings!({ snapshot_suffix => "windows_degraded" }, {
        assert_snapshot!("approvals_selection_popup", popup);
    });
}

#[tokio::test]
async fn preset_matching_ignores_extra_writable_roots() {
    let preset = builtin_approval_presets()
        .into_iter()
        .find(|p| p.id == "auto")
        .expect("auto preset exists");
    let current_sandbox = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![AbsolutePathBuf::try_from("C:\\extra").unwrap()],
        network_access: false,
        exclude_tmpdir_env_var: false,
        exclude_slash_tmp: false,
    };

    assert!(
        ChatWidget::preset_matches_current(AskForApproval::OnRequest, &current_sandbox, &preset),
        "WorkspaceWrite with extra roots should still match the Agent preset"
    );
    assert!(
        !ChatWidget::preset_matches_current(AskForApproval::Never, &current_sandbox, &preset),
        "approval mismatch should prevent matching the preset"
    );
}

#[tokio::test]
async fn full_access_confirmation_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    let preset = builtin_approval_presets()
        .into_iter()
        .find(|preset| preset.id == "full-access")
        .expect("full access preset");
    chat.open_full_access_confirmation(preset, false);

    let popup = render_bottom_popup(&chat, 80);
    assert_snapshot!("full_access_confirmation_popup", popup);
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn windows_auto_mode_prompt_requests_enabling_sandbox_feature() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    let preset = builtin_approval_presets()
        .into_iter()
        .find(|preset| preset.id == "auto")
        .expect("auto preset");
    chat.open_windows_sandbox_enable_prompt(preset);

    let popup = render_bottom_popup(&chat, 120);
    assert!(
        popup.contains("requires elevation"),
        "expected auto mode prompt to mention elevation, popup: {popup}"
    );
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn startup_prompts_for_windows_sandbox_when_agent_requested() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.set_feature_enabled(Feature::WindowsSandbox, false);
    chat.set_feature_enabled(Feature::WindowsSandboxElevated, false);
    chat.config.forced_auto_mode_downgraded_on_windows = true;

    chat.maybe_prompt_windows_sandbox_enable();

    let popup = render_bottom_popup(&chat, 120);
    assert!(
        popup.contains("requires elevation"),
        "expected startup prompt to explain elevation: {popup}"
    );
    assert!(
        popup.contains("Set up agent sandbox"),
        "expected startup prompt to offer agent sandbox setup: {popup}"
    );
    assert!(
        popup.contains("Stay in"),
        "expected startup prompt to offer staying in current kind: {popup}"
    );
}

#[tokio::test]
async fn model_reasoning_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1-codex-max")).await;

    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let preset = get_available_model(&chat, "gpt-5.1-codex-max");
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, 80);
    assert_snapshot!("model_reasoning_selection_popup", popup);
}

#[tokio::test]
async fn model_reasoning_selection_popup_extra_high_warning_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1-codex-max")).await;

    chat.set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));

    let preset = get_available_model(&chat, "gpt-5.1-codex-max");
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, 80);
    assert_snapshot!("model_reasoning_selection_popup_extra_high_warning", popup);
}

#[tokio::test]
async fn reasoning_popup_shows_extra_high_with_space() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1-codex-max")).await;

    let preset = get_available_model(&chat, "gpt-5.1-codex-max");
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, 120);
    assert!(
        popup.contains("Extra high"),
        "expected popup to include 'Extra high'; popup: {popup}"
    );
    assert!(
        !popup.contains("Extrahigh"),
        "expected popup not to include 'Extrahigh'; popup: {popup}"
    );
}

#[tokio::test]
async fn single_reasoning_option_skips_selection() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let single_effort = vec![ReasoningEffortPreset {
        effort: ReasoningEffortConfig::High,
        description: "Greater reasoning depth for complex or ambiguous problems".to_string(),
    }];
    let preset = ModelPreset {
        id: "model-with-single-reasoning".to_string(),
        model: "model-with-single-reasoning".to_string(),
        model_provider_id: None,
        display_name: "model-with-single-reasoning".to_string(),
        description: "".to_string(),
        default_reasoning_effort: ReasoningEffortConfig::High,
        supported_reasoning_efforts: single_effort,
        supports_personality: false,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        supported_in_api: true,
    };
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, 80);
    assert!(
        !popup.contains("Select Reasoning Level"),
        "expected reasoning selection popup to be skipped"
    );

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    assert!(
        events.iter().any(|ev| matches!(
            ev,
            AppEvent::PersistModelSelection {
                model,
                provider_id: None,
                effort,
            }
                if model == "model-with-single-reasoning"
                    && *effort == Some(ReasoningEffortConfig::High)
        )),
        "expected single reasoning option to persist model selection automatically; events: {events:?}"
    );
}

#[tokio::test]
async fn no_reasoning_options_persist_none_effort() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let preset = ModelPreset {
        id: "custom-model".to_string(),
        model: "custom-model".to_string(),
        model_provider_id: None,
        display_name: "custom-model".to_string(),
        description: "Configured model from config.toml.".to_string(),
        default_reasoning_effort: ReasoningEffortConfig::None,
        supported_reasoning_efforts: Vec::new(),
        supports_personality: false,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        supported_in_api: true,
    };
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, 80);
    assert!(
        !popup.contains("Select Reasoning Level"),
        "expected reasoning selection popup to be skipped"
    );

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    assert!(
        events.iter().any(|ev| matches!(
            ev,
            AppEvent::PersistModelSelection {
                model,
                provider_id: None,
                effort,
            }
                if model == "custom-model" && effort.is_none()
        )),
        "expected no reasoning options to persist model selection with no effort; events: {events:?}"
    );
}

#[tokio::test]
async fn no_reasoning_options_preserve_provider_identity() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let preset = ModelPreset {
        id: "gpt-5.2".to_string(),
        model: "gpt-5.2".to_string(),
        model_provider_id: Some("openai".to_string()),
        display_name: "gpt-5.2".to_string(),
        description:
            "Latest frontier model with improvements across knowledge, reasoning and coding"
                .to_string(),
        default_reasoning_effort: ReasoningEffortConfig::None,
        supported_reasoning_efforts: Vec::new(),
        supports_personality: false,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        supported_in_api: true,
    };
    chat.open_reasoning_popup(preset);

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    assert!(
        events.iter().any(|ev| matches!(
            ev,
            AppEvent::PersistModelSelection {
                model,
                provider_id: Some(provider_id),
                effort,
            } if model == "gpt-5.2" && provider_id == "openai" && effort.is_none()
        )),
        "expected provider-aware selection to preserve openai provider: {events:?}"
    );
}

#[tokio::test]
async fn model_picker_with_no_reasoning_options_dismisses_after_selection() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let preset = ModelPreset {
        id: "custom-model".to_string(),
        model: "custom-model".to_string(),
        model_provider_id: None,
        display_name: "custom-model".to_string(),
        description: "Configured model from config.toml.".to_string(),
        default_reasoning_effort: ReasoningEffortConfig::Medium,
        supported_reasoning_efforts: Vec::new(),
        supports_personality: false,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        supported_in_api: true,
    };
    chat.open_all_models_popup(vec![preset]);

    let before = render_bottom_popup(&chat, 80);
    assert!(
        before.contains("Select Model and Effort"),
        "expected model picker to be open; popup: {before}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let after = render_bottom_popup(&chat, 80);
    assert!(
        !after.contains("Select Model and Effort"),
        "expected model picker to dismiss after selecting a model with no reasoning options; popup: {after}"
    );

    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, AppEvent::OpenReasoningPopup { model } if model.model == "custom-model")),
        "expected custom model selection to continue through the reasoning handler; events: {events:?}"
    );
}

#[tokio::test]
async fn feedback_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    // Open the feedback category selection popup via slash command.
    chat.dispatch_command(SlashCommand::Feedback);

    let popup = render_bottom_popup(&chat, 80);
    assert_snapshot!("feedback_selection_popup", popup);
}

#[tokio::test]
async fn feedback_selection_popup_allows_transcript_mouse_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    chat.dispatch_command(SlashCommand::Feedback);

    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let transcript_area = chat
        .cached_transcript_area()
        .expect("transcript area cached");

    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: transcript_area.x,
        row: transcript_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: transcript_area.x.saturating_add(5),
        row: transcript_area.y,
        modifiers: KeyModifiers::NONE,
    });

    assert!(chat.transcript.borrow().selection_active_for_test());
}

#[tokio::test]
async fn feedback_selection_popup_ignores_mouse_selection_in_bottom_pane() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    chat.dispatch_command(SlashCommand::Feedback);

    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let bottom_area = chat.cached_bottom_area().expect("bottom area cached");

    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: bottom_area.x,
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: bottom_area.x.saturating_add(5),
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });

    assert!(!chat.transcript.borrow().selection_active_for_test());
}

#[tokio::test]
async fn feedback_upload_consent_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    // Open the consent popup directly for a chosen category.
    chat.open_feedback_consent(crate::app_event::FeedbackCategory::Bug);

    let popup = render_bottom_popup(&chat, 80);
    assert_snapshot!("feedback_upload_consent_popup", popup);
}

#[tokio::test]
async fn feedback_upload_consent_popup_allows_transcript_page_scroll() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.open_feedback_consent(crate::app_event::FeedbackCategory::Bug);
    chat.handle_key_event(KeyEvent::from(KeyCode::PageUp));

    assert!(chat.transcript_scroll_offset() < at_tail);
}

#[tokio::test]
async fn feedback_upload_consent_popup_keeps_arrow_keys_for_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let at_tail = chat.transcript_scroll_offset();

    chat.open_feedback_consent(crate::app_event::FeedbackCategory::Bug);
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));

    assert_eq!(chat.transcript_scroll_offset(), at_tail);
    let popup = render_bottom_popup(&chat, 80);
    assert!(popup.contains("› 2. No"));
}

#[tokio::test]
async fn feedback_note_view_allows_transcript_mouse_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    chat.open_feedback_note(crate::app_event::FeedbackCategory::Bug, true);

    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let transcript_area = chat
        .cached_transcript_area()
        .expect("transcript area cached");

    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: transcript_area.x,
        row: transcript_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: transcript_area.x.saturating_add(5),
        row: transcript_area.y,
        modifiers: KeyModifiers::NONE,
    });

    assert!(chat.transcript.borrow().selection_active_for_test());
}

#[tokio::test]
async fn feedback_note_view_ignores_mouse_selection_in_bottom_pane() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.replace_transcript_cells(vec![Arc::new(TallTranscriptCell(40))]);
    chat.open_feedback_note(crate::app_event::FeedbackCategory::Bug, true);

    let area = Rect::new(0, 0, 80, 18);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);
    let bottom_area = chat.cached_bottom_area().expect("bottom area cached");

    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: bottom_area.x,
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });
    chat.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: bottom_area.x.saturating_add(5),
        row: bottom_area.y,
        modifiers: KeyModifiers::NONE,
    });

    assert!(!chat.transcript.borrow().selection_active_for_test());
}

#[tokio::test]
async fn reasoning_popup_escape_dismisses_popup() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.1-codex-max")).await;
    chat.thread_id = Some(ThreadId::new());

    let preset = get_available_model(&chat, "gpt-5.1-codex-max");
    chat.open_reasoning_popup(preset);

    let before_escape = render_bottom_popup(&chat, 80);
    assert!(before_escape.contains("Select Reasoning Level"));

    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    let after_escape = render_bottom_popup(&chat, 80);
    assert!(!after_escape.contains("Select Reasoning Level"));
}

#[tokio::test]
async fn exec_history_extends_previous_when_consecutive() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    // 1) Start "ls -la" (List)
    let begin_ls = begin_exec(&mut chat, "call-ls", "ls -la");
    assert_snapshot!("exploring_step1_start_ls", active_blob(&chat));

    // 2) Finish "ls -la"
    end_exec(&mut chat, begin_ls, "", "", 0);
    assert_snapshot!("exploring_step2_finish_ls", active_blob(&chat));

    // 3) Start "cat foo.txt" (Read)
    let begin_cat_foo = begin_exec(&mut chat, "call-cat-foo", "cat foo.txt");
    assert_snapshot!("exploring_step3_start_cat_foo", active_blob(&chat));

    // 4) Complete "cat foo.txt"
    end_exec(&mut chat, begin_cat_foo, "hello from foo", "", 0);
    assert_snapshot!("exploring_step4_finish_cat_foo", active_blob(&chat));

    // 5) Start & complete "sed -n 100,200p foo.txt" (treated as Read of foo.txt)
    let begin_sed_range = begin_exec(&mut chat, "call-sed-range", "sed -n 100,200p foo.txt");
    end_exec(&mut chat, begin_sed_range, "chunk", "", 0);
    assert_snapshot!("exploring_step5_finish_sed_range", active_blob(&chat));

    // 6) Start & complete "cat bar.txt"
    let begin_cat_bar = begin_exec(&mut chat, "call-cat-bar", "cat bar.txt");
    end_exec(&mut chat, begin_cat_bar, "hello from bar", "", 0);
    assert_snapshot!("exploring_step6_finish_cat_bar", active_blob(&chat));
}

#[tokio::test]
async fn exec_output_delta_keeps_exploring_cell_active() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    let begin_ls = begin_exec(&mut chat, "call-ls", "ls -la");
    output_delta(&mut chat, "call-ls", "file1\n");

    let active = active_blob(&chat);
    assert!(
        active.contains("• Exploring"),
        "expected output delta to keep exploring active: {active:?}"
    );
    assert!(
        !active.contains("• Explored"),
        "output delta should not mark exploring as completed: {active:?}"
    );

    end_exec(&mut chat, begin_ls, "file1\n", "", 0);
    assert!(
        active_blob(&chat).contains("• Exploring"),
        "exec end should keep the exploring label"
    );
    assert!(
        !active_blob(&chat).contains("• Explored"),
        "exploring cells should not render the completed label"
    );
}

#[tokio::test]
async fn user_shell_command_renders_output_not_exploring() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let begin_ls = begin_exec_with_source(
        &mut chat,
        "user-shell-ls",
        "ls",
        ExecCommandSource::UserShell,
    );
    end_exec(&mut chat, begin_ls, "file1\nfile2\n", "", 0);

    let cells = drain_insert_history(&mut rx);
    assert_eq!(
        cells.len(),
        1,
        "expected a single history cell for the user command"
    );
    let blob = lines_to_single_string(cells.first().unwrap());
    assert_snapshot!("user_shell_ls_output", blob);
}

#[tokio::test]
async fn disabled_slash_command_while_task_running_snapshot() {
    // Build a chat widget and simulate an active task
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.bottom_pane.set_task_running(true);

    // Dispatch a command that is unavailable while a task runs (e.g., /model)
    chat.dispatch_command(SlashCommand::Model);

    // Drain history and snapshot the rendered error line(s)
    let cells = drain_insert_history(&mut rx);
    assert!(
        !cells.is_empty(),
        "expected an error message history cell to be emitted",
    );
    let blob = lines_to_single_string(cells.last().unwrap());
    assert_snapshot!(blob);
}

#[tokio::test]
async fn providers_command_opens_provider_wizard() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.dispatch_command(SlashCommand::Providers);

    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenProviderConfigModal));
    assert!(!render_bottom_popup(&chat, 80).contains("Configure a custom API provider"));
}

#[tokio::test]
async fn missing_models_json_blocks_model_submit_and_opens_provider_wizard() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;
    chat.config.provider_config_required = true;

    chat.submit_user_message("hello".to_string().into());

    assert!(
        op_rx.try_recv().is_err(),
        "missing models.json must not submit a model turn"
    );
    let events = drain_events(&mut rx);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppEvent::OpenProviderConfigModal))
    );
    let cells = events
        .into_iter()
        .filter_map(into_insert_history_cell)
        .map(|cell| cell.display_lines(80))
        .collect::<Vec<_>>();
    let blob = lines_to_single_string(cells.last().expect("info message cell"));
    assert!(blob.contains("Configure a model provider before starting a session."));
}

#[tokio::test]
async fn missing_models_json_routes_model_command_to_provider_wizard() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.config.provider_config_required = true;

    chat.dispatch_command(SlashCommand::Model);

    let events = drain_events(&mut rx);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppEvent::OpenProviderConfigModal))
    );
    let cells = events
        .into_iter()
        .filter_map(into_insert_history_cell)
        .map(|cell| cell.display_lines(80))
        .collect::<Vec<_>>();
    let blob = lines_to_single_string(cells.last().expect("info message cell"));
    assert!(blob.contains("Configure a model provider before choosing a model."));
}

#[tokio::test]
async fn missing_models_json_placeholder_header_shows_no_provider_configured() {
    let (chat, _rx) = make_provider_required_chatwidget(true).await;
    let active_cell = chat.active_cell.as_ref().expect("placeholder header");
    let rendered = lines_to_single_string(&active_cell.display_lines(120));
    assert!(rendered.contains("No provider configured"));
    assert!(!rendered.contains("loading"));
}

#[tokio::test]
async fn missing_models_json_startup_does_not_emit_info_history_cell() {
    let (chat, mut rx) = make_provider_required_chatwidget(true).await;
    let popup = render_bottom_popup(&chat, 80);
    assert!(
        !popup.contains("Configure a custom API provider"),
        "startup provider modal should be owned by App, not the bottom pane:\n{popup}"
    );
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenProviderConfigModal));
    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn missing_models_json_startup_can_defer_provider_popup_to_app_modal() {
    let (chat, mut rx) = make_provider_required_chatwidget(false).await;

    assert!(
        !render_bottom_popup(&chat, 80).contains("Configure a custom API provider"),
        "startup provider modal should be owned by App, not the bottom pane"
    );
    assert!(
        rx.try_recv().is_err(),
        "startup should not emit transcript history cells for required provider config"
    );
}

#[tokio::test]
async fn providers_command_is_disabled_while_task_running() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.bottom_pane.set_task_running(true);

    chat.dispatch_command(SlashCommand::Providers);

    let cells = drain_insert_history(&mut rx);
    let blob = lines_to_single_string(cells.last().expect("error message cell"));
    assert!(blob.contains("'/providers' is disabled while a task is in progress."));
    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
    assert!(!render_bottom_popup(&chat, 80).contains("Configure a custom API provider"));
}

#[tokio::test]
async fn approvals_popup_shows_disabled_presets() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.config.approval_policy =
        Constrained::new(AskForApproval::OnRequest, |candidate| match candidate {
            AskForApproval::OnRequest => Ok(()),
            _ => Err(invalid_value(
                candidate.to_string(),
                "this message should be printed in the description",
            )),
        })
        .expect("construct constrained approval policy");
    chat.open_approvals_popup();

    let width = 80;
    let height = chat.desired_height(width);
    let mut terminal =
        ratatui::Terminal::new(VT100Backend::new(width, height)).expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("render approvals popup");

    let screen = terminal.backend().vt100().screen().contents();
    let collapsed = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains("(disabled)"),
        "disabled preset label should be shown"
    );
    assert!(
        collapsed.contains("this message should be printed in the description"),
        "disabled preset reason should be shown"
    );
}

#[tokio::test]
async fn approvals_popup_navigation_skips_disabled() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;

    chat.config.approval_policy =
        Constrained::new(AskForApproval::OnRequest, |candidate| match candidate {
            AskForApproval::OnRequest => Ok(()),
            _ => Err(invalid_value(candidate.to_string(), "[on-request]")),
        })
        .expect("construct constrained approval policy");
    chat.open_approvals_popup();

    // The approvals popup is the active bottom-pane view; drive navigation via chat handle_key_event.
    // Start selected at idx 0 (enabled), move down twice; the disabled option should be skipped
    // and selection should wrap back to idx 0 (also enabled).
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));

    // Press numeric shortcut for the disabled row (3 => idx 2); should not close or accept.
    chat.handle_key_event(KeyEvent::from(KeyCode::Char('3')));

    // Ensure the popup remains open and no selection actions were sent.
    let width = 80;
    let height = chat.desired_height(width);
    let mut terminal =
        ratatui::Terminal::new(VT100Backend::new(width, height)).expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("render approvals popup after disabled selection");
    let screen = terminal.backend().vt100().screen().contents();
    assert!(
        screen.contains("Update Model Permissions"),
        "popup should remain open after selecting a disabled entry"
    );
    assert!(
        op_rx.try_recv().is_err(),
        "no actions should be dispatched yet"
    );
    assert!(rx.try_recv().is_err(), "no history should be emitted");

    // Press Enter; selection should land on an enabled preset and dispatch updates.
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let mut app_events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        app_events.push(ev);
    }
    assert!(
        app_events.iter().any(|ev| matches!(
            ev,
            AppEvent::CodexOp(Op::OverrideTurnContext {
                approval_policy: Some(AskForApproval::OnRequest),
                personality: None,
                ..
            })
        )),
        "enter should select an enabled preset"
    );
    assert!(
        !app_events.iter().any(|ev| matches!(
            ev,
            AppEvent::CodexOp(Op::OverrideTurnContext {
                approval_policy: Some(AskForApproval::Never),
                personality: None,
                ..
            })
        )),
        "disabled preset should not be selected"
    );
}

//
// Snapshot test: command approval modal
//
// Synthesizes a Adam ExecApprovalRequest event to trigger the approval modal
// and snapshots the visual output using the ratatui TestBackend.
#[tokio::test]
async fn approval_modal_exec_snapshot() -> anyhow::Result<()> {
    // Build a chat widget with manual channels to avoid spawning the agent.
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    // Ensure policy allows surfacing approvals explicitly (not strictly required for direct event).
    chat.config.approval_policy.set(AskForApproval::OnRequest)?;
    // Inject an exec approval request to display the approval modal.
    let ev = ExecApprovalRequestEvent {
        call_id: "call-approve-cmd".into(),
        turn_id: "turn-approve-cmd".into(),
        command: vec!["bash".into(), "-lc".into(), "echo hello world".into()],
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        reason: Some(
            "this is a test reason such as one that would be produced by the model".into(),
        ),
        proposed_execpolicy_amendment: Some(ExecPolicyAmendment::new(vec![
            "echo".into(),
            "hello".into(),
            "world".into(),
        ])),
        parsed_cmd: vec![],
    };
    chat.handle_codex_event(Event {
        id: "sub-approve".into(),
        msg: EventMsg::ExecApprovalRequest(ev),
    });
    // Render to a fixed-size test terminal and snapshot.
    // Call desired_height first and use that exact height for rendering.
    let width = 100;
    let height = chat.desired_height(width);
    let mut terminal =
        crate::custom_terminal::Terminal::with_options(VT100Backend::new(width, height))
            .expect("create terminal");
    let viewport = Rect::new(0, 0, width, height);
    terminal.set_viewport_area(viewport);

    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw approval modal");
    assert!(
        terminal
            .backend()
            .vt100()
            .screen()
            .contents()
            .contains("echo hello world")
    );
    assert_snapshot!(
        "approval_modal_exec",
        terminal.backend().vt100().screen().contents()
    );

    Ok(())
}

// Snapshot test: command approval modal without a reason
// Ensures spacing looks correct when no reason text is provided.
#[tokio::test]
async fn approval_modal_exec_without_reason_snapshot() -> anyhow::Result<()> {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.config.approval_policy.set(AskForApproval::OnRequest)?;

    let ev = ExecApprovalRequestEvent {
        call_id: "call-approve-cmd-noreason".into(),
        turn_id: "turn-approve-cmd-noreason".into(),
        command: vec!["bash".into(), "-lc".into(), "echo hello world".into()],
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        reason: None,
        proposed_execpolicy_amendment: Some(ExecPolicyAmendment::new(vec![
            "echo".into(),
            "hello".into(),
            "world".into(),
        ])),
        parsed_cmd: vec![],
    };
    chat.handle_codex_event(Event {
        id: "sub-approve-noreason".into(),
        msg: EventMsg::ExecApprovalRequest(ev),
    });

    let width = 100;
    let height = chat.desired_height(width);
    let mut terminal =
        ratatui::Terminal::new(VT100Backend::new(width, height)).expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw approval modal (no reason)");
    assert_snapshot!(
        "approval_modal_exec_no_reason",
        terminal.backend().vt100().screen().contents()
    );

    Ok(())
}

// Snapshot test: approval modal with a proposed execpolicy prefix that is multi-line;
// we should not offer adding it to execpolicy.
#[tokio::test]
async fn approval_modal_exec_multiline_prefix_hides_execpolicy_option_snapshot()
-> anyhow::Result<()> {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.config.approval_policy.set(AskForApproval::OnRequest)?;

    let script = "python - <<'PY'\nprint('hello')\nPY".to_string();
    let command = vec!["bash".into(), "-lc".into(), script];
    let ev = ExecApprovalRequestEvent {
        call_id: "call-approve-cmd-multiline-trunc".into(),
        turn_id: "turn-approve-cmd-multiline-trunc".into(),
        command: command.clone(),
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        reason: None,
        proposed_execpolicy_amendment: Some(ExecPolicyAmendment::new(command)),
        parsed_cmd: vec![],
    };
    chat.handle_codex_event(Event {
        id: "sub-approve-multiline-trunc".into(),
        msg: EventMsg::ExecApprovalRequest(ev),
    });

    let width = 100;
    let height = chat.desired_height(width);
    let mut terminal =
        ratatui::Terminal::new(VT100Backend::new(width, height)).expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw approval modal (multiline prefix)");
    let contents = terminal.backend().vt100().screen().contents();
    assert!(!contents.contains("don't ask again"));
    assert_snapshot!(
        "approval_modal_exec_multiline_prefix_no_execpolicy",
        contents
    );

    Ok(())
}

// Snapshot test: patch approval modal
#[tokio::test]
async fn approval_modal_patch_snapshot() -> anyhow::Result<()> {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.config.approval_policy.set(AskForApproval::OnRequest)?;

    // Build a small changeset and a reason/grant_root to exercise the prompt text.
    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("README.md"),
        FileChange::Add {
            content: "hello\nworld\n".into(),
        },
    );
    let ev = ApplyPatchApprovalRequestEvent {
        call_id: "call-approve-patch".into(),
        turn_id: "turn-approve-patch".into(),
        changes,
        reason: Some("The model wants to apply changes".into()),
        grant_root: Some(PathBuf::from("/tmp")),
    };
    chat.handle_codex_event(Event {
        id: "sub-approve-patch".into(),
        msg: EventMsg::ApplyPatchApprovalRequest(ev),
    });

    // Render at the widget's desired height and snapshot.
    let height = chat.desired_height(80);
    let mut terminal =
        ratatui::Terminal::new(VT100Backend::new(80, height)).expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, 80, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw patch approval modal");
    assert_snapshot!(
        "approval_modal_patch",
        terminal.backend().vt100().screen().contents()
    );

    Ok(())
}

#[tokio::test]
async fn interrupt_restores_queued_messages_into_composer() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;

    // Simulate a running task to enable queuing of user inputs.
    chat.bottom_pane.set_task_running(true);

    // Queue two user messages while the task is running.
    chat.queued_user_messages
        .push_back(UserMessage::from("first queued".to_string()));
    chat.queued_user_messages
        .push_back(UserMessage::from("second queued".to_string()));
    chat.refresh_queued_user_messages();

    // Deliver a TurnAborted event with Interrupted reason (as if Esc was pressed).
    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnAborted(adam_agent::protocol::TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }),
    });

    // Composer should now contain the queued messages joined by newlines, in order.
    assert_eq!(
        chat.bottom_pane.composer_text(),
        "first queued\nsecond queued"
    );

    // Queue should be cleared and no new user input should have been auto-submitted.
    assert!(chat.queued_user_messages.is_empty());
    assert!(
        op_rx.try_recv().is_err(),
        "unexpected outbound op after interrupt"
    );

    // Drain rx to avoid unused warnings.
    let _ = drain_insert_history(&mut rx);
}

#[tokio::test]
async fn interrupt_prepends_queued_messages_before_existing_composer_text() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;

    chat.bottom_pane.set_task_running(true);
    chat.bottom_pane
        .set_composer_text("current draft".to_string(), Vec::new(), Vec::new());

    chat.queued_user_messages
        .push_back(UserMessage::from("first queued".to_string()));
    chat.queued_user_messages
        .push_back(UserMessage::from("second queued".to_string()));
    chat.refresh_queued_user_messages();

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnAborted(adam_agent::protocol::TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }),
    });

    assert_eq!(
        chat.bottom_pane.composer_text(),
        "first queued\nsecond queued\ncurrent draft"
    );
    assert!(chat.queued_user_messages.is_empty());
    assert!(
        op_rx.try_recv().is_err(),
        "unexpected outbound op after interrupt"
    );

    let _ = drain_insert_history(&mut rx);
}

#[tokio::test]
async fn interrupt_preserves_unified_exec_processes() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    begin_unified_exec_startup(&mut chat, "call-1", "process-1", "sleep 5");
    begin_unified_exec_startup(&mut chat, "call-2", "process-2", "sleep 6");
    assert_eq!(chat.unified_exec_processes.len(), 2);

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnAborted(adam_agent::protocol::TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }),
    });

    assert_eq!(chat.unified_exec_processes.len(), 2);

    let _ = drain_insert_history(&mut rx);
}

#[tokio::test]
async fn interrupt_preserves_unified_exec_wait_streak_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });

    let begin = begin_unified_exec_startup(&mut chat, "call-1", "process-1", "just fix");
    terminal_interaction(&mut chat, "call-1a", "process-1", "");

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnAborted(adam_agent::protocol::TurnAbortedEvent {
            reason: TurnAbortReason::Interrupted,
        }),
    });

    end_exec(&mut chat, begin, "", "", 0);
    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    let snapshot = format!("cells={}\n{combined}", cells.len());
    assert_snapshot!("interrupt_preserves_unified_exec_wait_streak", snapshot);
}

#[tokio::test]
async fn turn_complete_clears_unified_exec_processes() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    begin_unified_exec_startup(&mut chat, "call-1", "process-1", "sleep 5");
    begin_unified_exec_startup(&mut chat, "call-2", "process-2", "sleep 6");
    assert_eq!(chat.unified_exec_processes.len(), 2);

    chat.handle_codex_event(Event {
        id: "turn-1".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: None,
        }),
    });

    assert!(chat.unified_exec_processes.is_empty());

    let _ = drain_insert_history(&mut rx);
}

// Snapshot test: ChatWidget at very small heights (idle)
// Ensures overall layout behaves when terminal height is extremely constrained.
#[tokio::test]
async fn ui_snapshots_small_heights_idle() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let (chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    for h in [1u16, 2, 3] {
        let name = format!("chat_small_idle_h{h}");
        let mut terminal = Terminal::new(TestBackend::new(40, h)).expect("create terminal");
        terminal
            .draw(|f| chat.render(f.area(), f.buffer_mut()))
            .expect("draw chat idle");
        assert_snapshot!(name, terminal.backend());
    }
}

// Snapshot test: ChatWidget at very small heights (task running)
// Validates how status + composer are presented within tight space.
#[tokio::test]
async fn ui_snapshots_small_heights_task_running() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    // Activate status line
    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });
    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent {
            delta: "**Thinking**".into(),
        }),
    });
    for h in [1u16, 2, 3] {
        let name = format!("chat_small_running_h{h}");
        let mut terminal = Terminal::new(TestBackend::new(40, h)).expect("create terminal");
        terminal
            .draw(|f| chat.render(f.area(), f.buffer_mut()))
            .expect("draw chat running");
        assert_snapshot!(name, terminal.backend());
    }
}

// Snapshot test: status widget + approval modal active together
// The modal takes precedence visually; this captures the layout with a running
// task (status indicator active) while an approval request is shown.
#[tokio::test]
async fn status_widget_and_approval_modal_snapshot() {
    use adam_agent::protocol::ExecApprovalRequestEvent;

    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    // Begin a running task so the status indicator would be active.
    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });
    // Provide a deterministic header for the status line.
    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent {
            delta: "**Analyzing**".into(),
        }),
    });

    // Now show an approval modal (e.g. exec approval).
    let ev = ExecApprovalRequestEvent {
        call_id: "call-approve-exec".into(),
        turn_id: "turn-approve-exec".into(),
        command: vec!["echo".into(), "hello world".into()],
        cwd: PathBuf::from("/tmp"),
        reason: Some(
            "this is a test reason such as one that would be produced by the model".into(),
        ),
        proposed_execpolicy_amendment: Some(ExecPolicyAmendment::new(vec![
            "echo".into(),
            "hello world".into(),
        ])),
        parsed_cmd: vec![],
    };
    chat.handle_codex_event(Event {
        id: "sub-approve-exec".into(),
        msg: EventMsg::ExecApprovalRequest(ev),
    });

    // Render at the widget's desired height and snapshot.
    let width: u16 = 100;
    let height = chat.desired_height(width);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
        .expect("create terminal");
    terminal.set_viewport_area(Rect::new(0, 0, width, height));
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw status + approval modal");
    assert_snapshot!("status_widget_and_approval_modal", terminal.backend());
}

// Snapshot test: status widget active (StatusIndicatorView)
// Ensures the VT100 rendering of the status indicator is stable when active.
#[tokio::test]
async fn status_widget_active_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    apply_stable_snapshot_cwd(&mut chat);
    // Activate the status indicator by simulating a task start.
    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });
    // Provide a deterministic header via a bold reasoning chunk.
    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent {
            delta: "**Analyzing**".into(),
        }),
    });
    // Render and snapshot.
    let height = chat.desired_height(80);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height))
        .expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw status widget");
    assert_snapshot!("status_widget_active", terminal.backend());
}

#[tokio::test]
async fn mcp_startup_header_booting_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    apply_stable_snapshot_cwd(&mut chat);
    chat.show_welcome_banner = false;

    chat.handle_codex_event(Event {
        id: "mcp-1".into(),
        msg: EventMsg::McpStartupUpdate(McpStartupUpdateEvent {
            server: "alpha".into(),
            status: McpStartupStatus::Starting,
        }),
    });

    let height = chat.desired_height(80);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height))
        .expect("create terminal");
    terminal
        .draw(|f| chat.render(f.area(), f.buffer_mut()))
        .expect("draw chat widget");
    assert_snapshot!("mcp_startup_header_booting", terminal.backend());
}

#[tokio::test]
async fn mcp_startup_complete_does_not_clear_running_task() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "task-1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });

    assert!(chat.bottom_pane.is_task_running());
    assert!(chat.bottom_pane.status_indicator_visible());

    chat.handle_codex_event(Event {
        id: "mcp-1".into(),
        msg: EventMsg::McpStartupComplete(McpStartupCompleteEvent {
            ready: vec!["schaltwerk".into()],
            ..Default::default()
        }),
    });

    assert!(chat.bottom_pane.is_task_running());
    assert!(chat.bottom_pane.status_indicator_visible());
}

#[tokio::test]
async fn background_event_updates_status_header() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    chat.handle_codex_event(Event {
        id: "bg-1".into(),
        msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
            message: "Waiting for `vim`".to_string(),
        }),
    });

    assert!(chat.bottom_pane.status_indicator_visible());
    assert_eq!(chat.current_status.header, "Waiting for `vim`");
    assert!(drain_insert_history(&mut rx).is_empty());
}

#[tokio::test]
async fn apply_patch_events_emit_history_cells() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // 1) Approval request -> proposed patch summary cell
    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    let ev = ApplyPatchApprovalRequestEvent {
        call_id: "c1".into(),
        turn_id: "turn-c1".into(),
        changes,
        reason: None,
        grant_root: None,
    };
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::ApplyPatchApprovalRequest(ev),
    });
    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected approval request to surface via modal without emitting history cells"
    );

    let area = Rect::new(0, 0, 80, chat.desired_height(80));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    chat.render(area, &mut buf);
    let mut saw_summary = false;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        if row.contains("foo.txt (+1 -0)") {
            saw_summary = true;
            break;
        }
    }
    assert!(saw_summary, "expected approval modal to show diff summary");

    // 2) Begin apply -> per-file apply block cell (no global header)
    let mut changes2 = HashMap::new();
    changes2.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    let begin = PatchApplyBeginEvent {
        call_id: "c1".into(),
        turn_id: "turn-c1".into(),
        auto_approved: true,
        changes: changes2,
    };
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::PatchApplyBegin(begin),
    });
    let cells = drain_insert_history(&mut rx);
    assert!(!cells.is_empty(), "expected apply block cell to be sent");
    let blob = lines_to_single_string(cells.last().unwrap());
    assert!(
        blob.contains("Added foo.txt") || blob.contains("Edited foo.txt"),
        "expected single-file header with filename (Added/Edited): {blob:?}"
    );

    // 3) End apply success -> success cell
    let mut end_changes = HashMap::new();
    end_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    let end = PatchApplyEndEvent {
        call_id: "c1".into(),
        turn_id: "turn-c1".into(),
        stdout: "ok\n".into(),
        stderr: String::new(),
        success: true,
        changes: end_changes,
    };
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::PatchApplyEnd(end),
    });
    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "no success cell should be emitted anymore"
    );
}

#[tokio::test]
async fn apply_patch_manual_approval_adjusts_header() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let mut proposed_changes = HashMap::new();
    proposed_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id: "c1".into(),
            turn_id: "turn-c1".into(),
            changes: proposed_changes,
            reason: None,
            grant_root: None,
        }),
    });
    drain_insert_history(&mut rx);

    let mut apply_changes = HashMap::new();
    apply_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
            call_id: "c1".into(),
            turn_id: "turn-c1".into(),
            auto_approved: false,
            changes: apply_changes,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert!(!cells.is_empty(), "expected apply block cell to be sent");
    let blob = lines_to_single_string(cells.last().unwrap());
    assert!(
        blob.contains("Added foo.txt") || blob.contains("Edited foo.txt"),
        "expected apply summary header for foo.txt: {blob:?}"
    );
}

#[tokio::test]
async fn apply_patch_manual_flow_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    let mut proposed_changes = HashMap::new();
    proposed_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id: "c1".into(),
            turn_id: "turn-c1".into(),
            changes: proposed_changes,
            reason: Some("Manual review required".into()),
            grant_root: None,
        }),
    });
    let history_before_apply = drain_insert_history(&mut rx);
    assert!(
        history_before_apply.is_empty(),
        "expected approval modal to defer history emission"
    );

    let mut apply_changes = HashMap::new();
    apply_changes.insert(
        PathBuf::from("foo.txt"),
        FileChange::Add {
            content: "hello\n".to_string(),
        },
    );
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
            call_id: "c1".into(),
            turn_id: "turn-c1".into(),
            auto_approved: false,
            changes: apply_changes,
        }),
    });
    let approved_lines = drain_insert_history(&mut rx)
        .pop()
        .expect("approved patch cell");

    assert_snapshot!(
        "apply_patch_manual_flow_history_approved",
        lines_to_single_string(&approved_lines)
    );
}

#[tokio::test]
async fn apply_patch_approval_sends_op_with_submission_id() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    // Simulate receiving an approval request with a distinct submission id and call id
    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("file.rs"),
        FileChange::Add {
            content: "fn main(){}\n".into(),
        },
    );
    let ev = ApplyPatchApprovalRequestEvent {
        call_id: "call-999".into(),
        turn_id: "turn-999".into(),
        changes,
        reason: None,
        grant_root: None,
    };
    chat.handle_codex_event(Event {
        id: "sub-123".into(),
        msg: EventMsg::ApplyPatchApprovalRequest(ev),
    });

    // Approve via key press 'y'
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

    // Expect a CodexOp with PatchApproval carrying the submission id, not call id
    let mut found = false;
    while let Ok(app_ev) = rx.try_recv() {
        if let AppEvent::CodexOp(Op::PatchApproval { id, decision }) = app_ev {
            assert_eq!(id, "sub-123");
            assert_matches!(decision, adam_agent::protocol::ReviewDecision::Approved);
            found = true;
            break;
        }
    }
    assert!(found, "expected PatchApproval op to be sent");
}

#[tokio::test]
async fn apply_patch_full_flow_integration_like() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(None).await;

    // 1) Backend requests approval
    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("pkg.rs"),
        FileChange::Add { content: "".into() },
    );
    chat.handle_codex_event(Event {
        id: "sub-xyz".into(),
        msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id: "call-1".into(),
            turn_id: "turn-call-1".into(),
            changes,
            reason: None,
            grant_root: None,
        }),
    });

    // 2) User approves via 'y' and App receives a CodexOp
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let mut maybe_op: Option<Op> = None;
    while let Ok(app_ev) = rx.try_recv() {
        if let AppEvent::CodexOp(op) = app_ev {
            maybe_op = Some(op);
            break;
        }
    }
    let op = maybe_op.expect("expected CodexOp after key press");

    // 3) App forwards to widget.submit_op, which pushes onto codex_op_tx
    chat.submit_op(op);
    let forwarded = op_rx
        .try_recv()
        .expect("expected op forwarded to codex channel");
    match forwarded {
        Op::PatchApproval { id, decision } => {
            assert_eq!(id, "sub-xyz");
            assert_matches!(decision, adam_agent::protocol::ReviewDecision::Approved);
        }
        other => panic!("unexpected op forwarded: {other:?}"),
    }

    // 4) Simulate patch begin/end events from backend; ensure history cells are emitted
    let mut changes2 = HashMap::new();
    changes2.insert(
        PathBuf::from("pkg.rs"),
        FileChange::Add { content: "".into() },
    );
    chat.handle_codex_event(Event {
        id: "sub-xyz".into(),
        msg: EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
            call_id: "call-1".into(),
            turn_id: "turn-call-1".into(),
            auto_approved: false,
            changes: changes2,
        }),
    });
    let mut end_changes = HashMap::new();
    end_changes.insert(
        PathBuf::from("pkg.rs"),
        FileChange::Add { content: "".into() },
    );
    chat.handle_codex_event(Event {
        id: "sub-xyz".into(),
        msg: EventMsg::PatchApplyEnd(PatchApplyEndEvent {
            call_id: "call-1".into(),
            turn_id: "turn-call-1".into(),
            stdout: String::from("ok"),
            stderr: String::new(),
            success: true,
            changes: end_changes,
        }),
    });
}

#[tokio::test]
async fn apply_patch_untrusted_shows_approval_modal() -> anyhow::Result<()> {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    // Ensure approval policy is untrusted (OnRequest)
    chat.config.approval_policy.set(AskForApproval::OnRequest)?;

    // Simulate a patch approval request from backend
    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("a.rs"),
        FileChange::Add { content: "".into() },
    );
    chat.handle_codex_event(Event {
        id: "sub-1".into(),
        msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id: "call-1".into(),
            turn_id: "turn-call-1".into(),
            changes,
            reason: None,
            grant_root: None,
        }),
    });

    // Render and ensure the approval modal title is present
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    let mut contains_title = false;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        if row.contains("Would you like to make the following edits?") {
            contains_title = true;
            break;
        }
    }
    assert!(
        contains_title,
        "expected approval modal to be visible with title 'Would you like to make the following edits?'"
    );

    Ok(())
}

#[tokio::test]
async fn apply_patch_request_shows_diff_summary() -> anyhow::Result<()> {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Ensure we are in OnRequest so an approval is surfaced
    chat.config.approval_policy.set(AskForApproval::OnRequest)?;

    // Simulate backend asking to apply a patch adding two lines to README.md
    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("README.md"),
        FileChange::Add {
            // Two lines (no trailing empty line counted)
            content: "line one\nline two\n".into(),
        },
    );
    chat.handle_codex_event(Event {
        id: "sub-apply".into(),
        msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id: "call-apply".into(),
            turn_id: "turn-apply".into(),
            changes,
            reason: None,
            grant_root: None,
        }),
    });

    // No history entries yet; the modal should contain the diff summary
    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected approval request to render via modal instead of history"
    );

    let area = Rect::new(0, 0, 80, chat.desired_height(80));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    chat.render(area, &mut buf);

    let mut saw_header = false;
    let mut saw_line1 = false;
    let mut saw_line2 = false;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        if row.contains("README.md (+2 -0)") {
            saw_header = true;
        }
        if row.contains("+line one") {
            saw_line1 = true;
        }
        if row.contains("+line two") {
            saw_line2 = true;
        }
        if saw_header && saw_line1 && saw_line2 {
            break;
        }
    }
    assert!(saw_header, "expected modal to show diff header with totals");
    assert!(
        saw_line1 && saw_line2,
        "expected modal to show per-line diff summary"
    );

    Ok(())
}

#[tokio::test]
async fn plan_update_renders_history_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let update = UpdatePlanArgs {
        explanation: Some("Adapting plan".to_string()),
        plan: vec![
            PlanItemArg {
                step: "Explore codebase".into(),
                status: StepStatus::Completed,
            },
            PlanItemArg {
                step: "Implement feature".into(),
                status: StepStatus::InProgress,
            },
            PlanItemArg {
                step: "Write tests".into(),
                status: StepStatus::Pending,
            },
        ],
    };
    chat.handle_codex_event(Event {
        id: "sub-1".into(),
        msg: EventMsg::PlanUpdate(update),
    });
    let cells = drain_insert_history(&mut rx);
    assert!(!cells.is_empty(), "expected plan update cell to be sent");
    let blob = lines_to_single_string(cells.last().unwrap());
    assert!(
        blob.contains("Updated Plan"),
        "missing plan header: {blob:?}"
    );
    assert!(blob.contains("Explore codebase"));
    assert!(blob.contains("Implement feature"));
    assert!(blob.contains("Write tests"));
}

#[tokio::test]
async fn plan_update_populates_sidebar_todo() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_plan_update(UpdatePlanArgs {
        explanation: Some("Adapting plan".to_string()),
        plan: vec![
            PlanItemArg {
                step: "Explore codebase".into(),
                status: StepStatus::Completed,
            },
            PlanItemArg {
                step: "Implement feature".into(),
                status: StepStatus::InProgress,
            },
            PlanItemArg {
                step: "Write tests".into(),
                status: StepStatus::Pending,
            },
        ],
    });

    let todo = chat
        .sidebar_snapshot()
        .todo
        .expect("plan update should populate sidebar todo");
    let steps = todo
        .items
        .iter()
        .map(|item| item.step.as_str())
        .collect::<Vec<_>>();
    let statuses = todo
        .items
        .iter()
        .map(|item| match item.status {
            StepStatus::Completed => "completed",
            StepStatus::InProgress => "in_progress",
            StepStatus::Pending => "pending",
        })
        .collect::<Vec<_>>();
    assert_eq!(
        (steps, statuses),
        (
            vec!["Explore codebase", "Implement feature", "Write tests"],
            vec!["completed", "in_progress", "pending"]
        )
    );
}

#[tokio::test]
async fn loaded_skills_are_tracked_once_by_path() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    let skill_path = PathBuf::from("/tmp/adam-test-skill/SKILL.md");
    let items = vec![
        UserInput::Skill {
            name: "skill-one".to_string(),
            path: skill_path.clone(),
        },
        UserInput::Skill {
            name: "skill-one-again".to_string(),
            path: skill_path.clone(),
        },
    ];

    chat.track_loaded_skills_from_inputs(&items);

    let skills = chat.sidebar_snapshot().skills;
    assert_eq!(
        skills,
        vec![SkillPanelEntry {
            name: "skill-one".to_string(),
            path: skill_path,
        }]
    );
    assert_eq!(
        skills[0].path,
        PathBuf::from("/tmp/adam-test-skill/SKILL.md")
    );
}

#[tokio::test]
async fn stream_error_updates_status_indicator() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.bottom_pane.set_task_running(true);
    let msg = "Reconnecting... 2/5";
    let details = "Idle timeout waiting for SSE";
    chat.handle_codex_event(Event {
        id: "sub-1".into(),
        msg: EventMsg::StreamError(StreamErrorEvent {
            message: msg.to_string(),
            codex_error_info: Some(CodexErrorInfo::Other),
            additional_details: Some(details.to_string()),
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert!(
        cells.is_empty(),
        "expected no history cell for StreamError event"
    );
    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), msg);
    assert_eq!(status.details(), Some(details));
}

#[tokio::test]
async fn warning_event_adds_warning_history_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.handle_codex_event(Event {
        id: "sub-1".into(),
        msg: EventMsg::Warning(WarningEvent {
            message: "test warning message".to_string(),
        }),
    });

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1, "expected one warning history cell");
    let rendered = lines_to_single_string(&cells[0]);
    assert!(
        rendered.contains("test warning message"),
        "warning cell missing content: {rendered}"
    );
}

#[tokio::test]
async fn stream_recovery_restores_previous_status_header() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.handle_codex_event(Event {
        id: "task".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });
    drain_insert_history(&mut rx);
    chat.handle_codex_event(Event {
        id: "retry".into(),
        msg: EventMsg::StreamError(StreamErrorEvent {
            message: "Reconnecting... 1/5".to_string(),
            codex_error_info: Some(CodexErrorInfo::Other),
            additional_details: None,
        }),
    });
    drain_insert_history(&mut rx);
    chat.handle_codex_event(Event {
        id: "delta".into(),
        msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
            delta: "hello".to_string(),
        }),
    });

    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), "Working");
    assert_eq!(status.details(), None);
    assert!(chat.retry_status.is_none());
}

#[tokio::test]
async fn stream_recovery_restores_previous_status_details() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    chat.on_task_started();
    begin_unified_exec_startup(&mut chat, "call-retry", "proc-retry", "sleep 5");

    terminal_interaction(&mut chat, "call-retry-stdin", "proc-retry", "");
    drain_insert_history(&mut rx);

    chat.handle_codex_event(Event {
        id: "retry".into(),
        msg: EventMsg::StreamError(StreamErrorEvent {
            message: "Reconnecting... 1/5".to_string(),
            codex_error_info: Some(CodexErrorInfo::Other),
            additional_details: None,
        }),
    });
    drain_insert_history(&mut rx);

    chat.handle_codex_event(Event {
        id: "warning".into(),
        msg: EventMsg::Warning(WarningEvent {
            message: "test warning message".to_string(),
        }),
    });

    let status = chat
        .bottom_pane
        .status_widget()
        .expect("status indicator should be visible");
    assert_eq!(status.header(), "Waiting for background terminal");
    assert_eq!(status.details(), Some("sleep 5"));
    assert!(chat.retry_status.is_none());
}

#[tokio::test]
async fn multiple_agent_messages_in_single_turn_emit_multiple_headers() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Begin turn
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });

    // First finalized assistant message
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentMessage(AgentMessageEvent {
            message: "First message".into(),
        }),
    });

    // Second finalized assistant message in the same turn
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentMessage(AgentMessageEvent {
            message: "Second message".into(),
        }),
    });

    // End turn
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: None,
        }),
    });

    let cells = drain_insert_history(&mut rx);
    let combined: String = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect();
    assert!(
        combined.contains("First message"),
        "missing first message: {combined}"
    );
    assert!(
        combined.contains("Second message"),
        "missing second message: {combined}"
    );
    let first_idx = combined.find("First message").unwrap();
    let second_idx = combined.find("Second message").unwrap();
    assert!(first_idx < second_idx, "messages out of order: {combined}");
}

#[tokio::test]
async fn final_reasoning_then_message_without_deltas_are_rendered() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // No deltas; only final reasoning followed by final message.
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentReasoning(AgentReasoningEvent {
            text: "I will first analyze the request.".into(),
        }),
    });
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentMessage(AgentMessageEvent {
            message: "Here is the result.".into(),
        }),
    });

    // Drain history and snapshot the combined visible content.
    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_snapshot!(combined);
}

#[tokio::test]
async fn deltas_then_same_final_message_are_rendered_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Stream some reasoning deltas first.
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent {
            delta: "I will ".into(),
        }),
    });
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent {
            delta: "first analyze the ".into(),
        }),
    });
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent {
            delta: "request.".into(),
        }),
    });
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentReasoning(AgentReasoningEvent {
            text: "request.".into(),
        }),
    });

    // Then stream answer deltas, followed by the exact same final message.
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
            delta: "Here is the ".into(),
        }),
    });
    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
            delta: "result.".into(),
        }),
    });

    chat.handle_codex_event(Event {
        id: "s1".into(),
        msg: EventMsg::AgentMessage(AgentMessageEvent {
            message: "Here is the result.".into(),
        }),
    });

    // Snapshot the combined visible content to ensure we render as expected
    // when deltas are followed by the identical final message.
    let cells = drain_insert_history(&mut rx);
    let combined = cells
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_snapshot!(combined);
}

// Combined visual snapshot using vt100 for history + direct buffer overlay for UI.
// This renders the final visual as seen in a terminal: history above, then a blank line,
// then the exec block, another blank line, the status line, a blank line, and the composer.
#[tokio::test]
async fn chatwidget_exec_and_status_layout_vt100_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    apply_stable_snapshot_cwd(&mut chat);
    chat.handle_codex_event(Event {
        id: "t1".into(),
        msg: EventMsg::AgentMessage(AgentMessageEvent { message: "I’m going to search the repo for where “Change Approved” is rendered to update that view.".into() }),
    });

    let command = vec!["bash".into(), "-lc".into(), "rg \"Change Approved\"".into()];
    let parsed_cmd = vec![
        ParsedCommand::Search {
            query: Some("Change Approved".into()),
            path: None,
            cmd: "rg \"Change Approved\"".into(),
        },
        ParsedCommand::Read {
            name: "diff_render.rs".into(),
            cmd: "cat diff_render.rs".into(),
            path: "diff_render.rs".into(),
        },
    ];
    let cwd = stable_snapshot_cwd();
    chat.handle_codex_event(Event {
        id: "c1".into(),
        msg: EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
            call_id: "c1".into(),
            process_id: None,
            turn_id: "turn-1".into(),
            command: command.clone(),
            cwd: cwd.clone(),
            parsed_cmd: parsed_cmd.clone(),
            source: ExecCommandSource::Agent,
            interaction_input: None,
        }),
    });
    chat.handle_codex_event(Event {
        id: "c1".into(),
        msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "c1".into(),
            process_id: None,
            turn_id: "turn-1".into(),
            command,
            cwd,
            parsed_cmd,
            source: ExecCommandSource::Agent,
            interaction_input: None,
            stdout: String::new(),
            stderr: String::new(),
            aggregated_output: String::new(),
            exit_code: 0,
            duration: std::time::Duration::from_millis(16000),
            formatted_output: String::new(),
        }),
    });
    chat.handle_codex_event(Event {
        id: "t1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });
    chat.handle_codex_event(Event {
        id: "t1".into(),
        msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent {
            delta: "**Investigating rendering code**".into(),
        }),
    });
    chat.bottom_pane.set_composer_text(
        "Summarize recent commits".to_string(),
        Vec::new(),
        Vec::new(),
    );

    let width: u16 = 80;
    let ui_height: u16 = chat.desired_height(width);
    let vt_height: u16 = 40;
    let viewport = Rect::new(0, vt_height - ui_height - 1, width, ui_height);

    let backend = VT100Backend::new(width, vt_height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    term.set_viewport_area(viewport);

    for lines in drain_insert_history(&mut rx) {
        crate::insert_history::insert_history_lines(&mut term, lines)
            .expect("Failed to insert history lines in test");
    }

    term.draw(|f| {
        chat.render(f.area(), f.buffer_mut());
    })
    .unwrap();

    assert_snapshot!(term.backend().vt100().screen().contents());
}

// E2E vt100 snapshot for complex markdown with indented and nested fenced code blocks
#[tokio::test]
async fn chatwidget_markdown_code_blocks_vt100_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;

    // Simulate a final agent message via streaming deltas instead of a single message

    chat.handle_codex_event(Event {
        id: "t1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });
    // Build a vt100 visual from the history insertions only (no UI overlay)
    let width: u16 = 80;
    let height: u16 = 50;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    // Place viewport at the last line so that history lines insert above it
    term.set_viewport_area(Rect::new(0, height - 1, width, 1));

    // Simulate streaming via AgentMessageDelta in 2-character chunks (no final AgentMessage).
    let source: &str = r#"

    -- Indented code block (4 spaces)
    SELECT *
    FROM "users"
    WHERE "email" LIKE '%@example.com';

````markdown
```sh
printf 'fenced within fenced\n'
```
````

```jsonc
{
  // comment allowed in jsonc
  "path": "C:\\Program Files\\App",
  "regex": "^foo.*(bar)?$"
}
```
"#;

    let mut it = source.chars();
    loop {
        let mut delta = String::new();
        match it.next() {
            Some(c) => delta.push(c),
            None => break,
        }
        if let Some(c2) = it.next() {
            delta.push(c2);
        }

        chat.handle_codex_event(Event {
            id: "t1".into(),
            msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent { delta }),
        });
        // Drive commit ticks and drain emitted history lines into the vt100 buffer.
        loop {
            chat.on_commit_tick();
            let mut inserted_any = false;
            while let Ok(app_ev) = rx.try_recv() {
                if let Some(cell) = into_insert_history_cell(app_ev) {
                    let lines = cell.display_lines(width);
                    crate::insert_history::insert_history_lines(&mut term, lines)
                        .expect("Failed to insert history lines in test");
                    inserted_any = true;
                }
            }
            if !inserted_any {
                break;
            }
        }
    }

    // Finalize the stream without sending a final AgentMessage, to flush any tail.
    chat.handle_codex_event(Event {
        id: "t1".into(),
        msg: EventMsg::TurnComplete(TurnCompleteEvent {
            last_agent_message: None,
        }),
    });
    for lines in drain_insert_history(&mut rx) {
        crate::insert_history::insert_history_lines(&mut term, lines)
            .expect("Failed to insert history lines in test");
    }

    assert_snapshot!(term.backend().vt100().screen().contents());
}

#[tokio::test]
async fn chatwidget_tall() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    apply_stable_snapshot_cwd(&mut chat);
    chat.thread_id = Some(ThreadId::new());
    chat.handle_codex_event(Event {
        id: "t1".into(),
        msg: EventMsg::TurnStarted(TurnStartedEvent {
            model_context_window: None,
            identity_kind: IdentityKind::Nobody,
        }),
    });
    for i in 0..30 {
        chat.queue_user_message(format!("Hello, world! {i}").into());
    }
    let width: u16 = 80;
    let height: u16 = 24;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let desired_height = chat.desired_height(width).min(height);
    term.set_viewport_area(Rect::new(0, height - desired_height, width, desired_height));
    term.draw(|f| {
        chat.render(f.area(), f.buffer_mut());
    })
    .unwrap();
    assert_snapshot!(term.backend().vt100().screen().contents());
}

#[tokio::test]
async fn review_queues_user_messages_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    apply_stable_snapshot_cwd(&mut chat);
    chat.thread_id = Some(ThreadId::new());

    chat.handle_codex_event(Event {
        id: "review-1".into(),
        msg: EventMsg::EnteredReviewMode(ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: Some("current changes".to_string()),
        }),
    });
    let _ = drain_insert_history(&mut rx);

    chat.queue_user_message(UserMessage::from(
        "Queued while /review is running.".to_string(),
    ));

    let width: u16 = 80;
    let height: u16 = 18;
    let backend = VT100Backend::new(width, height);
    let mut term = crate::custom_terminal::Terminal::with_options(backend).expect("terminal");
    let desired_height = chat.desired_height(width).min(height);
    term.set_viewport_area(Rect::new(0, height - desired_height, width, desired_height));
    term.draw(|f| {
        chat.render(f.area(), f.buffer_mut());
    })
    .unwrap();
    assert_snapshot!(term.backend().vt100().screen().contents());
}
