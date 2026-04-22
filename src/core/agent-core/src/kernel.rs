use async_trait::async_trait;
use codex_async_utils::CancelErr;
use codex_async_utils::OrCancelExt;
use codex_llm::ItemHandle;
use codex_llm::ToolResultItem;
use codex_llm::TurnEvent;
use codex_llm::TurnEventStream;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesOrdered;
use tokio_util::sync::CancellationToken;

pub type ToolFuture<E> = BoxFuture<'static, Result<ToolResultItem, E>>;

pub struct TurnEventUpdate<E> {
    pub tool_future: Option<ToolFuture<E>>,
    pub needs_follow_up: bool,
    pub last_agent_message: Option<String>,
    pub active_handle: Option<ItemHandle>,
}

impl<E> Default for TurnEventUpdate<E> {
    fn default() -> Self {
        Self {
            tool_future: None,
            needs_follow_up: false,
            last_agent_message: None,
            active_handle: None,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TurnStreamState {
    pub needs_follow_up: bool,
    pub last_agent_message: Option<String>,
    pub active_handle: Option<ItemHandle>,
}

impl TurnStreamState {
    fn merge<E>(&mut self, update: TurnEventUpdate<E>) -> Option<ToolFuture<E>> {
        self.needs_follow_up |= update.needs_follow_up;
        if let Some(last_agent_message) = update.last_agent_message {
            self.last_agent_message = Some(last_agent_message);
        }
        if let Some(active_handle) = update.active_handle {
            self.active_handle = Some(active_handle);
        }
        update.tool_future
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TurnStreamOutcome {
    pub needs_follow_up: bool,
    pub last_agent_message: Option<String>,
    pub response_total_tokens: Option<i64>,
    pub tool_output_tokens: i64,
}

#[async_trait]
pub trait TurnEventProcessor: Send {
    type Error: Send + 'static;

    async fn handle_event(
        &mut self,
        event: TurnEvent,
    ) -> Result<TurnEventUpdate<Self::Error>, Self::Error>;

    async fn record_tool_result(&mut self, response: ToolResultItem) -> Result<(), Self::Error>;

    async fn on_tool_future_error(&mut self, err: Self::Error) -> Result<(), Self::Error>;

    async fn finish(self, state: TurnStreamState) -> Result<TurnStreamOutcome, Self::Error>
    where
        Self: Sized;

    fn cancelled_error(&self) -> Self::Error;

    fn llm_error(&self, err: codex_llm::Error) -> Self::Error;

    fn stream_closed_error(&self) -> Self::Error;
}

#[derive(Default)]
pub struct AgentKernel;

impl AgentKernel {
    pub fn new() -> Self {
        Self
    }

    pub async fn run_turn<P>(
        &self,
        mut stream: TurnEventStream,
        mut processor: P,
        cancellation_token: CancellationToken,
    ) -> Result<TurnStreamOutcome, P::Error>
    where
        P: TurnEventProcessor,
    {
        let mut in_flight: FuturesOrdered<ToolFuture<P::Error>> = FuturesOrdered::new();
        let mut state = TurnStreamState::default();

        loop {
            let next_event = match stream.next().or_cancel(&cancellation_token).await {
                Ok(Some(Ok(event))) => event,
                Ok(Some(Err(err))) => return Err(processor.llm_error(err)),
                Ok(None) => return Err(processor.stream_closed_error()),
                Err(CancelErr::Cancelled) => return Err(processor.cancelled_error()),
            };

            let is_completed = matches!(next_event, TurnEvent::Completed { .. });
            let tool_future = state.merge(processor.handle_event(next_event).await?);
            if let Some(tool_future) = tool_future {
                in_flight.push_back(tool_future);
            }

            if is_completed {
                break;
            }
        }

        while let Some(result) = in_flight.next().await {
            match result {
                Ok(response) => processor.record_tool_result(response).await?,
                Err(err) => processor.on_tool_future_error(err).await?,
            }
        }

        processor.finish(state).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_llm::SemanticOutputItem;
    use codex_llm::ToolCallPayload;
    use codex_llm::ToolCallRequest;
    use codex_llm::ToolResultItem;
    use codex_llm::ToolResultPayload;
    use codex_llm::TranscriptItem;
    use codex_llm_types::ContentItem;
    use codex_llm_types::TokenUsage;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum TestError {
        Cancelled,
        Llm(String),
        StreamClosed,
    }

    struct RecordingProcessor {
        tool_results: usize,
        response_total_tokens: Option<i64>,
    }

    #[async_trait]
    impl TurnEventProcessor for RecordingProcessor {
        type Error = TestError;

        async fn handle_event(
            &mut self,
            event: TurnEvent,
        ) -> Result<TurnEventUpdate<Self::Error>, Self::Error> {
            match event {
                TurnEvent::ItemStarted { handle, .. } => Ok(TurnEventUpdate {
                    active_handle: Some(handle),
                    ..Default::default()
                }),
                TurnEvent::ItemCompleted { item, .. } => {
                    let last_agent_message = match item {
                        SemanticOutputItem::AssistantMessage {
                            item: TranscriptItem::Message { content, .. },
                        } => content.into_iter().find_map(|entry| match entry {
                            ContentItem::OutputText { text } => Some(text),
                            _ => None,
                        }),
                        _ => None,
                    };
                    Ok(TurnEventUpdate {
                        last_agent_message,
                        ..Default::default()
                    })
                }
                TurnEvent::ToolCall(call) => Ok(TurnEventUpdate {
                    tool_future: Some(Box::pin(async move {
                        Ok(ToolResultItem {
                            call_id: call.call_id,
                            tool_name: call.tool_name,
                            payload: ToolResultPayload::Structured {
                                content: "tool-ok".to_string(),
                                content_items: None,
                                success: Some(true),
                            },
                        })
                    })),
                    needs_follow_up: true,
                    ..Default::default()
                }),
                TurnEvent::Completed { token_usage, .. } => {
                    self.response_total_tokens = token_usage.map(|usage| usage.total_tokens);
                    Ok(TurnEventUpdate::default())
                }
                _ => Ok(TurnEventUpdate::default()),
            }
        }

        async fn record_tool_result(
            &mut self,
            _response: ToolResultItem,
        ) -> Result<(), Self::Error> {
            self.tool_results += 1;
            Ok(())
        }

        async fn on_tool_future_error(&mut self, err: Self::Error) -> Result<(), Self::Error> {
            Err(err)
        }

        async fn finish(self, state: TurnStreamState) -> Result<TurnStreamOutcome, Self::Error> {
            Ok(TurnStreamOutcome {
                needs_follow_up: state.needs_follow_up,
                last_agent_message: state.last_agent_message,
                response_total_tokens: self.response_total_tokens,
                tool_output_tokens: self.tool_results as i64,
            })
        }

        fn cancelled_error(&self) -> Self::Error {
            TestError::Cancelled
        }

        fn llm_error(&self, err: codex_llm::Error) -> Self::Error {
            TestError::Llm(err.to_string())
        }

        fn stream_closed_error(&self) -> Self::Error {
            TestError::StreamClosed
        }
    }

    fn assistant_message_item(text: &str) -> SemanticOutputItem {
        SemanticOutputItem::AssistantMessage {
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

    fn tool_call_request() -> ToolCallRequest {
        ToolCallRequest {
            id: None,
            tool_name: "test_tool".to_string(),
            call_id: "call-1".to_string(),
            payload: ToolCallPayload::JsonArguments {
                arguments: "{}".to_string(),
            },
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kernel_processes_tool_futures_and_completion() {
        let (tx_event, rx_event) = mpsc::channel(8);
        tx_event
            .send(Ok(TurnEvent::ItemStarted {
                handle: "msg-1".to_string(),
                item: assistant_message_item("hello"),
            }))
            .await
            .expect("send start");
        tx_event
            .send(Ok(TurnEvent::ItemCompleted {
                handle: "msg-1".to_string(),
                item: assistant_message_item("hello"),
            }))
            .await
            .expect("send completed item");
        tx_event
            .send(Ok(TurnEvent::ToolCall(tool_call_request())))
            .await
            .expect("send tool call");
        tx_event
            .send(Ok(TurnEvent::Completed {
                response_id: "resp-1".to_string(),
                token_usage: Some(TokenUsage {
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    output_tokens: 2,
                    reasoning_output_tokens: 0,
                    total_tokens: 3,
                }),
            }))
            .await
            .expect("send completed");
        drop(tx_event);

        let kernel = AgentKernel::new();
        let outcome = kernel
            .run_turn(
                TurnEventStream::from_receiver(rx_event),
                RecordingProcessor {
                    tool_results: 0,
                    response_total_tokens: None,
                },
                CancellationToken::new(),
            )
            .await
            .expect("kernel should succeed");

        assert_eq!(
            outcome,
            TurnStreamOutcome {
                needs_follow_up: true,
                last_agent_message: Some("hello".to_string()),
                response_total_tokens: Some(3),
                tool_output_tokens: 1,
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kernel_returns_stream_closed_error_without_completed() {
        let (tx_event, rx_event) = mpsc::channel(1);
        drop(tx_event);
        let kernel = AgentKernel::new();
        let err = kernel
            .run_turn(
                TurnEventStream::from_receiver(rx_event),
                RecordingProcessor {
                    tool_results: 0,
                    response_total_tokens: None,
                },
                CancellationToken::new(),
            )
            .await
            .expect_err("stream should fail");

        assert_eq!(err, TestError::StreamClosed);
    }
}
