use crate::Error;
use crate::Result;
use crate::builder::AgentDefinition;
use crate::events::AgentEvent;
use crate::events::TurnSummary;
use crate::input::InputQueue;
use crate::input::SessionInput;
use crate::processor::SessionTurnProcessor;
use crate::processor::outcome_summary;
use crate::snapshot::ActiveTurnSnapshot;
use crate::snapshot::SessionSnapshot;
use crate::status::SessionStatus;
use crate::tools::ToolExecutor;
use async_channel::Receiver;
use async_channel::Sender;
use codex_agent_core::kernel::AgentKernel;
use codex_llm::SemanticRuntimeSession;
use codex_llm::TranscriptItem;
use codex_llm::TurnRequest;
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
            .emit_event(AgentEvent::InputQueued {
                session_id: self.inner.session_id,
                queue: InputQueue::Primary,
                items: items.clone(),
            })
            .await;
        self.inner.spawn_turn_loop(items).await
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
        self.inner.spawn_turn_loop(Vec::new()).await
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
    ) -> Result<SubmissionId> {
        let submission_id = self.next_submission_id.fetch_add(1, Ordering::SeqCst);
        let cancellation_token = CancellationToken::new();

        {
            let mut state = self.state.lock().await;
            if state.active_turn.is_some() {
                return Err(Error::SessionBusy);
            }
            state.status = SessionStatus::Running;
            state.active_turn = Some(ActiveTurnState {
                submission_id,
                cancellation_token: cancellation_token.clone(),
            });
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

            let request = self.build_turn_request().await;
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

    async fn build_turn_request(&self) -> TurnRequest {
        let state = self.state.lock().await;
        TurnRequest {
            conversation: state.conversation.clone(),
            tools: self.definition.tools.specs(),
            parallel_tool_calls: self.definition.tools.any_parallel_tool_calls(),
            base_instructions: self.definition.base_instructions.clone(),
            personality: self.definition.personality,
            output_schema: self.definition.output_schema.clone(),
        }
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
    use crate::tools::ToolError;
    use crate::tools::ToolHandler;
    use crate::tools::ToolInvocation;
    use crate::tools::ToolOutput;
    use async_trait::async_trait;
    use codex_llm::FunctionToolDescriptor;
    use codex_llm::RuntimeMetadata;
    use codex_llm::SemanticConversationCompactor;
    use codex_llm::SemanticRuntime;
    use codex_llm::SemanticRuntimeSession;
    use codex_llm::ToolCallPayload;
    use codex_llm::ToolCallRequest;
    use codex_llm::ToolDescriptor;
    use codex_llm::ToolInputSchema;
    use codex_llm::TranscriptItem;
    use codex_llm::TurnEvent;
    use codex_llm::TurnEventStream;
    use codex_llm_types::ContentItem;
    use codex_llm_types::TokenUsage;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::Notify;
    use tokio::sync::mpsc;
    use tokio::time::Duration;
    use tokio::time::timeout;
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct FakeRuntime {
        scripts: Arc<StdMutex<VecDeque<FakeTurnScript>>>,
    }

    struct FakeTurnScript {
        events: Vec<codex_llm::Result<TurnEvent>>,
        gate: Option<Arc<Notify>>,
        hold_open: Option<Arc<Notify>>,
    }

    struct FakeRuntimeSession {
        scripts: Arc<StdMutex<VecDeque<FakeTurnScript>>>,
    }

    #[async_trait]
    impl SemanticConversationCompactor for FakeRuntime {
        async fn compact_conversation_history(
            &self,
            input: &TurnRequest,
        ) -> codex_llm::Result<Vec<TranscriptItem>> {
            Ok(input.conversation.clone())
        }
    }

    #[async_trait]
    impl SemanticRuntime for FakeRuntime {
        fn new_session(&self) -> Box<dyn SemanticRuntimeSession> {
            Box::new(FakeRuntimeSession {
                scripts: Arc::clone(&self.scripts),
            })
        }

        fn capabilities(&self) -> codex_llm::RuntimeCapabilities {
            codex_llm::RuntimeCapabilities {
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
        async fn run_turn(&mut self, _input: &TurnRequest) -> codex_llm::Result<TurnEventStream> {
            let script = self
                .scripts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("script should exist");
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                for event in script.events {
                    if tx.send(event).await.is_err() {
                        return;
                    }
                    if let Some(gate) = script.gate.as_ref() {
                        gate.notified().await;
                    }
                }
                if let Some(hold_open) = script.hold_open.as_ref() {
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
        Arc::new(FakeRuntime {
            scripts: Arc::new(StdMutex::new(VecDeque::from(scripts))),
        })
    }

    fn assistant_item(text: &str) -> codex_llm::SemanticOutputItem {
        codex_llm::SemanticOutputItem::AssistantMessage {
            item: TranscriptItem::Message {
                id: Some("msg-1".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: text.to_string(),
                }],
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
    async fn run_handles_tool_follow_up_turn() {
        let runtime = fake_runtime(vec![
            FakeTurnScript {
                events: vec![
                    Ok(TurnEvent::ToolCall(ToolCallRequest {
                        tool_name: "echo_tool".to_string(),
                        call_id: "call-1".to_string(),
                        payload: ToolCallPayload::Function {
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

        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.conversation.len(), 4);
        assert!(matches!(
            snapshot.conversation[2],
            TranscriptItem::FunctionCallOutput { .. }
        ));
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
}
