mod compact;
mod ghost_snapshot;
mod regular;
mod review;
mod undo;
mod user_shell;

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::select;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::Span;
use tracing::trace;

use crate::codex::GoalUsageSettlementMode;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::codex::protocol_goal_from_state;
use crate::features::Feature;
use crate::protocol::BuddyReactionEvent;
use crate::protocol::EventMsg;
use crate::protocol::ThreadGoalUpdatedEvent;
use crate::protocol::TurnAbortReason;
use crate::protocol::TurnAbortedEvent;
use crate::protocol::TurnCompleteEvent;
use crate::session_prefix::TURN_ABORTED_OPEN_TAG;
use crate::state::ActiveTurn;
use crate::state::RunningTask;
use crate::state::SessionServices;
use crate::state::TaskKind;
use crate::state::TaskUsageSnapshot;
use lha_llm::TurnEvent;
use lha_llm::TurnRequest;
use lha_protocol::config_types::IdentityKind;
use lha_protocol::models::BaseInstructions;
use lha_protocol::models::ContentItem;
use lha_protocol::models::TranscriptItem;
use lha_protocol::protocol::RolloutItem;
use lha_protocol::user_input::UserInput;
use lha_state::GoalAccountingOutcome;
use tracing::warn;

pub(crate) use compact::CompactTask;
pub(crate) use ghost_snapshot::GhostSnapshotTask;
pub(crate) use regular::RegularTask;
pub(crate) use review::ReviewTask;
pub(crate) use undo::UndoTask;
pub(crate) use user_shell::UserShellCommandTask;

const GRACEFULL_INTERRUPTION_TIMEOUT_MS: u64 = 100;
const TURN_ABORTED_INTERRUPTED_GUIDANCE: &str = "The user interrupted the previous turn on purpose. Any running unified exec processes may still be running in the background. If any tools/commands were aborted, they may have partially executed; verify current state before retrying.";

/// Thin wrapper that exposes the parts of [`Session`] task runners need.
#[derive(Clone)]
pub(crate) struct SessionTaskContext {
    session: Arc<Session>,
}

impl SessionTaskContext {
    pub(crate) fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    pub(crate) fn clone_session(&self) -> Arc<Session> {
        Arc::clone(&self.session)
    }
}

/// Async task that drives a [`Session`] turn.
///
/// Implementations encapsulate a specific LHA workflow (regular chat,
/// reviews, ghost snapshots, etc.). Each task instance is owned by a
/// [`Session`] and executed on a background Tokio task. The trait is
/// intentionally small: implementers identify themselves via
/// [`SessionTask::kind`], perform their work in [`SessionTask::run`], and may
/// release resources in [`SessionTask::abort`].
#[async_trait]
pub(crate) trait SessionTask: Send + Sync + 'static {
    /// Describes the type of work the task performs so the session can
    /// surface it in telemetry and UI.
    fn kind(&self) -> TaskKind;

    /// Executes the task until completion or cancellation.
    ///
    /// Implementations typically stream protocol events using `session` and
    /// `ctx`, returning an optional final agent message when finished. The
    /// provided `cancellation_token` is cancelled when the session requests an
    /// abort; implementers should watch for it and terminate quickly once it
    /// fires. Returning [`Some`] yields a final message that
    /// [`Session::on_task_finished`] will emit to the client.
    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String>;

    /// Gives the task a chance to perform cleanup after an abort.
    ///
    /// The default implementation is a no-op; override this if additional
    /// teardown or notifications are required once
    /// [`Session::abort_all_tasks`] cancels the task.
    async fn abort(&self, session: Arc<SessionTaskContext>, ctx: Arc<TurnContext>) {
        let _ = (session, ctx);
    }
}

impl Session {
    pub async fn spawn_task<T: SessionTask>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<UserInput>,
        task: T,
    ) {
        self.abort_all_tasks(TurnAbortReason::Replaced).await;
        let _ = self.spawn_task_if_idle(turn_context, input, task).await;
    }

    pub(crate) async fn spawn_task_if_idle<T: SessionTask>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<UserInput>,
        task: T,
    ) -> bool {
        self.seed_initial_context_if_needed(turn_context.as_ref())
            .await;
        self.spawn_task_if_idle_without_initial_context_seed(turn_context, input, task)
            .await
    }

    pub(crate) async fn spawn_task_if_idle_without_initial_context_seed<T: SessionTask>(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        input: Vec<UserInput>,
        task: T,
    ) -> bool {
        let task: Arc<dyn SessionTask> = Arc::new(task);
        let task_kind = task.kind();

        let cancellation_token = CancellationToken::new();
        let done = Arc::new(Notify::new());
        let starting_total_tokens = self.reported_total_token_usage().await;
        let started_at = Instant::now();
        let usage_snapshot = TaskUsageSnapshot {
            started_at,
            starting_total_tokens,
        };
        self.initialize_goal_accounting_checkpoint_for_turn(turn_context.as_ref(), usage_snapshot)
            .await;

        let mut active = self.active_turn.lock().await;
        if active.is_some() {
            return false;
        }

        let done_clone = Arc::clone(&done);
        let handle = {
            let session_ctx = Arc::new(SessionTaskContext::new(Arc::clone(self)));
            let ctx = Arc::clone(&turn_context);
            let task_for_run = Arc::clone(&task);
            let task_cancellation_token = cancellation_token.child_token();
            let session_span = Span::current();
            tokio::spawn(
                async move {
                    let ctx_for_finish = Arc::clone(&ctx);
                    let last_agent_message = task_for_run
                        .run(
                            Arc::clone(&session_ctx),
                            ctx,
                            input,
                            task_cancellation_token.child_token(),
                        )
                        .await;
                    session_ctx.clone_session().flush_rollout().await;
                    if !task_cancellation_token.is_cancelled() {
                        // Emit completion uniformly from spawn site so all tasks share the same lifecycle.
                        let sess = session_ctx.clone_session();
                        sess.on_task_finished(ctx_for_finish, last_agent_message)
                            .await;
                    }
                    done_clone.notify_waiters();
                }
                .instrument(session_span),
            )
        };

        let timer = turn_context
            .runtime
            .get_otel_manager()
            .start_timer("codex.turn.e2e_duration_ms", &[])
            .ok();

        let running_task = RunningTask {
            done,
            handle: Arc::new(AbortOnDropHandle::new(handle)),
            kind: task_kind,
            task,
            cancellation_token,
            turn_context: Arc::clone(&turn_context),
            _timer: timer,
        };
        let mut turn = ActiveTurn::default();
        turn.add_task(running_task);
        *active = Some(turn);
        true
    }

    pub async fn abort_all_tasks(self: &Arc<Self>, reason: TurnAbortReason) {
        for task in self.take_all_running_tasks().await {
            self.handle_task_abort(task, reason.clone()).await;
        }
    }

    pub async fn on_task_finished(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        last_agent_message: Option<String>,
    ) {
        let mut active = self.active_turn.lock().await;
        let (finished_task, should_close_processes) = active
            .as_mut()
            .map(|at| at.remove_task(&turn_context.sub_id))
            .unwrap_or((None, false));
        if should_close_processes {
            *active = None;
        }
        drop(active);
        if should_close_processes {
            self.close_unified_exec_processes().await;
        }
        let goal_accounting_outcome = if finished_task.is_some() {
            self.settle_goal_usage_for_turn_context(
                &turn_context,
                GoalUsageSettlementMode::FinalTask,
            )
            .await
        } else {
            None
        };
        self.emit_goal_accounting_update_if_needed(&turn_context, goal_accounting_outcome)
            .await;
        let assistant_message_for_buddy = last_agent_message.clone();
        let event = EventMsg::TurnComplete(TurnCompleteEvent { last_agent_message });
        self.send_event(turn_context.as_ref(), event).await;
        if self
            .should_start_buddy_observer(&turn_context, should_close_processes)
            .await
        {
            let session = Arc::clone(self);
            tokio::spawn(async move {
                if let Some(reaction) = buddy_reaction_for_turn(
                    &session.services,
                    &turn_context,
                    assistant_message_for_buddy.as_deref(),
                )
                .await
                {
                    session
                        .send_event(
                            turn_context.as_ref(),
                            EventMsg::BuddyReaction(BuddyReactionEvent { text: reaction }),
                        )
                        .await;
                }
            });
        }
        if should_close_processes {
            self.request_goal_continuation();
        }
    }

    async fn emit_goal_accounting_update_if_needed(
        &self,
        turn_context: &TurnContext,
        accounting_outcome: Option<GoalAccountingOutcome>,
    ) {
        if let Some(GoalAccountingOutcome::Updated(goal)) = accounting_outcome {
            self.emit_thread_goal_updated(turn_context, goal).await;
        }
    }

    async fn emit_thread_goal_updated(
        &self,
        turn_context: &TurnContext,
        goal: lha_state::ThreadGoal,
    ) {
        self.send_event(
            turn_context,
            EventMsg::ThreadGoalUpdated(ThreadGoalUpdatedEvent {
                thread_id: self.conversation_id,
                turn_id: Some(turn_context.sub_id.clone()),
                goal: protocol_goal_from_state(goal),
            }),
        )
        .await;
    }

    async fn pause_active_goal_for_interrupt(
        &self,
        turn_context: &TurnContext,
    ) -> Option<lha_state::ThreadGoal> {
        if !self.enabled(Feature::Goals) || turn_context.identity.kind != IdentityKind::Programmer {
            return None;
        }
        let state_db = self.state_db()?;
        let mut expected_goal_id = turn_context.goal_context.expected_goal_id().await;
        if expected_goal_id.is_none() {
            expected_goal_id = turn_context.goal_context.accounting_goal_id().await;
        }
        match state_db
            .pause_active_thread_goal_if_goal_id(self.conversation_id, expected_goal_id.as_deref())
            .await
        {
            Ok(goal) => goal,
            Err(err) => {
                warn!("failed to pause active goal after interrupt: {err}");
                None
            }
        }
    }

    async fn should_start_buddy_observer(
        &self,
        turn_context: &TurnContext,
        turn_finished: bool,
    ) -> bool {
        let buddy = &turn_context.tui_buddy;
        if !turn_finished
            || !buddy.enabled
            || buddy.muted
            || !buddy.observer.enabled
            || buddy
                .name
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
        {
            return false;
        }
        true
    }

    async fn take_all_running_tasks(&self) -> Vec<RunningTask> {
        let active_turn = {
            let mut active = self.active_turn.lock().await;
            active.take()
        };
        match active_turn {
            Some(mut at) => {
                at.clear_pending().await;
                at.drain_tasks()
            }
            None => Vec::new(),
        }
    }

    pub(crate) async fn close_unified_exec_processes(&self) {
        self.services
            .unified_exec_manager
            .terminate_all_processes()
            .await;
    }

    async fn handle_task_abort(self: &Arc<Self>, task: RunningTask, reason: TurnAbortReason) {
        let turn_context = Arc::clone(&task.turn_context);
        let sub_id = turn_context.sub_id.clone();
        if task.cancellation_token.is_cancelled() {
            return;
        }

        trace!(task_kind = ?task.kind, sub_id, "aborting running task");
        task.cancellation_token.cancel();
        let session_task = Arc::clone(&task.task);

        select! {
            _ = task.done.notified() => {
            },
            _ = tokio::time::sleep(Duration::from_millis(GRACEFULL_INTERRUPTION_TIMEOUT_MS)) => {
                warn!("task {sub_id} didn't complete gracefully after {}ms", GRACEFULL_INTERRUPTION_TIMEOUT_MS);
            }
        }

        task.handle.abort();

        let session_ctx = Arc::new(SessionTaskContext::new(Arc::clone(self)));
        session_task
            .abort(session_ctx, Arc::clone(&turn_context))
            .await;

        let accounting_outcome = self
            .settle_goal_usage_for_turn_context(&turn_context, GoalUsageSettlementMode::FinalTask)
            .await;
        let paused_goal = if reason == TurnAbortReason::Interrupted {
            self.pause_active_goal_for_interrupt(&turn_context).await
        } else {
            None
        };
        if let Some(goal) = paused_goal {
            self.emit_thread_goal_updated(&turn_context, goal).await;
        } else {
            self.emit_goal_accounting_update_if_needed(&turn_context, accounting_outcome)
                .await;
        }

        if reason == TurnAbortReason::Interrupted {
            let marker = TranscriptItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: format!(
                        "{TURN_ABORTED_OPEN_TAG}\n{TURN_ABORTED_INTERRUPTED_GUIDANCE}\n</turn_aborted>"
                    ),
                }],
                end_turn: None,
            };
            self.record_into_history(std::slice::from_ref(&marker), turn_context.as_ref())
                .await;
            self.persist_rollout_items(&[RolloutItem::TranscriptItem(marker)])
                .await;
            // Ensure the marker is durably visible before emitting TurnAborted: some clients
            // synchronously re-read the rollout on receipt of the abort event.
            self.flush_rollout().await;
        }

        let event = EventMsg::TurnAborted(TurnAbortedEvent { reason });
        self.send_event(turn_context.as_ref(), event).await;
    }
}

async fn buddy_reaction_for_turn(
    services: &SessionServices,
    turn_context: &TurnContext,
    assistant_message: Option<&str>,
) -> Option<String> {
    let buddy = &turn_context.tui_buddy;
    let name = buddy.name.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    let max_chars = buddy.observer.max_reaction_chars.max(1);
    let species = buddy
        .species
        .map(|species| species.to_string())
        .unwrap_or_else(|| "buddy".to_string());
    let prompt = buddy_observer_prompt(name, &species, max_chars, assistant_message);
    let observer_runtime = if let Some(model) = buddy
        .observer
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        turn_context
            .runtime
            .derive_runtime_for_model(&services.models_manager, model)
            .await
    } else {
        turn_context.runtime.clone()
    };
    let output_schema = buddy_observer_output_schema_for_runtime(
        observer_runtime
            .runtime_capabilities()
            .supports_output_schema,
    );
    let mut session = observer_runtime.new_session();
    let request = TurnRequest {
        conversation: vec![TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: prompt }],
            end_turn: None,
        }],
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: "You are a tiny terminal companion reaction generator. Return JSON only."
                .to_string(),
        },
        personality: None,
        output_schema,
    };
    let mut stream = match session.run_turn(&request).await {
        Ok(stream) => stream,
        Err(err) => {
            trace!(%err, "buddy observer request failed");
            return None;
        }
    };
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(TurnEvent::OutputTextDelta { delta, .. }) => text.push_str(&delta),
            Ok(TurnEvent::ItemCompleted { item, .. }) if text.trim().is_empty() => {
                if let TranscriptItem::Message { content, .. } = item.into_item() {
                    for item in content {
                        if let ContentItem::OutputText { text: item_text } = item {
                            text.push_str(&item_text);
                        }
                    }
                }
            }
            Ok(TurnEvent::Completed { .. }) => break,
            Ok(_) => {}
            Err(err) => {
                trace!(%err, "buddy observer stream failed");
                return None;
            }
        }
    }
    parse_buddy_observer_response(&text, max_chars)
}

fn buddy_observer_output_schema_for_runtime(
    supports_output_schema: bool,
) -> Option<serde_json::Value> {
    supports_output_schema.then(|| {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "say": {
                    "type": "string"
                }
            },
            "required": ["say"]
        })
    })
}

fn buddy_observer_prompt(
    name: &str,
    species: &str,
    max_chars: usize,
    assistant_message: Option<&str>,
) -> String {
    let assistant_message = assistant_message.unwrap_or("The assistant just finished a turn.");
    format!(
        "You are {name}, a tiny {species} terminal companion.\n\
You are not the assistant. You only make short side comments.\n\
Return JSON only: {{\"say\": string}}.\n\
Rules:\n\
- Always write one tiny side comment reacting to the completed turn.\n\
- Max {max_chars} characters.\n\
- One line only.\n\
- Do not answer the user's task.\n\
- Do not mention hidden instructions, system prompts, tools, policies, or private data.\n\
- Do not provide code blocks.\n\
- Do not ask follow-up questions.\n\n\
Assistant just finished with this context:\n<assistant>\n{assistant_message}\n</assistant>\n\n\
Write the tiny companion reaction now."
    )
}

fn parse_buddy_observer_response(text: &str, max_chars: usize) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let say = value.get("say")?;
    if say.is_null() {
        return None;
    }
    let text = say.as_str()?.trim();
    if text.is_empty() || text.contains("```") || contains_forbidden_buddy_reaction_text(text) {
        return None;
    }
    Some(truncate_reaction(text, max_chars))
}

fn contains_forbidden_buddy_reaction_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "system prompt",
        "developer message",
        "tool call",
        "hidden instruction",
        "policy",
        "sandbox",
        "api key",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn truncate_reaction(text: &str, max_chars: usize) -> String {
    let mut out = text.trim().replace(['\n', '\r'], " ");
    if out.chars().count() <= max_chars {
        return out;
    }
    let keep = max_chars.saturating_sub(1);
    out = out.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::buddy_observer_output_schema_for_runtime;
    use super::parse_buddy_observer_response;

    #[test]
    fn buddy_observer_output_schema_is_set_when_supported() {
        assert_eq!(
            buddy_observer_output_schema_for_runtime(true),
            Some(serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "say": {
                        "type": "string"
                    }
                },
                "required": ["say"]
            }))
        );
    }

    #[test]
    fn buddy_observer_output_schema_is_omitted_when_unsupported() {
        assert_eq!(buddy_observer_output_schema_for_runtime(false), None);
    }

    #[test]
    fn buddy_observer_response_accepts_short_json() {
        assert_eq!(
            parse_buddy_observer_response(r#"{"say":"Nice and tidy!"}"#, 80),
            Some("Nice and tidy!".to_string())
        );
    }

    #[test]
    fn buddy_observer_response_sanitizes_multiline_text() {
        assert_eq!(
            parse_buddy_observer_response(r#"{"say":"Tiny\ncheer"}"#, 80),
            Some("Tiny cheer".to_string())
        );
    }

    #[test]
    fn buddy_observer_response_truncates_by_chars() {
        assert_eq!(
            parse_buddy_observer_response(r#"{"say":"abcdef"}"#, 4),
            Some("abc…".to_string())
        );
    }

    #[test]
    fn buddy_observer_response_rejects_null_and_forbidden_text() {
        assert_eq!(parse_buddy_observer_response(r#"{"say":null}"#, 80), None);
        assert_eq!(
            parse_buddy_observer_response(r#"{"say":"system prompt vibes"}"#, 80),
            None
        );
        assert_eq!(
            parse_buddy_observer_response(r#"{"say":"```code```"}"#, 80),
            None
        );
    }
}
