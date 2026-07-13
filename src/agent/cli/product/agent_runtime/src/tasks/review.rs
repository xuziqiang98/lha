use std::sync::Arc;
use std::time::Duration;

use crate::product::protocol::items::TurnItem;
use crate::product::protocol::models::ContentItem;
use crate::product::protocol::models::TranscriptItem;
use crate::product::protocol::protocol::EventMsg;
use crate::product::protocol::protocol::ExitedReviewModeEvent;
use crate::product::protocol::protocol::ReviewOutputEvent;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::product::agent::agent_jobs::AgentJobExecConfig;
use crate::product::agent::agent_jobs::AgentJobSpawnOptions;
use crate::product::agent::agent_jobs::AgentJobStatus;
use crate::product::agent::codex::Session;
use crate::product::agent::codex::TurnContext;
use crate::product::agent::review_format::format_review_findings_block;
use crate::product::agent::review_format::render_review_output_text;
use crate::product::agent::state::TaskKind;
use crate::product::protocol::user_input::UserInput;

use super::SessionTask;
use super::SessionTaskContext;

#[derive(Clone, Copy)]
pub(crate) struct ReviewTask;

impl ReviewTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[derive(Default)]
struct ReviewProgressForwarder {
    saw_canonical_reasoning: bool,
}

impl ReviewProgressForwarder {
    fn should_forward(&mut self, msg: &EventMsg) -> bool {
        if is_canonical_reasoning_progress_event(msg) {
            self.saw_canonical_reasoning = true;
            return true;
        }

        if self.saw_canonical_reasoning && is_legacy_reasoning_progress_event(msg) {
            return false;
        }

        is_forwardable_review_progress_event(msg)
    }
}

#[async_trait]
impl SessionTask for ReviewTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Review
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let _ = session
            .session
            .services
            .otel_manager
            .counter("codex.task.review", 1, &[]);

        let output = run_review_job(
            session.clone(),
            ctx.clone(),
            input,
            cancellation_token.clone(),
        )
        .await;
        if !cancellation_token.is_cancelled() {
            exit_review_mode(session.clone_session(), output.clone(), ctx.clone()).await;
        }
        None
    }

    async fn abort(&self, session: Arc<SessionTaskContext>, ctx: Arc<TurnContext>) {
        exit_review_mode(session.clone_session(), None, ctx).await;
    }
}

async fn run_review_job(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    input: Vec<UserInput>,
    cancellation_token: CancellationToken,
) -> Option<ReviewOutputEvent> {
    let prompt = input
        .into_iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text),
            UserInput::LocalImage { .. } => None,
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let config = ctx.runtime.config();
    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| ctx.runtime.get_model());
    let exec_config = AgentJobExecConfig::from_runtime(
        &ctx.runtime,
        &model,
        ctx.sandbox_policy.clone(),
        ctx.windows_sandbox_level,
    );
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    // Review model work runs in an isolated CLI-backed job; this task only
    // starts the job, waits for its final result, and folds that result back
    // into the parent session.
    let job = match session
        .session
        .services
        .agent_jobs
        .spawn(
            session.session.conversation_id,
            crate::product::agent::agent_jobs::AgentJobType::Reviewer,
            prompt,
            ctx.cwd.clone(),
            exec_config,
            AgentJobSpawnOptions::raw_events(None, progress_tx),
        )
        .await
    {
        Ok(job) => job,
        Err(err) => {
            return Some(review_failure_output(format!(
                "Review failed to start: {err}"
            )));
        }
    };
    session
        .session
        .send_event(ctx.as_ref(), job.status_event())
        .await;
    let mut progress_forwarder = ReviewProgressForwarder::default();
    let mut progress_closed = false;
    loop {
        tokio::select! {
            maybe_msg = progress_rx.recv(), if !progress_closed => {
                if let Some(msg) = maybe_msg {
                    forward_review_progress_event(&session, &ctx, &mut progress_forwarder, msg).await;
                } else {
                    progress_closed = true;
                }
            }
            () = tokio::time::sleep(Duration::from_millis(100)) => {
                let snapshot = session.session.services.agent_jobs.status(&job.id).await;
                if snapshot.status.is_final() {
                    session
                        .session
                        .send_event(ctx.as_ref(), snapshot.status_event())
                        .await;
                }
                match snapshot.status {
                    AgentJobStatus::Completed { result, .. } => {
                        drain_review_progress_events(
                            &session,
                            &ctx,
                            &mut progress_forwarder,
                            &mut progress_rx,
                        )
                        .await;
                        return Some(parse_review_output_event(&result));
                    }
                    AgentJobStatus::Failed { message, .. } => {
                        drain_review_progress_events(
                            &session,
                            &ctx,
                            &mut progress_forwarder,
                            &mut progress_rx,
                        )
                        .await;
                        let message = message.trim();
                        let message = if message.is_empty() {
                            "Review failed without error output.".to_string()
                        } else {
                            format!("Review failed: {message}")
                        };
                        return Some(review_failure_output(message));
                    }
                    AgentJobStatus::TimedOut => {
                        return Some(review_failure_output(
                            "Review timed out before producing a result.",
                        ));
                    }
                    AgentJobStatus::Cancelled => {
                        return Some(review_failure_output(
                            "Review was cancelled before producing a result.",
                        ));
                    }
                    AgentJobStatus::NotFound => {
                        return Some(review_failure_output(
                            "Review job disappeared before producing a result.",
                        ));
                    }
                    AgentJobStatus::Running => {}
                }
            }
            () = cancellation_token.cancelled() => break,
        }
    }
    let snapshot = session.session.services.agent_jobs.close(&job.id).await;
    session
        .session
        .send_event(ctx.as_ref(), snapshot.status_event())
        .await;
    None
}

async fn drain_review_progress_events(
    session: &Arc<SessionTaskContext>,
    ctx: &Arc<TurnContext>,
    forwarder: &mut ReviewProgressForwarder,
    progress_rx: &mut mpsc::UnboundedReceiver<EventMsg>,
) {
    while let Ok(msg) = progress_rx.try_recv() {
        forward_review_progress_event(session, ctx, forwarder, msg).await;
    }
}

async fn forward_review_progress_event(
    session: &Arc<SessionTaskContext>,
    ctx: &Arc<TurnContext>,
    forwarder: &mut ReviewProgressForwarder,
    msg: EventMsg,
) {
    if forwarder.should_forward(&msg) {
        session.session.send_event(ctx.as_ref(), msg).await;
    }
}

fn is_canonical_reasoning_progress_event(msg: &EventMsg) -> bool {
    matches!(
        msg,
        EventMsg::ReasoningContentDelta(_)
            | EventMsg::ReasoningRawContentDelta(_)
            | EventMsg::ItemCompleted(crate::product::protocol::protocol::ItemCompletedEvent {
                item: TurnItem::Reasoning(_),
                ..
            })
    )
}

fn is_legacy_reasoning_progress_event(msg: &EventMsg) -> bool {
    matches!(
        msg,
        EventMsg::AgentReasoningDelta(_)
            | EventMsg::AgentReasoning(_)
            | EventMsg::AgentReasoningRawContentDelta(_)
            | EventMsg::AgentReasoningRawContent(_)
    )
}

fn is_forwardable_review_progress_event(msg: &EventMsg) -> bool {
    matches!(
        msg,
        EventMsg::ReasoningContentDelta(_)
            | EventMsg::ReasoningRawContentDelta(_)
            | EventMsg::ItemCompleted(crate::product::protocol::protocol::ItemCompletedEvent {
                item: TurnItem::Reasoning(_),
                ..
            })
            | EventMsg::AgentReasoningDelta(_)
            | EventMsg::AgentReasoning(_)
            | EventMsg::AgentReasoningSectionBreak(_)
            | EventMsg::AgentReasoningRawContentDelta(_)
            | EventMsg::AgentReasoningRawContent(_)
            | EventMsg::ExecCommandBegin(_)
            | EventMsg::ExecCommandOutputDelta(_)
            | EventMsg::TerminalInteraction(_)
            | EventMsg::ExecCommandEnd(_)
            | EventMsg::PatchApplyBegin(_)
            | EventMsg::PatchApplyEnd(_)
            | EventMsg::McpToolCallBegin(_)
            | EventMsg::McpToolCallEnd(_)
            | EventMsg::WebSearchBegin(_)
            | EventMsg::WebSearchEnd(_)
            | EventMsg::ViewImageToolCall(_)
            | EventMsg::Warning(_)
            | EventMsg::StreamError(_)
            | EventMsg::BackgroundEvent(_)
            | EventMsg::TokenCount(_)
    )
}

fn review_failure_output(message: impl Into<String>) -> ReviewOutputEvent {
    ReviewOutputEvent {
        overall_explanation: message.into(),
        ..Default::default()
    }
}

/// Parse a ReviewOutputEvent from a text blob returned by the reviewer model.
/// If the text is valid JSON matching ReviewOutputEvent, deserialize it.
/// Otherwise, attempt to extract the first JSON object substring and parse it.
/// If parsing still fails, return a structured fallback carrying the plain text
/// in `overall_explanation`.
fn parse_review_output_event(text: &str) -> ReviewOutputEvent {
    if let Ok(ev) = serde_json::from_str::<ReviewOutputEvent>(text) {
        return ev;
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}'))
        && start < end
        && let Some(slice) = text.get(start..=end)
        && let Ok(ev) = serde_json::from_str::<ReviewOutputEvent>(slice)
    {
        return ev;
    }
    ReviewOutputEvent {
        overall_explanation: text.to_string(),
        ..Default::default()
    }
}

/// Emits an ExitedReviewMode Event with optional ReviewOutput,
/// and records a developer message with the review output.
pub(crate) async fn exit_review_mode(
    session: Arc<Session>,
    review_output: Option<ReviewOutputEvent>,
    ctx: Arc<TurnContext>,
) {
    const REVIEW_USER_MESSAGE_ID: &str = "review_rollout_user";
    const REVIEW_ASSISTANT_MESSAGE_ID: &str = "review_rollout_assistant";
    let (user_message, assistant_message) = if let Some(out) = review_output.clone() {
        let mut findings_str = String::new();
        let text = out.overall_explanation.trim();
        if !text.is_empty() {
            findings_str.push_str(text);
        }
        if !out.findings.is_empty() {
            let block = format_review_findings_block(&out.findings, None);
            findings_str.push_str(&format!("\n{block}"));
        }
        let rendered = crate::product::agent::client_common::REVIEW_EXIT_SUCCESS_TMPL
            .replace("{results}", &findings_str);
        let assistant_message = render_review_output_text(&out);
        (rendered, assistant_message)
    } else {
        let rendered =
            crate::product::agent::client_common::REVIEW_EXIT_INTERRUPTED_TMPL.to_string();
        let assistant_message =
            "Review was interrupted. Please re-run /review and wait for it to complete."
                .to_string();
        (rendered, assistant_message)
    };

    session
        .record_conversation_items(
            &ctx,
            &[TranscriptItem::Message {
                id: Some(REVIEW_USER_MESSAGE_ID.to_string()),
                role: "user".to_string(),
                content: vec![ContentItem::InputText { text: user_message }],
                end_turn: None,
            }],
        )
        .await;
    session
        .send_event(
            ctx.as_ref(),
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent { review_output }),
        )
        .await;
    session
        .record_response_item_and_emit_turn_item(
            ctx.as_ref(),
            TranscriptItem::Message {
                id: Some(REVIEW_ASSISTANT_MESSAGE_ID.to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: assistant_message,
                }],
                end_turn: None,
            },
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn review_failure_output_returns_user_visible_explanation() {
        let output = review_failure_output("Review failed: boom");

        assert_eq!(
            output,
            ReviewOutputEvent {
                overall_explanation: "Review failed: boom".to_string(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn parse_review_output_event_falls_back_to_plain_text() {
        let output = parse_review_output_event("plain failure text");

        assert_eq!(
            output,
            ReviewOutputEvent {
                overall_explanation: "plain failure text".to_string(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn review_progress_forwarder_prefers_canonical_reasoning() {
        let mut forwarder = ReviewProgressForwarder::default();
        let thread_id = crate::product::protocol::ThreadId::new();
        let events = [
            EventMsg::ReasoningContentDelta(
                crate::product::protocol::protocol::ReasoningContentDeltaEvent {
                    thread_id: thread_id.to_string(),
                    turn_id: "turn-1".into(),
                    item_id: "reasoning-1".into(),
                    delta: "summary".into(),
                    summary_index: 0,
                },
            ),
            EventMsg::ReasoningRawContentDelta(
                crate::product::protocol::protocol::ReasoningRawContentDeltaEvent {
                    thread_id: thread_id.to_string(),
                    turn_id: "turn-1".into(),
                    item_id: "reasoning-1".into(),
                    delta: "raw detail".into(),
                    content_index: 0,
                },
            ),
            EventMsg::ItemCompleted(crate::product::protocol::protocol::ItemCompletedEvent {
                thread_id,
                turn_id: "turn-1".into(),
                item: TurnItem::Reasoning(crate::product::protocol::items::ReasoningItem {
                    id: "reasoning-1".into(),
                    summary_text: vec!["summary".into()],
                    raw_content: vec!["raw detail".into()],
                }),
            }),
            EventMsg::AgentReasoning(crate::product::protocol::protocol::AgentReasoningEvent {
                text: "summary".into(),
            }),
            EventMsg::AgentReasoningRawContent(
                crate::product::protocol::protocol::AgentReasoningRawContentEvent {
                    text: "raw detail".into(),
                },
            ),
        ];

        let forwarded = events
            .iter()
            .map(|event| forwarder.should_forward(event))
            .collect::<Vec<_>>();
        assert_eq!(forwarded, vec![true, true, true, false, false]);
    }

    #[test]
    fn review_progress_forwarder_keeps_legacy_only_reasoning() {
        let mut forwarder = ReviewProgressForwarder::default();
        let legacy =
            EventMsg::AgentReasoning(crate::product::protocol::protocol::AgentReasoningEvent {
                text: "summary".into(),
            });

        assert!(forwarder.should_forward(&legacy));
    }
}
