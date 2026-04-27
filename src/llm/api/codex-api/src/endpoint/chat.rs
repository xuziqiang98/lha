use crate::ChatRequest;
use crate::auth::AuthProvider;
use crate::common::Prompt as ApiPrompt;
use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::endpoint::streaming::StreamingClient;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::provider::WireApi;
use crate::sse::chat::spawn_chat_stream;
use crate::telemetry::SseTelemetry;
use adam_client::HttpTransport;
use adam_client::RequestCompression;
use adam_client::RequestTelemetry;
use adam_llm_types::ContentItem;
use adam_llm_types::ReasoningContentItem;
use adam_llm_types::TranscriptItem;
use futures::Stream;
use http::HeaderMap;
use serde_json::Value;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

pub struct ChatClient<T: HttpTransport, A: AuthProvider> {
    streaming: StreamingClient<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> ChatClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            streaming: StreamingClient::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            streaming: self.streaming.with_telemetry(request, sse),
        }
    }

    pub async fn stream_request(&self, request: ChatRequest) -> Result<ResponseStream, ApiError> {
        self.stream(request.body, request.headers).await
    }

    pub async fn stream_prompt(
        &self,
        model: &str,
        prompt: &ApiPrompt,
        conversation_id: Option<String>,
        origin_tag: Option<String>,
    ) -> Result<ResponseStream, ApiError> {
        use crate::requests::ChatRequestBuilder;

        let request =
            ChatRequestBuilder::new(model, &prompt.instructions, &prompt.input, &prompt.tools)
                .conversation_id(conversation_id)
                .origin_tag(origin_tag)
                .build(self.streaming.provider())?;

        self.stream_request(request).await
    }

    fn path(&self) -> &'static str {
        match self.streaming.provider().wire {
            WireApi::Chat => "chat/completions",
            WireApi::Responses | WireApi::Compact => "responses",
            WireApi::Messages => "messages",
        }
    }

    pub async fn stream(
        &self,
        body: Value,
        extra_headers: HeaderMap,
    ) -> Result<ResponseStream, ApiError> {
        if self.streaming.provider().wire == WireApi::Messages {
            return Err(ApiError::Stream(
                "messages wire api requires MessagesClient".to_string(),
            ));
        }

        self.streaming
            .stream(
                self.path(),
                body,
                extra_headers,
                RequestCompression::None,
                spawn_chat_stream,
                None,
            )
            .await
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum AggregateMode {
    AggregatedOnly,
    Streaming,
}

/// Stream adapter that merges token deltas into a single assistant message per turn.
pub struct AggregatedStream {
    inner: ResponseStream,
    cumulative: String,
    cumulative_reasoning: String,
    pending: VecDeque<ResponseEvent>,
    mode: AggregateMode,
}

impl Stream for AggregatedStream {
    type Item = Result<ResponseEvent, ApiError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if let Some(ev) = this.pending.pop_front() {
            return Poll::Ready(Some(Ok(ev)));
        }

        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Some(Ok(ResponseEvent::OutputItemDone(item)))) => {
                    let is_assistant_message = matches!(
                        &item,
                        TranscriptItem::Message { role, .. } if role == "assistant"
                    );

                    if is_assistant_message {
                        match this.mode {
                            AggregateMode::AggregatedOnly => {
                                if this.cumulative.is_empty()
                                    && let TranscriptItem::Message { content, .. } = &item
                                    && let Some(text) = content.iter().find_map(|c| match c {
                                        ContentItem::OutputText { text } => Some(text),
                                        _ => None,
                                    })
                                {
                                    this.cumulative.push_str(text);
                                }
                                continue;
                            }
                            AggregateMode::Streaming => {
                                if this.cumulative.is_empty() {
                                    return Poll::Ready(Some(Ok(ResponseEvent::OutputItemDone(
                                        item,
                                    ))));
                                } else {
                                    continue;
                                }
                            }
                        }
                    }

                    return Poll::Ready(Some(Ok(ResponseEvent::OutputItemDone(item))));
                }
                Poll::Ready(Some(Ok(ResponseEvent::ServerReasoningIncluded(included)))) => {
                    return Poll::Ready(Some(Ok(ResponseEvent::ServerReasoningIncluded(included))));
                }
                Poll::Ready(Some(Ok(ResponseEvent::RateLimits(snapshot)))) => {
                    return Poll::Ready(Some(Ok(ResponseEvent::RateLimits(snapshot))));
                }
                Poll::Ready(Some(Ok(ResponseEvent::ModelsEtag(etag)))) => {
                    return Poll::Ready(Some(Ok(ResponseEvent::ModelsEtag(etag))));
                }
                Poll::Ready(Some(Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                }))) => {
                    let mut emitted_any = false;

                    if !this.cumulative_reasoning.is_empty() {
                        let aggregated_reasoning = TranscriptItem::Reasoning {
                            id: String::new(),
                            summary: Vec::new(),
                            content: Some(vec![ReasoningContentItem::ReasoningText {
                                text: std::mem::take(&mut this.cumulative_reasoning),
                            }]),
                            encrypted_content: None,
                        };
                        this.pending
                            .push_back(ResponseEvent::OutputItemDone(aggregated_reasoning));
                        emitted_any = true;
                    }

                    if !this.cumulative.is_empty() {
                        let aggregated_message = TranscriptItem::Message {
                            id: None,
                            role: "assistant".to_string(),
                            content: vec![ContentItem::OutputText {
                                text: std::mem::take(&mut this.cumulative),
                            }],
                            end_turn: None,
                        };
                        this.pending
                            .push_back(ResponseEvent::OutputItemDone(aggregated_message));
                        emitted_any = true;
                    }

                    if emitted_any {
                        this.pending.push_back(ResponseEvent::Completed {
                            response_id: response_id.clone(),
                            token_usage: token_usage.clone(),
                        });
                        if let Some(ev) = this.pending.pop_front() {
                            return Poll::Ready(Some(Ok(ev)));
                        }
                    }

                    return Poll::Ready(Some(Ok(ResponseEvent::Completed {
                        response_id,
                        token_usage,
                    })));
                }
                Poll::Ready(Some(Ok(ResponseEvent::Created))) => {
                    continue;
                }
                Poll::Ready(Some(Ok(ResponseEvent::OutputTextDelta(delta)))) => {
                    this.cumulative.push_str(&delta);
                    if matches!(this.mode, AggregateMode::Streaming) {
                        return Poll::Ready(Some(Ok(ResponseEvent::OutputTextDelta(delta))));
                    } else {
                        continue;
                    }
                }
                Poll::Ready(Some(Ok(ResponseEvent::ReasoningContentDelta {
                    delta,
                    content_index,
                }))) => {
                    this.cumulative_reasoning.push_str(&delta);
                    if matches!(this.mode, AggregateMode::Streaming) {
                        return Poll::Ready(Some(Ok(ResponseEvent::ReasoningContentDelta {
                            delta,
                            content_index,
                        })));
                    } else {
                        continue;
                    }
                }
                Poll::Ready(Some(Ok(ResponseEvent::ReasoningSummaryDelta { .. }))) => continue,
                Poll::Ready(Some(Ok(ResponseEvent::ReasoningSummaryPartAdded { .. }))) => {
                    continue;
                }
                Poll::Ready(Some(Ok(ResponseEvent::ProposedPlanDelta(delta)))) => {
                    return Poll::Ready(Some(Ok(ResponseEvent::ProposedPlanDelta(delta))));
                }
                Poll::Ready(Some(Ok(ResponseEvent::ProposedPlanDone(text)))) => {
                    return Poll::Ready(Some(Ok(ResponseEvent::ProposedPlanDone(text))));
                }
                Poll::Ready(Some(Ok(ResponseEvent::OutputItemAdded(item)))) => {
                    return Poll::Ready(Some(Ok(ResponseEvent::OutputItemAdded(item))));
                }
            }
        }
    }
}

pub trait AggregateStreamExt {
    fn aggregate(self) -> AggregatedStream;

    fn streaming_mode(self) -> ResponseStream;
}

impl AggregateStreamExt for ResponseStream {
    fn aggregate(self) -> AggregatedStream {
        AggregatedStream::new(self, AggregateMode::AggregatedOnly)
    }

    fn streaming_mode(self) -> ResponseStream {
        self
    }
}

impl AggregatedStream {
    fn new(inner: ResponseStream, mode: AggregateMode) -> Self {
        AggregatedStream {
            inner,
            cumulative: String::new(),
            cumulative_reasoning: String::new(),
            pending: VecDeque::new(),
            mode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AggregateStreamExt;
    use super::ResponseEvent;
    use super::ResponseStream;
    use adam_llm_types::ContentItem;
    use adam_llm_types::TranscriptItem;
    use futures::StreamExt;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn aggregate_preserves_proposed_plan_events() {
        let (tx, rx) = mpsc::channel(8);
        tx.send(Ok(ResponseEvent::OutputTextDelta(
            "Intro\n<proposed_plan>\n- Step 1\n</proposed_plan>\nOutro".to_string(),
        )))
        .await
        .expect("send output delta");
        tx.send(Ok(ResponseEvent::ProposedPlanDelta(
            "- Step 1\n".to_string(),
        )))
        .await
        .expect("send plan delta");
        tx.send(Ok(ResponseEvent::ProposedPlanDone(
            "- Step 1\n".to_string(),
        )))
        .await
        .expect("send plan done");
        tx.send(Ok(ResponseEvent::Completed {
            response_id: "resp-1".to_string(),
            token_usage: None,
        }))
        .await
        .expect("send completed");
        drop(tx);

        let mut stream = ResponseStream { rx_event: rx }.aggregate();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("stream event"));
        }

        assert!(events.iter().any(|event| matches!(
            event,
            ResponseEvent::ProposedPlanDelta(text) if text == "- Step 1\n"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ResponseEvent::ProposedPlanDone(text) if text == "- Step 1\n"
        )));

        assert_eq!(events.len(), 4);
        assert!(matches!(
            &events[0],
            ResponseEvent::ProposedPlanDelta(text) if text == "- Step 1\n"
        ));
        assert!(matches!(
            &events[1],
            ResponseEvent::ProposedPlanDone(text) if text == "- Step 1\n"
        ));
        assert!(matches!(
            &events[2],
            ResponseEvent::OutputItemDone(TranscriptItem::Message { content, .. })
                if content == &vec![ContentItem::OutputText {
                    text: "Intro\n<proposed_plan>\n- Step 1\n</proposed_plan>\nOutro".to_string(),
                }]
        ));
        assert!(matches!(
            events[3],
            ResponseEvent::Completed {
                ref response_id,
                token_usage: None,
            } if response_id == "resp-1"
        ));
    }
}
