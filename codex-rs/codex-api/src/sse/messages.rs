use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::telemetry::SseTelemetry;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;

static NEXT_ASSISTANT_ITEM_ID: AtomicU64 = AtomicU64::new(1);

pub fn spawn_messages_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    _turn_state: Option<Arc<OnceLock<String>>>,
) -> ResponseStream {
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(process_messages_sse(
        stream_response.bytes,
        tx_event,
        idle_timeout,
        telemetry,
    ));
    ResponseStream { rx_event }
}

#[derive(Debug, Default)]
struct ToolUseState {
    id: Option<String>,
    name: Option<String>,
    input_json: String,
}

#[derive(Debug, Default)]
struct UsageState {
    input_tokens: i64,
    output_tokens: i64,
}

impl UsageState {
    fn token_usage(&self) -> Option<TokenUsage> {
        if self.input_tokens == 0 && self.output_tokens == 0 {
            return None;
        }

        let total_tokens = self.input_tokens.saturating_add(self.output_tokens);
        Some(TokenUsage {
            input_tokens: self.input_tokens,
            cached_input_tokens: 0,
            output_tokens: self.output_tokens,
            reasoning_output_tokens: 0,
            total_tokens,
        })
    }
}

#[derive(Debug, Deserialize)]
struct MessagesEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    message: Option<MessagePayload>,
    #[serde(default)]
    content_block: Option<ContentBlockPayload>,
    #[serde(default)]
    delta: Option<DeltaPayload>,
    #[serde(default)]
    usage: Option<UsagePayload>,
    #[serde(default)]
    error: Option<ErrorPayload>,
}

#[derive(Debug, Deserialize)]
struct MessagePayload {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<UsagePayload>,
}

#[derive(Debug, Deserialize)]
struct UsagePayload {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ContentBlockPayload {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeltaPayload {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorPayload {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

pub async fn process_messages_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream.eventsource();
    let mut assistant_item: Option<ResponseItem> = None;
    let mut assistant_item_id: Option<String> = None;
    let mut tool_uses: HashMap<usize, ToolUseState> = HashMap::new();
    let mut usage_state = UsageState::default();
    let mut response_id = String::new();
    let mut stop_reason: Option<String> = None;

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(err))) => {
                let _ = tx_event.send(Err(ApiError::Stream(err.to_string()))).await;
                return;
            }
            Ok(None) => {
                let _ = tx_event
                    .send(Ok(ResponseEvent::Completed {
                        response_id,
                        token_usage: usage_state.token_usage(),
                    }))
                    .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        let event: MessagesEvent = match serde_json::from_str(&sse.data) {
            Ok(event) => event,
            Err(err) => {
                debug!("failed to parse Messages SSE event: {err}");
                continue;
            }
        };

        match event.kind.as_str() {
            "message_start" => {
                if let Some(message) = event.message {
                    response_id = message.id.unwrap_or_default();
                    assistant_item_id = Some(if response_id.is_empty() {
                        next_assistant_item_id()
                    } else {
                        response_id.clone()
                    });
                    if let Some(usage) = message.usage {
                        if let Some(input_tokens) = usage.input_tokens {
                            usage_state.input_tokens = input_tokens;
                        }
                        if let Some(output_tokens) = usage.output_tokens {
                            usage_state.output_tokens = output_tokens;
                        }
                    }
                }
                let _ = tx_event.send(Ok(ResponseEvent::Created)).await;
            }
            "content_block_start" => {
                let Some(index) = event.index else {
                    continue;
                };
                let Some(block) = event.content_block else {
                    continue;
                };
                if block.kind == "tool_use" {
                    tool_uses.insert(
                        index,
                        ToolUseState {
                            id: block.id,
                            name: block.name,
                            input_json: String::new(),
                        },
                    );
                }
            }
            "content_block_delta" => {
                let Some(index) = event.index else {
                    continue;
                };
                let Some(delta) = event.delta else {
                    continue;
                };
                match delta.kind.as_str() {
                    "text_delta" => {
                        let text = delta.text.unwrap_or_default();
                        let assistant_item_id = assistant_item_id
                            .get_or_insert_with(next_assistant_item_id)
                            .clone();
                        append_assistant_text(
                            &tx_event,
                            &mut assistant_item,
                            assistant_item_id,
                            text,
                        )
                        .await;
                    }
                    "input_json_delta" => {
                        if let Some(tool_use) = tool_uses.get_mut(&index)
                            && let Some(partial_json) = delta.partial_json
                        {
                            tool_use.input_json.push_str(&partial_json);
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(index) = event.index else {
                    continue;
                };
                let Some(tool_use) = tool_uses.remove(&index) else {
                    continue;
                };
                let Some(name) = tool_use.name else {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "messages tool_use missing name".to_string(),
                        )))
                        .await;
                    return;
                };
                let Some(call_id) = tool_use.id else {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "messages tool_use missing id".to_string(),
                        )))
                        .await;
                    return;
                };
                let input_json = if tool_use.input_json.trim().is_empty() {
                    "{}".to_string()
                } else {
                    tool_use.input_json
                };
                if serde_json::from_str::<Value>(&input_json)
                    .ok()
                    .filter(Value::is_object)
                    .is_none()
                {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "messages tool_use input did not form a JSON object".to_string(),
                        )))
                        .await;
                    return;
                }

                let _ = tx_event
                    .send(Ok(ResponseEvent::OutputItemDone(
                        ResponseItem::FunctionCall {
                            id: None,
                            name,
                            arguments: input_json,
                            call_id,
                        },
                    )))
                    .await;
            }
            "message_delta" => {
                if let Some(delta) = event.delta
                    && let Some(reason) = delta.stop_reason
                {
                    stop_reason = Some(reason);
                }
                if let Some(usage) = event.usage
                    && let Some(output_tokens) = usage.output_tokens
                {
                    usage_state.output_tokens = output_tokens;
                }
            }
            "message_stop" => {
                if let Some(assistant) = assistant_item.take() {
                    let _ = tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(assistant)))
                        .await;
                }
                if stop_reason.as_deref() == Some("max_tokens") {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "messages response reached max_tokens limit".to_string(),
                        )))
                        .await;
                    return;
                }
                let _ = tx_event
                    .send(Ok(ResponseEvent::Completed {
                        response_id,
                        token_usage: usage_state.token_usage(),
                    }))
                    .await;
                return;
            }
            "error" => {
                let Some(error) = event.error else {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "messages stream returned unknown error".to_string(),
                        )))
                        .await;
                    return;
                };
                let api_error = match error.kind.as_str() {
                    "invalid_request_error" => ApiError::InvalidRequest {
                        message: error.message,
                    },
                    "rate_limit_error" => ApiError::RateLimit(error.message),
                    "overloaded_error" => ApiError::Retryable {
                        message: error.message,
                        delay: None,
                    },
                    _ => ApiError::Stream(error.message),
                };
                let _ = tx_event.send(Err(api_error)).await;
                return;
            }
            "ping" => {}
            _ => {}
        }
    }
}

async fn append_assistant_text(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    assistant_item: &mut Option<ResponseItem>,
    assistant_item_id: String,
    text: String,
) {
    if assistant_item.is_none() {
        let item = ResponseItem::Message {
            id: Some(assistant_item_id),
            role: "assistant".to_string(),
            content: vec![],
            end_turn: None,
        };
        *assistant_item = Some(item.clone());
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputItemAdded(item)))
            .await;
    }

    if let Some(ResponseItem::Message { content, .. }) = assistant_item {
        content.push(ContentItem::OutputText { text: text.clone() });
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputTextDelta(text)))
            .await;
    }
}

fn next_assistant_item_id() -> String {
    let next_id = NEXT_ASSISTANT_ITEM_ID.fetch_add(1, Ordering::Relaxed);
    format!("messages-assistant-{next_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc;
    use tokio_util::io::ReaderStream;

    fn build_stream(body: &str) -> ByteStream {
        let reader = std::io::Cursor::new(body.to_string());
        let stream = ReaderStream::new(reader)
            .map_err(|err| codex_client::TransportError::Network(err.to_string()));
        Box::pin(stream)
    }

    #[tokio::test]
    async fn parses_text_and_tool_use_stream() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":12}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"src/main.rs\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(16);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.expect("event should succeed"));
        }

        assert_eq!(events.len(), 6);
        assert!(matches!(events[0], ResponseEvent::Created));
        assert!(matches!(events[1], ResponseEvent::OutputItemAdded(_)));
        assert!(matches!(events[2], ResponseEvent::OutputTextDelta(_)));
        assert!(matches!(
            events[3],
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { .. })
        ));
        assert!(matches!(
            events[4],
            ResponseEvent::OutputItemDone(ResponseItem::Message { .. })
        ));
        assert!(matches!(events[5], ResponseEvent::Completed { .. }));
    }

    #[tokio::test]
    async fn maps_invalid_request_error() {
        let body = concat!(
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"bad request\"}}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(4);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let err = rx
            .recv()
            .await
            .expect("event")
            .expect_err("should be error");
        assert_eq!(err.to_string(), "invalid request: bad request");
    }

    #[tokio::test]
    async fn text_stream_reuses_message_id_for_output_item_lifecycle() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(8);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.expect("event should succeed"));
        }

        assert_eq!(events.len(), 5);
        assert!(matches!(events[0], ResponseEvent::Created));
        assert!(matches!(events[2], ResponseEvent::OutputTextDelta(ref delta) if delta == "Hello"));
        assert!(matches!(events[4], ResponseEvent::Completed { .. }));

        let ResponseEvent::OutputItemAdded(ResponseItem::Message {
            id: Some(added_id), ..
        }) = &events[1]
        else {
            panic!("unexpected added event: {:?}", events[1]);
        };

        let ResponseEvent::OutputItemDone(ResponseItem::Message {
            id: Some(completed_id),
            ..
        }) = &events[3]
        else {
            panic!("unexpected completed event: {:?}", events[3]);
        };

        assert_eq!(added_id, "msg_1");
        assert_eq!(completed_id, "msg_1");
    }

    #[tokio::test]
    async fn text_stream_generates_stable_fallback_id_without_message_id() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(8);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.expect("event should succeed"));
        }

        let ResponseEvent::OutputItemAdded(ResponseItem::Message {
            id: Some(added_id), ..
        }) = &events[1]
        else {
            panic!("unexpected added event: {:?}", events[1]);
        };

        let ResponseEvent::OutputItemDone(ResponseItem::Message {
            id: Some(completed_id),
            ..
        }) = &events[3]
        else {
            panic!("unexpected completed event: {:?}", events[3]);
        };

        assert_eq!(added_id, completed_id);
        assert!(added_id.starts_with("messages-assistant-"));
    }
}
