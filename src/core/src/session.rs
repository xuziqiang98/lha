use crate::Error;
use crate::Result;
use crate::RunCollectTextError;
use crate::builder::AgentDefinition;
use crate::events::AgentEvent;
use crate::events::TurnItemDelta;
use crate::events::TurnSummary;
use crate::input::InputQueue;
use crate::input::SessionInput;
use crate::kernel::AgentKernel;
use crate::processor::SessionTurnProcessor;
use crate::processor::outcome_summary;
use crate::snapshot::ActiveTurnSnapshot;
use crate::snapshot::SessionSnapshot;
use crate::status::SessionStatus;
use crate::tools::ToolExecutor;
use async_channel::Receiver;
use async_channel::Sender;
use lha_llm::BaseInstructions;
use lha_llm::SemanticRuntimeSession;
use lha_llm::ToolDescriptor;
use lha_llm::TranscriptItem;
use lha_llm::TurnRequest;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub type SessionId = u64;
pub type SubmissionId = u64;

#[derive(Clone)]
pub struct AgentSession {
    inner: Arc<AgentSessionInner>,
}

pub(crate) struct AgentSessionInner {
    pub(crate) session_id: SessionId,
    definition: Arc<AgentDefinition>,
    runtime_session: Mutex<Box<dyn SemanticRuntimeSession>>,
    state: Mutex<SessionState>,
    tx_event: Sender<AgentEvent>,
    rx_event: Receiver<AgentEvent>,
    next_submission_id: AtomicU64,
}

struct SessionState {
    conversation: Vec<TranscriptItem>,
    steering_queue: VecDeque<SessionInput>,
    follow_up_queue: VecDeque<SessionInput>,
    status: SessionStatus,
    active_turn: Option<ActiveTurnState>,
}

struct ActiveTurnState {
    submission_id: SubmissionId,
    cancellation_token: CancellationToken,
}

impl AgentSession {
    pub(crate) fn new(
        session_id: SessionId,
        definition: Arc<AgentDefinition>,
        conversation: Vec<TranscriptItem>,
    ) -> Self {
        let (tx_event, rx_event) = async_channel::unbounded();
        let inner = Arc::new(AgentSessionInner {
            session_id,
            runtime_session: Mutex::new(definition.runtime.new_session()),
            definition,
            state: Mutex::new(SessionState {
                conversation,
                steering_queue: VecDeque::new(),
                follow_up_queue: VecDeque::new(),
                status: SessionStatus::Idle,
                active_turn: None,
            }),
            tx_event,
            rx_event,
            next_submission_id: AtomicU64::new(1),
        });

        let _ = inner
            .tx_event
            .try_send(AgentEvent::SessionStarted { session_id });
        let _ = inner.tx_event.try_send(AgentEvent::SessionStatusChanged {
            session_id,
            status: SessionStatus::Idle,
        });

        Self { inner }
    }

    pub fn id(&self) -> SessionId {
        self.inner.session_id
    }

    pub async fn run(&self, input: SessionInput) -> Result<SubmissionId> {
        let items = input.into_items();
        self.inner
            .spawn_turn_loop(items.clone(), Some((InputQueue::Primary, items)))
            .await
    }

    pub async fn continue_turn(&self) -> Result<SubmissionId> {
        {
            let state = self.inner.state.lock().await;
            if state.active_turn.is_some() {
                return Err(Error::SessionBusy);
            }
            if state.conversation.is_empty() && !state.has_bootstrap_input() {
                return Err(Error::EmptyConversation);
            }
            if !state.has_bootstrap_input() && !can_continue_from_history(&state.conversation) {
                return Err(Error::InvalidContinuation);
            }
        }
        self.inner.spawn_turn_loop(Vec::new(), None).await
    }

    pub async fn steer(&self, input: SessionInput) {
        let items = input.items().to_vec();
        {
            let mut state = self.inner.state.lock().await;
            state.steering_queue.push_back(input);
        }
        self.inner
            .emit_event(AgentEvent::InputQueued {
                session_id: self.inner.session_id,
                queue: InputQueue::Steering,
                items,
            })
            .await;
    }

    pub async fn follow_up(&self, input: SessionInput) {
        let items = input.items().to_vec();
        {
            let mut state = self.inner.state.lock().await;
            state.follow_up_queue.push_back(input);
        }
        self.inner
            .emit_event(AgentEvent::InputQueued {
                session_id: self.inner.session_id,
                queue: InputQueue::FollowUp,
                items,
            })
            .await;
    }

    pub async fn abort_current_turn(&self) -> bool {
        let cancellation_token = {
            let mut state = self.inner.state.lock().await;
            let Some(cancellation_token) = state
                .active_turn
                .as_ref()
                .map(|active_turn| active_turn.cancellation_token.clone())
            else {
                return false;
            };
            state.status = SessionStatus::Aborting;
            cancellation_token
        };
        self.inner
            .emit_event(AgentEvent::SessionStatusChanged {
                session_id: self.inner.session_id,
                status: SessionStatus::Aborting,
            })
            .await;
        cancellation_token.cancel();
        true
    }

    pub async fn next_event(&self) -> Result<AgentEvent> {
        self.inner
            .rx_event
            .recv()
            .await
            .map_err(|_| Error::EventChannelClosed)
    }

    pub async fn run_collect_text(
        &self,
        input: impl Into<SessionInput>,
    ) -> std::result::Result<String, RunCollectTextError> {
        let submission_id = self.run(input.into()).await?;
        let mut current_turn_streamed_text = String::new();
        let mut current_turn_saw_output_text_delta = false;
        let mut terminal_result = None;

        loop {
            match self.next_event().await? {
                AgentEvent::TurnStarted {
                    submission_id: event_submission_id,
                    ..
                } if event_submission_id == submission_id && terminal_result.is_none() => {
                    current_turn_streamed_text.clear();
                    current_turn_saw_output_text_delta = false;
                }
                AgentEvent::OutputItemDelta {
                    submission_id: event_submission_id,
                    delta: TurnItemDelta::OutputText { delta },
                    ..
                } if event_submission_id == submission_id && terminal_result.is_none() => {
                    current_turn_saw_output_text_delta = true;
                    current_turn_streamed_text.push_str(&delta);
                }
                AgentEvent::TurnCompleted {
                    submission_id: event_submission_id,
                    outcome,
                    ..
                } if event_submission_id == submission_id && terminal_result.is_none() => {
                    if !outcome.needs_follow_up {
                        let final_text = if current_turn_saw_output_text_delta {
                            current_turn_streamed_text.clone()
                        } else {
                            outcome.last_agent_message.unwrap_or_default()
                        };
                        terminal_result = Some(Ok(final_text));
                    }
                }
                AgentEvent::TurnFailed {
                    submission_id: event_submission_id,
                    error,
                    ..
                } if event_submission_id == submission_id && terminal_result.is_none() => {
                    terminal_result = Some(Err(RunCollectTextError::TurnFailed(error)));
                }
                AgentEvent::TurnAborted {
                    submission_id: event_submission_id,
                    ..
                } if event_submission_id == submission_id && terminal_result.is_none() => {
                    terminal_result = Some(Err(Error::Aborted.into()));
                }
                AgentEvent::SessionStatusChanged {
                    status: SessionStatus::Idle,
                    ..
                } if terminal_result.is_some() => {
                    if let Some(result) = terminal_result.take() {
                        return result;
                    }
                }
                _ => {}
            }
        }
    }

    pub async fn status(&self) -> SessionStatus {
        let state = self.inner.state.lock().await;
        state.status
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        self.inner.snapshot().await
    }
}

impl AgentSessionInner {
    pub(crate) async fn emit_event(&self, event: AgentEvent) {
        let _ = self.tx_event.send(event).await;
    }

    pub(crate) async fn push_conversation_item(&self, item: TranscriptItem) {
        let mut state = self.state.lock().await;
        state.conversation.push(item);
    }

    async fn snapshot(&self) -> SessionSnapshot {
        let state = self.state.lock().await;
        SessionSnapshot {
            session_id: self.session_id,
            status: state.status,
            conversation: state.conversation.clone(),
            steering_queue: state
                .steering_queue
                .iter()
                .map(|input| input.items().to_vec())
                .collect(),
            follow_up_queue: state
                .follow_up_queue
                .iter()
                .map(|input| input.items().to_vec())
                .collect(),
            runtime: self.definition.runtime_metadata.clone(),
            active_turn: state.active_turn.as_ref().map(|active| ActiveTurnSnapshot {
                submission_id: active.submission_id,
            }),
        }
    }

    async fn spawn_turn_loop(
        self: &Arc<Self>,
        initial_items: Vec<TranscriptItem>,
        queued_event: Option<(InputQueue, Vec<TranscriptItem>)>,
    ) -> Result<SubmissionId> {
        let cancellation_token = CancellationToken::new();

        let submission_id = {
            let mut state = self.state.lock().await;
            if state.active_turn.is_some() {
                return Err(Error::SessionBusy);
            }
            let submission_id = self.next_submission_id.fetch_add(1, Ordering::SeqCst);
            state.status = SessionStatus::Running;
            state.active_turn = Some(ActiveTurnState {
                submission_id,
                cancellation_token: cancellation_token.clone(),
            });
            submission_id
        };

        if let Some((queue, items)) = queued_event {
            self.emit_event(AgentEvent::InputQueued {
                session_id: self.session_id,
                queue,
                items,
            })
            .await;
        }

        self.emit_event(AgentEvent::SessionStatusChanged {
            session_id: self.session_id,
            status: SessionStatus::Running,
        })
        .await;

        let session = Arc::clone(self);
        tokio::spawn(async move {
            session
                .drive_turn_loop(submission_id, initial_items, cancellation_token)
                .await;
        });

        Ok(submission_id)
    }

    async fn drive_turn_loop(
        self: Arc<Self>,
        submission_id: SubmissionId,
        mut initial_items: Vec<TranscriptItem>,
        cancellation_token: CancellationToken,
    ) {
        loop {
            if cancellation_token.is_cancelled() {
                self.emit_event(AgentEvent::TurnAborted {
                    session_id: self.session_id,
                    submission_id,
                })
                .await;
                break;
            }

            if !initial_items.is_empty() {
                self.append_items(std::mem::take(&mut initial_items)).await;
            } else if !self.drain_bootstrap_inputs().await {
                let state = self.state.lock().await;
                if state.conversation.is_empty() {
                    drop(state);
                    self.emit_event(AgentEvent::TurnFailed {
                        session_id: self.session_id,
                        submission_id,
                        error: Error::EmptyConversation.to_string(),
                    })
                    .await;
                    break;
                }
            }

            self.emit_event(AgentEvent::TurnStarted {
                session_id: self.session_id,
                submission_id,
            })
            .await;

            let request = match self.build_turn_request().await {
                Ok(request) => request,
                Err(err) => {
                    self.emit_event(AgentEvent::TurnFailed {
                        session_id: self.session_id,
                        submission_id,
                        error: err.to_string(),
                    })
                    .await;
                    break;
                }
            };
            let stream = {
                let mut runtime_session = self.runtime_session.lock().await;
                runtime_session.run_turn(&request).await
            };

            let result = match stream {
                Ok(stream) => {
                    let processor = SessionTurnProcessor::new(
                        Arc::clone(&self),
                        submission_id,
                        ToolExecutor::new(Arc::clone(&self.definition.tools)),
                        cancellation_token.child_token(),
                    );
                    AgentKernel::new()
                        .run_turn(stream, processor, cancellation_token.child_token())
                        .await
                        .map(outcome_summary)
                }
                Err(err) => Err(Error::Runtime(err)),
            };
            let result = if cancellation_token.is_cancelled() && result.is_err() {
                Err(Error::Aborted)
            } else {
                result
            };

            match result {
                Ok(outcome) => {
                    self.emit_event(AgentEvent::TurnCompleted {
                        session_id: self.session_id,
                        submission_id,
                        outcome: TurnSummary {
                            needs_follow_up: outcome.needs_follow_up,
                            last_agent_message: outcome.last_agent_message.clone(),
                            response_total_tokens: outcome.response_total_tokens,
                            tool_output_tokens: outcome.tool_output_tokens,
                        },
                    })
                    .await;

                    if cancellation_token.is_cancelled() {
                        self.emit_event(AgentEvent::TurnAborted {
                            session_id: self.session_id,
                            submission_id,
                        })
                        .await;
                        break;
                    }

                    if outcome.needs_follow_up {
                        continue;
                    }

                    if self.drain_input_queue(InputQueue::Steering).await {
                        continue;
                    }
                    if self.drain_input_queue(InputQueue::FollowUp).await {
                        continue;
                    }
                    break;
                }
                Err(Error::Aborted) => {
                    self.emit_event(AgentEvent::TurnAborted {
                        session_id: self.session_id,
                        submission_id,
                    })
                    .await;
                    break;
                }
                Err(err) => {
                    self.emit_event(AgentEvent::TurnFailed {
                        session_id: self.session_id,
                        submission_id,
                        error: err.to_string(),
                    })
                    .await;
                    break;
                }
            }
        }

        let mut state = self.state.lock().await;
        state.status = SessionStatus::Idle;
        state.active_turn = None;
        drop(state);

        self.emit_event(AgentEvent::SessionStatusChanged {
            session_id: self.session_id,
            status: SessionStatus::Idle,
        })
        .await;
    }

    async fn build_turn_request(&self) -> Result<TurnRequest> {
        let (conversation, tools) = {
            let state = self.state.lock().await;
            (state.conversation.clone(), self.definition.tools.specs())
        };
        let base_instructions = self
            .base_instructions_for_turn(&conversation, &tools)
            .await?;
        Ok(TurnRequest {
            conversation,
            tools,
            parallel_tool_calls: self.definition.tools.any_parallel_tool_calls(),
            base_instructions,
            personality: self.definition.personality,
            output_schema: self.definition.output_schema.clone(),
        })
    }

    async fn base_instructions_for_turn(
        &self,
        conversation: &[TranscriptItem],
        tools: &[ToolDescriptor],
    ) -> Result<BaseInstructions> {
        if self.definition.skill_providers.is_empty() {
            return Ok(self.definition.base_instructions.clone());
        }

        let context = crate::skills::SkillContext {
            session_id: self.session_id,
            conversation: conversation.to_vec(),
            runtime: self.definition.runtime_metadata.clone(),
            tools: tools.to_vec(),
        };
        let mut skills = Vec::new();
        for provider in &self.definition.skill_providers {
            skills.extend(provider.skills_for_turn(&context).await?);
        }

        if skills.is_empty() {
            return Ok(self.definition.base_instructions.clone());
        }

        let mut base_instructions = self.definition.base_instructions.clone();
        base_instructions.text.push_str("\n\n<available_skills>\n");
        for skill in skills {
            base_instructions.text.push_str("<skill>\n");
            base_instructions.text.push_str("id: ");
            base_instructions.text.push_str(&skill.id);
            base_instructions.text.push('\n');
            base_instructions.text.push_str("name: ");
            base_instructions.text.push_str(&skill.name);
            base_instructions.text.push('\n');
            if let Some(description) = skill.description {
                base_instructions.text.push_str("description: ");
                base_instructions.text.push_str(&description);
                base_instructions.text.push('\n');
            }
            if !skill.required_tools.is_empty() {
                base_instructions.text.push_str("required_tools: ");
                base_instructions
                    .text
                    .push_str(&skill.required_tools.join(", "));
                base_instructions.text.push('\n');
            }
            base_instructions.text.push_str("instructions:\n");
            base_instructions.text.push_str(&skill.instructions);
            base_instructions.text.push_str("\n</skill>\n");
        }
        base_instructions.text.push_str("</available_skills>");
        Ok(base_instructions)
    }

    async fn append_items(&self, items: Vec<TranscriptItem>) {
        let mut state = self.state.lock().await;
        state.conversation.extend(items);
    }

    async fn drain_bootstrap_inputs(&self) -> bool {
        if self.drain_input_queue(InputQueue::Steering).await {
            true
        } else {
            self.drain_input_queue(InputQueue::FollowUp).await
        }
    }

    async fn drain_input_queue(&self, queue: InputQueue) -> bool {
        let items = {
            let mut state = self.state.lock().await;
            let source = match queue {
                InputQueue::Primary => return false,
                InputQueue::Steering => &mut state.steering_queue,
                InputQueue::FollowUp => &mut state.follow_up_queue,
            };
            let mut drained = Vec::new();
            while let Some(input) = source.pop_front() {
                drained.extend(input.into_items());
            }
            drained
        };

        if items.is_empty() {
            false
        } else {
            self.append_items(items).await;
            true
        }
    }
}

impl SessionState {
    fn has_bootstrap_input(&self) -> bool {
        !self.steering_queue.is_empty() || !self.follow_up_queue.is_empty()
    }
}

fn can_continue_from_history(history: &[TranscriptItem]) -> bool {
    !matches!(
        history.last(),
        Some(TranscriptItem::Message { role, .. }) if role == "assistant"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentBuilder;
    use crate::skills::Skill;
    use crate::skills::SkillContext;
    use crate::skills::SkillError;
    use crate::skills::SkillProvider;
    use crate::tools::ToolError;
    use crate::tools::ToolHandler;
    use crate::tools::ToolInvocation;
    use crate::tools::ToolOutput;
    use async_trait::async_trait;
    use lha_llm::BaseInstructions;
    use lha_llm::FunctionToolDescriptor;
    use lha_llm::RuntimeMetadata;
    use lha_llm::SemanticConversationCompactor;
    use lha_llm::SemanticRuntime;
    use lha_llm::SemanticRuntimeSession;
    use lha_llm::ToolCallPayload;
    use lha_llm::ToolCallRequest;
    use lha_llm::ToolDescriptor;
    use lha_llm::ToolInputSchema;
    use lha_llm::ToolResultPayload;
    use lha_llm::TranscriptItem;
    use lha_llm::TurnEvent;
    use lha_llm::TurnEventStream;
    use lha_llm::types::ContentItem;
    use lha_llm::types::TokenUsage;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::Notify;
    use tokio::sync::mpsc;
    use tokio::sync::oneshot;
    use tokio::time::Duration;
    use tokio::time::timeout;
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct FakeRuntime {
        scripts: Arc<StdMutex<VecDeque<FakeTurnScript>>>,
        requests: Arc<StdMutex<Vec<TurnRequest>>>,
    }

    struct FakeTurnScript {
        events: Vec<lha_llm::Result<TurnEvent>>,
        gate: Option<FakeGate>,
        hold_open: Option<Arc<Notify>>,
    }

    struct FakeGate {
        release: Arc<Notify>,
        ready: Option<oneshot::Sender<()>>,
    }

    struct FakeRuntimeSession {
        scripts: Arc<StdMutex<VecDeque<FakeTurnScript>>>,
        requests: Arc<StdMutex<Vec<TurnRequest>>>,
    }

    #[async_trait]
    impl SemanticConversationCompactor for FakeRuntime {
        async fn compact_conversation_history(
            &self,
            input: &TurnRequest,
        ) -> lha_llm::Result<Vec<TranscriptItem>> {
            Ok(input.conversation.clone())
        }
    }

    #[async_trait]
    impl SemanticRuntime for FakeRuntime {
        fn new_session(&self) -> Box<dyn SemanticRuntimeSession> {
            Box::new(FakeRuntimeSession {
                scripts: Arc::clone(&self.scripts),
                requests: Arc::clone(&self.requests),
            })
        }

        fn capabilities(&self) -> lha_llm::RuntimeCapabilities {
            lha_llm::RuntimeCapabilities {
                supports_parallel_tool_calls: true,
                enforce_declared_tool_names: false,
                supports_dynamic_context_window_probe: false,
                supports_reasoning_summaries: true,
                supports_output_schema: true,
                supports_remote_compaction: false,
            }
        }

        fn metadata(&self) -> RuntimeMetadata {
            RuntimeMetadata {
                endpoint_name: "fake".to_string(),
                model: "test-model".to_string(),
            }
        }

        fn estimated_input_tokens(&self, _input: &TurnRequest) -> Option<i64> {
            None
        }
    }

    #[async_trait]
    impl SemanticRuntimeSession for FakeRuntimeSession {
        async fn run_turn(&mut self, input: &TurnRequest) -> lha_llm::Result<TurnEventStream> {
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(input.clone());
            let script = self
                .scripts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("script should exist");
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let FakeTurnScript {
                    events,
                    gate,
                    hold_open,
                } = script;
                let mut gate = gate;

                for event in events {
                    if tx.send(event).await.is_err() {
                        return;
                    }
                    if let Some(gate) = gate.take() {
                        if let Some(ready) = gate.ready {
                            let _ = ready.send(());
                        }
                        gate.release.notified().await;
                    }
                }
                if let Some(hold_open) = hold_open.as_ref() {
                    hold_open.notified().await;
                }
            });
            Ok(TurnEventStream::from_receiver(rx))
        }
    }

    struct EchoTool;

    #[async_trait]
    impl ToolHandler for EchoTool {
        fn spec(&self) -> ToolDescriptor {
            ToolDescriptor::Function(FunctionToolDescriptor {
                name: "echo_tool".to_string(),
                description: "echo tool".to_string(),
                strict: false,
                parameters: ToolInputSchema::Object {
                    properties: BTreeMap::new(),
                    required: Some(Vec::new()),
                    additional_properties: Some(true.into()),
                },
            })
        }

        async fn handle(
            &self,
            invocation: ToolInvocation,
            _cancellation_token: CancellationToken,
        ) -> std::result::Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Function {
                content: format!("handled {}", invocation.tool_name),
                content_items: None,
                success: Some(true),
            })
        }
    }

    fn fake_runtime(scripts: Vec<FakeTurnScript>) -> Arc<dyn SemanticRuntime> {
        fake_runtime_with_requests(scripts).0
    }

    fn fake_runtime_with_requests(
        scripts: Vec<FakeTurnScript>,
    ) -> (Arc<dyn SemanticRuntime>, Arc<StdMutex<Vec<TurnRequest>>>) {
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let runtime: Arc<dyn SemanticRuntime> = Arc::new(FakeRuntime {
            scripts: Arc::new(StdMutex::new(VecDeque::from(scripts))),
            requests: Arc::clone(&requests),
        });
        (runtime, requests)
    }

    struct TestSkillProvider {
        skills: Vec<Skill>,
        error: Option<String>,
        contexts: Arc<StdMutex<Vec<SkillContext>>>,
    }

    #[async_trait]
    impl SkillProvider for TestSkillProvider {
        async fn skills_for_turn(
            &self,
            context: &SkillContext,
        ) -> std::result::Result<Vec<Skill>, SkillError> {
            self.contexts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(context.clone());
            if let Some(error) = &self.error {
                return Err(SkillError::Fatal(error.clone()));
            }
            Ok(self.skills.clone())
        }
    }

    fn test_skill(id: &str, instructions: &str) -> Skill {
        Skill {
            id: id.to_string(),
            name: format!("{id} skill"),
            description: Some(format!("{id} description")),
            instructions: instructions.to_string(),
            required_tools: vec!["echo_tool".to_string()],
        }
    }

    fn completed_script() -> FakeTurnScript {
        FakeTurnScript {
            events: vec![Ok(TurnEvent::Completed {
                response_id: "resp-1".to_string(),
                token_usage: None,
            })],
            gate: None,
            hold_open: None,
        }
    }

    fn lock_requests(requests: &StdMutex<Vec<TurnRequest>>) -> Vec<TurnRequest> {
        requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn assistant_item(text: &str) -> lha_llm::SemanticOutputItem {
        assistant_item_with_output_texts(&[text])
    }

    fn assistant_item_with_output_texts(texts: &[&str]) -> lha_llm::SemanticOutputItem {
        lha_llm::SemanticOutputItem::AssistantMessage {
            item: TranscriptItem::Message {
                id: Some("msg-1".to_string()),
                role: "assistant".to_string(),
                content: texts
                    .iter()
                    .map(|text| ContentItem::OutputText {
                        text: (*text).to_string(),
                    })
                    .collect(),
                end_turn: None,
            },
        }
    }

    async fn wait_for_idle(session: &AgentSession) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        let mut seen_running = false;
        loop {
            let event = timeout(Duration::from_secs(2), session.next_event())
                .await
                .expect("event should arrive")
                .expect("session event should succeed");
            if matches!(
                &event,
                AgentEvent::SessionStatusChanged {
                    status: SessionStatus::Running,
                    ..
                }
            ) {
                seen_running = true;
            }
            let is_idle = seen_running
                && matches!(
                    &event,
                    AgentEvent::SessionStatusChanged {
                        status: SessionStatus::Idle,
                        ..
                    }
                );
            events.push(event);
            if is_idle {
                return events;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_records_assistant_message_and_completes() {
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: vec![
                Ok(TurnEvent::ItemStarted {
                    handle: "msg-1".to_string(),
                    item: assistant_item("hello"),
                }),
                Ok(TurnEvent::OutputTextDelta {
                    handle: "msg-1".to_string(),
                    delta: "hello".to_string(),
                }),
                Ok(TurnEvent::ItemCompleted {
                    handle: "msg-1".to_string(),
                    item: assistant_item("hello"),
                }),
                Ok(TurnEvent::Completed {
                    response_id: "resp-1".to_string(),
                    token_usage: Some(TokenUsage {
                        input_tokens: 1,
                        cached_input_tokens: 0,
                        output_tokens: 2,
                        reasoning_output_tokens: 0,
                        total_tokens: 3,
                    }),
                }),
            ],
            gate: None,
            hold_open: None,
        }]);

        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();

        session
            .run(SessionInput::from_user_text("hi"))
            .await
            .expect("run should succeed");
        let events = wait_for_idle(&session).await;
        let event = events
            .into_iter()
            .find(|event| matches!(event, AgentEvent::TurnCompleted { .. }))
            .expect("turn completion should be emitted");

        match event {
            AgentEvent::TurnCompleted { outcome, .. } => {
                assert_eq!(outcome.last_agent_message, Some("hello".to_string()));
                assert_eq!(outcome.response_total_tokens, Some(3));
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }

        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.status, SessionStatus::Idle);
        assert_eq!(snapshot.conversation.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn turn_completed_summary_joins_output_text_blocks() {
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: vec![
                Ok(TurnEvent::ItemStarted {
                    handle: "msg-1".to_string(),
                    item: assistant_item_with_output_texts(&[]),
                }),
                Ok(TurnEvent::ItemCompleted {
                    handle: "msg-1".to_string(),
                    item: assistant_item_with_output_texts(&["hel", "lo"]),
                }),
                Ok(TurnEvent::Completed {
                    response_id: "resp-1".to_string(),
                    token_usage: None,
                }),
            ],
            gate: None,
            hold_open: None,
        }]);

        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();

        session
            .run(SessionInput::from_user_text("hi"))
            .await
            .expect("run should succeed");
        let events = wait_for_idle(&session).await;
        let event = events
            .into_iter()
            .find(|event| matches!(event, AgentEvent::TurnCompleted { .. }))
            .expect("turn completion should be emitted");

        match event {
            AgentEvent::TurnCompleted { outcome, .. } => {
                assert_eq!(outcome.last_agent_message, Some("hello".to_string()));
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_collect_text_returns_last_agent_message() {
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: vec![
                Ok(TurnEvent::ItemStarted {
                    handle: "msg-1".to_string(),
                    item: assistant_item("hello"),
                }),
                Ok(TurnEvent::ItemCompleted {
                    handle: "msg-1".to_string(),
                    item: assistant_item("hello"),
                }),
                Ok(TurnEvent::Completed {
                    response_id: "resp-1".to_string(),
                    token_usage: None,
                }),
            ],
            gate: None,
            hold_open: None,
        }]);
        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();

        let text = session
            .run_collect_text(SessionInput::from_user_text("hi"))
            .await
            .expect("text should be collected");

        assert_eq!(text, "hello");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_collect_text_joins_completed_output_text_blocks_without_deltas() {
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: vec![
                Ok(TurnEvent::ItemStarted {
                    handle: "msg-1".to_string(),
                    item: assistant_item_with_output_texts(&[]),
                }),
                Ok(TurnEvent::ItemCompleted {
                    handle: "msg-1".to_string(),
                    item: assistant_item_with_output_texts(&["hel", "lo"]),
                }),
                Ok(TurnEvent::Completed {
                    response_id: "resp-1".to_string(),
                    token_usage: None,
                }),
            ],
            gate: None,
            hold_open: None,
        }]);
        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();

        let text = session
            .run_collect_text(SessionInput::from_user_text("hi"))
            .await
            .expect("text should be collected");

        assert_eq!(text, "hello");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_collect_text_prefers_streamed_deltas_over_chunked_last_message() {
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: vec![
                Ok(TurnEvent::ItemStarted {
                    handle: "msg-1".to_string(),
                    item: assistant_item_with_output_texts(&[]),
                }),
                Ok(TurnEvent::OutputTextDelta {
                    handle: "msg-1".to_string(),
                    delta: "hel".to_string(),
                }),
                Ok(TurnEvent::OutputTextDelta {
                    handle: "msg-1".to_string(),
                    delta: "lo".to_string(),
                }),
                Ok(TurnEvent::ItemCompleted {
                    handle: "msg-1".to_string(),
                    item: assistant_item_with_output_texts(&["hel", "lo"]),
                }),
                Ok(TurnEvent::Completed {
                    response_id: "resp-1".to_string(),
                    token_usage: None,
                }),
            ],
            gate: None,
            hold_open: None,
        }]);
        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();

        let text = session
            .run_collect_text(SessionInput::from_user_text("hi"))
            .await
            .expect("text should be collected");

        assert_eq!(text, "hello");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_collect_text_falls_back_to_streamed_deltas() {
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: vec![
                Ok(TurnEvent::OutputTextDelta {
                    handle: "msg-1".to_string(),
                    delta: "hel".to_string(),
                }),
                Ok(TurnEvent::OutputTextDelta {
                    handle: "msg-1".to_string(),
                    delta: "lo".to_string(),
                }),
                Ok(TurnEvent::Completed {
                    response_id: "resp-1".to_string(),
                    token_usage: None,
                }),
            ],
            gate: None,
            hold_open: None,
        }]);
        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();

        let text = session
            .run_collect_text(SessionInput::from_user_text("hi"))
            .await
            .expect("text should be collected");

        assert_eq!(text, "hello");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_collect_text_waits_through_follow_up_turn() {
        let runtime = fake_runtime(vec![
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ToolCall(ToolCallRequest {
                        id: None,
                        tool_name: "echo_tool".to_string(),
                        call_id: "call-1".to_string(),
                        payload: ToolCallPayload::JsonArguments {
                            arguments: "{}".to_string(),
                        },
                    })),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-1".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: None,
                hold_open: None,
            },
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ItemStarted {
                        handle: "msg-2".to_string(),
                        item: assistant_item("done"),
                    }),
                    Ok(TurnEvent::ItemCompleted {
                        handle: "msg-2".to_string(),
                        item: assistant_item("done"),
                    }),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-2".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: None,
                hold_open: None,
            },
        ]);
        let manager = AgentBuilder::new(runtime)
            .register_tool(Arc::new(EchoTool))
            .build();
        let session = manager.create_session();

        let text = session
            .run_collect_text(SessionInput::from_user_text("hi"))
            .await
            .expect("text should be collected after follow-up");

        assert_eq!(text, "done");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_collect_text_uses_final_turn_streamed_text_after_follow_up() {
        let runtime = fake_runtime(vec![
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ItemStarted {
                        handle: "msg-1".to_string(),
                        item: assistant_item("draft"),
                    }),
                    Ok(TurnEvent::OutputTextDelta {
                        handle: "msg-1".to_string(),
                        delta: "draft".to_string(),
                    }),
                    Ok(TurnEvent::ItemCompleted {
                        handle: "msg-1".to_string(),
                        item: assistant_item("draft"),
                    }),
                    Ok(TurnEvent::ToolCall(ToolCallRequest {
                        id: None,
                        tool_name: "echo_tool".to_string(),
                        call_id: "call-1".to_string(),
                        payload: ToolCallPayload::JsonArguments {
                            arguments: "{}".to_string(),
                        },
                    })),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-1".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: None,
                hold_open: None,
            },
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ItemStarted {
                        handle: "msg-2".to_string(),
                        item: assistant_item_with_output_texts(&[]),
                    }),
                    Ok(TurnEvent::OutputTextDelta {
                        handle: "msg-2".to_string(),
                        delta: "fi".to_string(),
                    }),
                    Ok(TurnEvent::OutputTextDelta {
                        handle: "msg-2".to_string(),
                        delta: "nal".to_string(),
                    }),
                    Ok(TurnEvent::ItemCompleted {
                        handle: "msg-2".to_string(),
                        item: assistant_item_with_output_texts(&["fi", "nal"]),
                    }),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-2".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: None,
                hold_open: None,
            },
        ]);
        let manager = AgentBuilder::new(runtime)
            .register_tool(Arc::new(EchoTool))
            .build();
        let session = manager.create_session();

        let text = session
            .run_collect_text(SessionInput::from_user_text("hi"))
            .await
            .expect("text should be collected after follow-up");

        assert_eq!(text, "final");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_collect_text_waits_until_session_idle_before_returning() {
        let follow_up_gate = Arc::new(Notify::new());
        let (follow_up_waiting_tx, follow_up_waiting_rx) = oneshot::channel();
        let runtime = fake_runtime(vec![
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ItemStarted {
                        handle: "msg-1".to_string(),
                        item: assistant_item("first"),
                    }),
                    Ok(TurnEvent::ItemCompleted {
                        handle: "msg-1".to_string(),
                        item: assistant_item("first"),
                    }),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-1".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: None,
                hold_open: None,
            },
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ItemStarted {
                        handle: "msg-2".to_string(),
                        item: assistant_item("queued"),
                    }),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-2".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: Some(FakeGate {
                    release: Arc::clone(&follow_up_gate),
                    ready: Some(follow_up_waiting_tx),
                }),
                hold_open: None,
            },
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ItemStarted {
                        handle: "msg-3".to_string(),
                        item: assistant_item("second"),
                    }),
                    Ok(TurnEvent::ItemCompleted {
                        handle: "msg-3".to_string(),
                        item: assistant_item("second"),
                    }),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-3".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: None,
                hold_open: None,
            },
        ]);
        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();
        session
            .follow_up(SessionInput::from_user_text("queued"))
            .await;

        let collect_session = session.clone();
        let collect_task = tokio::spawn(async move {
            collect_session
                .run_collect_text(SessionInput::from_user_text("first"))
                .await
        });

        timeout(Duration::from_secs(2), follow_up_waiting_rx)
            .await
            .expect("follow-up turn should reach gate")
            .expect("follow-up gate waiter should be signaled");
        assert!(
            !collect_task.is_finished(),
            "collect-text should wait for queued follow-up cleanup"
        );

        follow_up_gate.notify_one();
        let text = timeout(Duration::from_secs(2), collect_task)
            .await
            .expect("collect task should finish")
            .expect("collect task should not panic")
            .expect("text should be collected");

        assert_eq!(text, "first");
        assert_eq!(session.status().await, SessionStatus::Idle);

        let second_text = session
            .run_collect_text(SessionInput::from_user_text("second"))
            .await
            .expect("session should be immediately reusable");
        assert_eq!(second_text, "second");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_collect_text_maps_turn_failed() {
        let runtime = fake_runtime(Vec::new());
        let manager = AgentBuilder::new(runtime)
            .register_skill_provider(Arc::new(TestSkillProvider {
                skills: Vec::new(),
                error: Some("boom".to_string()),
                contexts: Arc::new(StdMutex::new(Vec::new())),
            }))
            .build();
        let session = manager.create_session();

        let err = session
            .run_collect_text(SessionInput::from_user_text("hi"))
            .await
            .expect_err("turn failure should be returned");

        assert!(matches!(
            err,
            RunCollectTextError::TurnFailed(message) if message == "boom"
        ));
        assert_eq!(session.status().await, SessionStatus::Idle);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ask_once_creates_session_and_returns_text() {
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: vec![
                Ok(TurnEvent::ItemStarted {
                    handle: "msg-1".to_string(),
                    item: assistant_item("hello"),
                }),
                Ok(TurnEvent::ItemCompleted {
                    handle: "msg-1".to_string(),
                    item: assistant_item("hello"),
                }),
                Ok(TurnEvent::Completed {
                    response_id: "resp-1".to_string(),
                    token_usage: None,
                }),
            ],
            gate: None,
            hold_open: None,
        }]);
        let manager = AgentBuilder::new(runtime).build();

        let text = manager
            .ask_once("hi")
            .await
            .expect("text should be collected");

        assert_eq!(text, "hello");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_handles_tool_follow_up_turn() {
        let runtime = fake_runtime(vec![
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ToolCall(ToolCallRequest {
                        id: None,
                        tool_name: "echo_tool".to_string(),
                        call_id: "call-1".to_string(),
                        payload: ToolCallPayload::JsonArguments {
                            arguments: "{}".to_string(),
                        },
                    })),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-1".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: None,
                hold_open: None,
            },
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ItemStarted {
                        handle: "msg-2".to_string(),
                        item: assistant_item("done"),
                    }),
                    Ok(TurnEvent::ItemCompleted {
                        handle: "msg-2".to_string(),
                        item: assistant_item("done"),
                    }),
                    Ok(TurnEvent::Completed {
                        response_id: "resp-2".to_string(),
                        token_usage: None,
                    }),
                ],
                gate: None,
                hold_open: None,
            },
        ]);

        let manager = AgentBuilder::new(runtime)
            .register_tool(Arc::new(EchoTool))
            .build();
        let session = manager.create_session();

        session
            .run(SessionInput::from_user_text("hi"))
            .await
            .expect("run should succeed");
        let events = wait_for_idle(&session).await;
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::TurnCompleted { .. }))
                .count()
                >= 2
        );
        let completed_tool_call = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolCallCompleted { response, .. } => Some(response),
                _ => None,
            })
            .expect("tool completion should be emitted");
        assert_eq!(
            completed_tool_call,
            &lha_llm::ToolResultItem {
                call_id: "call-1".to_string(),
                tool_name: "echo_tool".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "handled echo_tool".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            }
        );

        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.conversation.len(), 4);
        assert_eq!(
            snapshot.conversation[2],
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "echo_tool".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "handled echo_tool".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_skill_provider_preserves_turn_request_shape() {
        let (runtime, requests) = fake_runtime_with_requests(vec![completed_script()]);
        let manager = AgentBuilder::new(runtime)
            .with_base_instructions("base")
            .register_tool(Arc::new(EchoTool))
            .build();
        let session = manager.create_session();

        session
            .run(SessionInput::from_user_text("hi"))
            .await
            .expect("run should succeed");
        wait_for_idle(&session).await;

        let requests = lock_requests(&requests);
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.conversation,
            SessionInput::from_user_text("hi").into_items()
        );
        assert_eq!(
            request.base_instructions,
            BaseInstructions {
                text: "base".to_string()
            }
        );
        assert_eq!(request.tools, vec![EchoTool.spec()]);
        assert!(!request.parallel_tool_calls);
        assert_eq!(request.personality, None);
        assert_eq!(request.output_schema, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skill_provider_appends_instructions_and_receives_context() {
        let (runtime, requests) = fake_runtime_with_requests(vec![completed_script()]);
        let contexts = Arc::new(StdMutex::new(Vec::new()));
        let provider = Arc::new(TestSkillProvider {
            skills: vec![test_skill("alpha", "Use alpha carefully.")],
            error: None,
            contexts: Arc::clone(&contexts),
        });
        let manager = AgentBuilder::new(runtime)
            .with_base_instructions("base")
            .register_tool(Arc::new(EchoTool))
            .register_skill_provider(provider)
            .build();
        let session = manager.create_session();

        session
            .run(SessionInput::from_user_text("hi"))
            .await
            .expect("run should succeed");
        wait_for_idle(&session).await;

        let requests = lock_requests(&requests);
        assert_eq!(requests.len(), 1);
        let instructions = &requests[0].base_instructions.text;
        assert!(instructions.contains("<available_skills>"));
        assert!(instructions.contains("id: alpha"));
        assert!(instructions.contains("name: alpha skill"));
        assert!(instructions.contains("Use alpha carefully."));

        let contexts = contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(
            contexts,
            vec![SkillContext {
                session_id: session.id(),
                conversation: SessionInput::from_user_text("hi").into_items(),
                runtime: RuntimeMetadata {
                    endpoint_name: "fake".to_string(),
                    model: "test-model".to_string(),
                },
                tools: vec![EchoTool.spec()],
            }]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiple_skill_providers_preserve_registration_order() {
        let (runtime, requests) = fake_runtime_with_requests(vec![completed_script()]);
        let first_contexts = Arc::new(StdMutex::new(Vec::new()));
        let second_contexts = Arc::new(StdMutex::new(Vec::new()));
        let manager = AgentBuilder::new(runtime)
            .with_base_instructions("base")
            .register_skill_provider(Arc::new(TestSkillProvider {
                skills: vec![test_skill("first", "First instructions.")],
                error: None,
                contexts: Arc::clone(&first_contexts),
            }))
            .register_skill_provider(Arc::new(TestSkillProvider {
                skills: vec![test_skill("second", "Second instructions.")],
                error: None,
                contexts: Arc::clone(&second_contexts),
            }))
            .build();
        let session = manager.create_session();

        session
            .run(SessionInput::from_user_text("hi"))
            .await
            .expect("run should succeed");
        wait_for_idle(&session).await;

        let requests = lock_requests(&requests);
        let instructions = &requests[0].base_instructions.text;
        let first = instructions
            .find("id: first")
            .expect("first skill should be present");
        let second = instructions
            .find("id: second")
            .expect("second skill should be present");
        assert!(first < second);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skill_provider_error_fails_turn() {
        let runtime = fake_runtime(Vec::new());
        let contexts = Arc::new(StdMutex::new(Vec::new()));
        let manager = AgentBuilder::new(runtime)
            .register_skill_provider(Arc::new(TestSkillProvider {
                skills: Vec::new(),
                error: Some("boom".to_string()),
                contexts,
            }))
            .build();
        let session = manager.create_session();

        session
            .run(SessionInput::from_user_text("hi"))
            .await
            .expect("run should be accepted");
        let events = wait_for_idle(&session).await;
        let failed = events
            .into_iter()
            .find(|event| matches!(event, AgentEvent::TurnFailed { .. }))
            .expect("turn failure should be emitted");

        match failed {
            AgentEvent::TurnFailed { error, .. } => assert_eq!(error, "boom"),
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn continue_turn_rejects_assistant_tail() {
        let runtime = fake_runtime(Vec::new());
        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();

        {
            let mut state = session.inner.state.lock().await;
            state.conversation.push(TranscriptItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "done".to_string(),
                }],
                end_turn: None,
            });
        }

        let err = session.continue_turn().await.expect_err("should fail");
        assert_eq!(err.to_string(), Error::InvalidContinuation.to_string());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_current_turn_cancels_active_turn() {
        let hold_open = Arc::new(Notify::new());
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: Vec::new(),
            gate: None,
            hold_open: Some(Arc::clone(&hold_open)),
        }]);

        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();
        session
            .run(SessionInput::from_user_text("hi"))
            .await
            .expect("run should succeed");

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(session.abort_current_turn().await);
        hold_open.notify_waiters();

        let events = wait_for_idle(&session).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::TurnAborted { .. }))
        );
        assert_eq!(session.status().await, SessionStatus::Idle);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_run_rejects_without_primary_input_queued_event() {
        let hold_open = Arc::new(Notify::new());
        let runtime = fake_runtime(vec![FakeTurnScript {
            events: Vec::new(),
            gate: None,
            hold_open: Some(Arc::clone(&hold_open)),
        }]);

        let manager = AgentBuilder::new(runtime).build();
        let session = manager.create_session();
        session
            .run(SessionInput::from_user_text("first"))
            .await
            .expect("first run should be accepted");

        loop {
            let event = timeout(Duration::from_secs(2), session.next_event())
                .await
                .expect("event should arrive")
                .expect("session event should succeed");
            if matches!(event, AgentEvent::TurnStarted { .. }) {
                break;
            }
        }

        let err = session
            .run(SessionInput::from_user_text("second"))
            .await
            .expect_err("second run should be rejected while busy");
        assert_eq!(err.to_string(), Error::SessionBusy.to_string());
        let event = timeout(Duration::from_millis(50), session.next_event()).await;
        assert!(
            event.is_err(),
            "busy run should not emit a primary input event, got {event:?}"
        );

        hold_open.notify_waiters();
        loop {
            let event = timeout(Duration::from_secs(2), session.next_event())
                .await
                .expect("event should arrive")
                .expect("session event should succeed");
            if matches!(
                event,
                AgentEvent::SessionStatusChanged {
                    status: SessionStatus::Idle,
                    ..
                }
            ) {
                break;
            }
        }
        assert_eq!(session.status().await, SessionStatus::Idle);
    }
}
