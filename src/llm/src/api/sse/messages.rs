use crate::api::common::CompletedResponse;
use crate::api::common::ResponseEvent;
use crate::api::common::ResponseStream;
use crate::api::error::ApiError;
use crate::api::proposed_plan_parser::ProposedPlanParser;
use crate::api::proposed_plan_parser::ProposedPlanSegment;
use crate::api::proposed_plan_parser::extract_proposed_plan_text;
use crate::api::telemetry::SseTelemetry;
use crate::client::ByteStream;
use crate::client::StreamResponse;
use crate::types::ContentItem;
use crate::types::TokenUsage;
use crate::types::ToolCallPayload;
use crate::types::TranscriptItem;
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
    cache_creation_input_tokens: i64,
    cache_read_input_tokens: i64,
    output_tokens: i64,
}

impl UsageState {
    fn apply(&mut self, usage: UsagePayload) {
        if let Some(input_tokens) = usage.input_tokens {
            self.input_tokens = input_tokens;
        }
        if let Some(cache_creation_input_tokens) = usage.cache_creation_input_tokens {
            self.cache_creation_input_tokens = cache_creation_input_tokens;
        }
        if let Some(cache_read_input_tokens) = usage.cache_read_input_tokens {
            self.cache_read_input_tokens = cache_read_input_tokens;
        }
        if let Some(output_tokens) = usage.output_tokens {
            self.output_tokens = output_tokens;
        }
    }

    fn token_usage(&self) -> Option<TokenUsage> {
        let input_tokens = self
            .input_tokens
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.cache_read_input_tokens);
        if input_tokens == 0 && self.output_tokens == 0 {
            return None;
        }

        let total_tokens = input_tokens.saturating_add(self.output_tokens);
        Some(TokenUsage {
            input_tokens,
            cached_input_tokens: self.cache_read_input_tokens,
            output_tokens: self.output_tokens,
            reasoning_output_tokens: 0,
            total_tokens,
        })
    }
}

pub(crate) fn parse_completed_response(value: Value) -> Result<CompletedResponse, ApiError> {
    if value.get("type").and_then(Value::as_str) == Some("error") {
        let error: MessagesErrorEnvelope = serde_json::from_value(value).map_err(|err| {
            ApiError::Stream(format!(
                "failed to parse non-streaming messages error: {err}"
            ))
        })?;
        return Err(messages_api_error(error.error));
    }

    let response_id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut usage_state = UsageState::default();
    if let Some(usage) = value.get("usage") {
        let usage = serde_json::from_value::<UsagePayload>(usage.clone()).map_err(|err| {
            ApiError::Stream(format!(
                "failed to parse non-streaming messages usage: {err}"
            ))
        })?;
        usage_state.apply(usage);
    }

    if value.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
        return Err(ApiError::Stream(
            "messages response reached max_tokens limit".to_string(),
        ));
    }

    let content_blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::Stream("messages response missing content".to_string()))?;
    let mut tool_calls = Vec::new();
    let mut message_content = Vec::new();

    for block in content_blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    message_content.push(ContentItem::OutputText {
                        text: text.to_string(),
                    });
                }
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| ApiError::Stream("messages tool_use missing name".to_string()))?
                    .to_string();
                let call_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| ApiError::Stream("messages tool_use missing id".to_string()))?
                    .to_string();
                let input = block
                    .get("input")
                    .filter(|input| input.is_object())
                    .ok_or_else(|| {
                        ApiError::Stream(
                            "messages tool_use input did not form a JSON object".to_string(),
                        )
                    })?;
                tool_calls.push(TranscriptItem::ToolCall {
                    id: None,
                    call_id,
                    tool_name: name,
                    payload: ToolCallPayload::JsonArguments {
                        arguments: input.to_string(),
                    },
                });
            }
            _ => {}
        }
    }

    let mut output = Vec::new();
    if !message_content.is_empty() {
        output.push(TranscriptItem::Message {
            id: Some(if response_id.is_empty() {
                next_assistant_item_id()
            } else {
                response_id.clone()
            }),
            role: "assistant".to_string(),
            content: message_content,
            end_turn: None,
        });
    }
    output.extend(tool_calls);

    Ok(CompletedResponse {
        response_id,
        output,
        token_usage: usage_state.token_usage(),
    })
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
    cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    cache_read_input_tokens: Option<i64>,
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
    #[serde(default)]
    kind: Option<String>,
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

#[derive(Debug, Deserialize)]
struct MessagesErrorEnvelope {
    error: ErrorPayload,
}

fn messages_api_error(error: ErrorPayload) -> ApiError {
    match error.kind.as_str() {
        "invalid_request_error" => ApiError::InvalidRequest {
            message: error.message,
        },
        "rate_limit_error" => ApiError::RateLimit(error.message),
        "overloaded_error" => ApiError::Retryable {
            message: error.message,
            delay: None,
        },
        _ => ApiError::Stream(error.message),
    }
}

fn parse_messages_event(event_name: &str, data: &str) -> Result<MessagesEvent, serde_json::Error> {
    let mut value = serde_json::from_str::<Value>(data)?;
    if let Some(object) = value.as_object_mut()
        && !object.contains_key("type")
        && !event_name.is_empty()
        && event_name != "message"
    {
        object.insert("type".to_string(), Value::String(event_name.to_string()));
    }

    serde_json::from_value(value)
}

pub async fn process_messages_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
) {
    let mut stream = stream.eventsource();
    let mut assistant_item: Option<TranscriptItem> = None;
    let mut assistant_item_id: Option<String> = None;
    let mut tool_uses: HashMap<usize, ToolUseState> = HashMap::new();
    let mut completed_tool_calls = Vec::new();
    let mut usage_state = UsageState::default();
    let mut response_id = String::new();
    let mut stop_reason: Option<String> = None;
    let mut assistant_plan_parser: Option<ProposedPlanParser> = None;

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
                    .send(Err(ApiError::Stream(
                        "messages stream closed before message_stop".to_string(),
                    )))
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

        let event: MessagesEvent = match parse_messages_event(&sse.event, &sse.data) {
            Ok(event) => event,
            Err(err) => {
                debug!(
                    event_name = sse.event.as_str(),
                    "failed to parse Messages SSE event: {err}"
                );
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
                        usage_state.apply(usage);
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
                match delta.kind.as_deref() {
                    Some("text_delta") => {
                        let text = delta.text.unwrap_or_default();
                        let assistant_item_id = assistant_item_id
                            .get_or_insert_with(next_assistant_item_id)
                            .clone();
                        append_assistant_text(
                            &tx_event,
                            &mut assistant_item,
                            &mut assistant_plan_parser,
                            assistant_item_id,
                            text,
                        )
                        .await;
                    }
                    Some("input_json_delta") => {
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

                completed_tool_calls.push(TranscriptItem::ToolCall {
                    id: None,
                    call_id,
                    tool_name: name,
                    payload: ToolCallPayload::JsonArguments {
                        arguments: input_json,
                    },
                });
            }
            "message_delta" => {
                if let Some(delta) = event.delta
                    && let Some(reason) = delta.stop_reason
                {
                    stop_reason = Some(reason);
                }
                if let Some(usage) = event.usage {
                    usage_state.apply(usage);
                }
            }
            "message_stop" => {
                if let Some(assistant) = assistant_item.take() {
                    emit_buffered_plan_events(&tx_event, &mut assistant_plan_parser, &assistant)
                        .await;
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
                for tool_call in completed_tool_calls {
                    let _ = tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(tool_call)))
                        .await;
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
                let _ = tx_event.send(Err(messages_api_error(error))).await;
                return;
            }
            "ping" => {}
            _ => {}
        }
    }
}

async fn append_assistant_text(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    assistant_item: &mut Option<TranscriptItem>,
    assistant_plan_parser: &mut Option<ProposedPlanParser>,
    assistant_item_id: String,
    text: String,
) {
    if assistant_item.is_none() {
        let item = TranscriptItem::Message {
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

    let parser = assistant_plan_parser.get_or_insert_with(ProposedPlanParser::new);
    emit_plan_deltas(tx_event, parser.parse(&text)).await;

    if let Some(TranscriptItem::Message { content, .. }) = assistant_item {
        if let Some(ContentItem::OutputText {
            text: aggregated_text,
        }) = content.last_mut()
        {
            aggregated_text.push_str(&text);
        } else {
            content.push(ContentItem::OutputText { text: text.clone() });
        }
        let _ = tx_event
            .send(Ok(ResponseEvent::OutputTextDelta(text)))
            .await;
    }
}

async fn emit_plan_deltas(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    segments: Vec<ProposedPlanSegment>,
) {
    for segment in segments {
        if let ProposedPlanSegment::ProposedPlanDelta(delta) = segment {
            let _ = tx_event
                .send(Ok(ResponseEvent::ProposedPlanDelta(delta)))
                .await;
        }
    }
}

async fn emit_buffered_plan_events(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    assistant_plan_parser: &mut Option<ProposedPlanParser>,
    assistant: &TranscriptItem,
) {
    if let Some(parser) = assistant_plan_parser.as_mut() {
        emit_plan_deltas(tx_event, parser.finish()).await;
    }
    *assistant_plan_parser = None;

    if let TranscriptItem::Message { content, .. } = assistant {
        let mut text = String::new();
        for entry in content {
            if let ContentItem::OutputText { text: chunk } = entry {
                text.push_str(chunk);
            }
        }
        if let Some(plan_text) = extract_proposed_plan_text(&text) {
            let _ = tx_event
                .send(Ok(ResponseEvent::ProposedPlanDone(plan_text)))
                .await;
        }
    }
}

fn next_assistant_item_id() -> String {
    let next_id = NEXT_ASSISTANT_ITEM_ID.fetch_add(1, Ordering::Relaxed);
    format!("messages-assistant-{next_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TranscriptItem;
    use assert_matches::assert_matches;
    use futures::TryStreamExt;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc;
    use tokio_util::io::ReaderStream;

    fn build_stream(body: &str) -> ByteStream {
        let reader = std::io::Cursor::new(body.to_string());
        let stream = ReaderStream::new(reader)
            .map_err(|err| crate::client::TransportError::Network(err.to_string()));
        Box::pin(stream)
    }

    async fn collect_event_results(body: &str) -> Vec<Result<ResponseEvent, ApiError>> {
        let (tx, mut rx) = mpsc::channel(16);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
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
            ResponseEvent::OutputItemDone(TranscriptItem::Message { .. })
        ));
        assert!(matches!(
            events[4],
            ResponseEvent::OutputItemDone(TranscriptItem::ToolCall { .. })
        ));
        assert!(matches!(events[5], ResponseEvent::Completed { .. }));
    }

    #[tokio::test]
    async fn eof_before_message_stop_is_stream_error() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"src/main.rs\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n"
        );

        let events = collect_event_results(body).await;

        assert_matches!(
            &events[..],
            [
                Ok(ResponseEvent::Created),
                Err(ApiError::Stream(message)),
            ] if message == "messages stream closed before message_stop"
        );
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Ok(ResponseEvent::OutputItemDone(
                    TranscriptItem::ToolCall { .. }
                )) | Ok(ResponseEvent::Completed { .. })
            )
        }));
    }

    #[tokio::test]
    async fn parses_usage_when_data_type_missing_and_event_name_present() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":12}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: message_delta\n",
            "data: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(16);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.expect("event should succeed"));
        }

        let ResponseEvent::Completed { token_usage, .. } =
            events.last().expect("completed event should be emitted")
        else {
            panic!("unexpected final event: {:?}", events.last());
        };

        assert_eq!(
            token_usage.as_ref(),
            Some(&TokenUsage {
                input_tokens: 12,
                cached_input_tokens: 0,
                output_tokens: 5,
                reasoning_output_tokens: 0,
                total_tokens: 17,
            })
        );
    }

    #[tokio::test]
    async fn message_delta_without_delta_type_preserves_stop_reason() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(16);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        assert!(matches!(
            rx.recv().await.expect("created event").expect("created"),
            ResponseEvent::Created
        ));
        let err = rx
            .recv()
            .await
            .expect("error event")
            .expect_err("max_tokens should be an error");
        assert_eq!(
            err.to_string(),
            "stream error: messages response reached max_tokens limit"
        );
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn parses_cache_usage_from_messages_stream() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":100,\"cache_creation_input_tokens\":20,\"cache_read_input_tokens\":30,\"output_tokens\":7}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(16);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.expect("event should succeed"));
        }

        let ResponseEvent::Completed { token_usage, .. } =
            events.last().expect("completed event should be emitted")
        else {
            panic!("unexpected final event: {:?}", events.last());
        };

        assert_eq!(
            token_usage.as_ref(),
            Some(&TokenUsage {
                input_tokens: 150,
                cached_input_tokens: 30,
                output_tokens: 7,
                reasoning_output_tokens: 0,
                total_tokens: 157,
            })
        );
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

        let ResponseEvent::OutputItemAdded(TranscriptItem::Message {
            id: Some(added_id), ..
        }) = &events[1]
        else {
            panic!("unexpected added event: {:?}", events[1]);
        };

        let ResponseEvent::OutputItemDone(TranscriptItem::Message {
            id: Some(completed_id),
            ..
        }) = &events[3]
        else {
            panic!("unexpected completed event: {:?}", events[3]);
        };

        assert_eq!(added_id, "msg_1");
        assert_eq!(completed_id, "msg_1");

        let ResponseEvent::OutputItemDone(TranscriptItem::Message { content, .. }) = &events[3]
        else {
            panic!("unexpected completed event: {:?}", events[3]);
        };

        assert_eq!(
            content,
            &vec![ContentItem::OutputText {
                text: "Hello".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn aggregates_text_deltas_into_single_output_item_text() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hey there! \"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"How can \"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"I help you today?\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(8);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.expect("event should succeed"));
        }

        assert_eq!(events.len(), 7);
        assert!(matches!(events[0], ResponseEvent::Created));
        assert!(matches!(
            &events[1],
            ResponseEvent::OutputItemAdded(TranscriptItem::Message {
                id: Some(id),
                role,
                content,
                end_turn: None,
            }) if id == "msg_1" && role == "assistant" && content.is_empty()
        ));
        assert!(matches!(
            &events[2],
            ResponseEvent::OutputTextDelta(delta) if delta == "Hey there! "
        ));
        assert!(matches!(
            &events[3],
            ResponseEvent::OutputTextDelta(delta) if delta == "How can "
        ));
        assert!(matches!(
            &events[4],
            ResponseEvent::OutputTextDelta(delta) if delta == "I help you today?"
        ));

        let ResponseEvent::OutputItemDone(TranscriptItem::Message {
            id: Some(id),
            role,
            content,
            end_turn: None,
        }) = &events[5]
        else {
            panic!("unexpected completed event: {:?}", events[5]);
        };

        assert_eq!(id, "msg_1");
        assert_eq!(role, "assistant");
        assert_eq!(
            content,
            &vec![ContentItem::OutputText {
                text: "Hey there! How can I help you today?".to_string(),
            }]
        );
        assert!(matches!(
            &events[6],
            ResponseEvent::Completed {
                response_id,
                token_usage: None,
            } if response_id == "msg_1"
        ));
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

        let ResponseEvent::OutputItemAdded(TranscriptItem::Message {
            id: Some(added_id), ..
        }) = &events[1]
        else {
            panic!("unexpected added event: {:?}", events[1]);
        };

        let ResponseEvent::OutputItemDone(TranscriptItem::Message {
            id: Some(completed_id),
            ..
        }) = &events[3]
        else {
            panic!("unexpected completed event: {:?}", events[3]);
        };

        assert_eq!(added_id, completed_id);
        assert!(added_id.starts_with("messages-assistant-"));
    }

    #[tokio::test]
    async fn emits_proposed_plan_events_for_plan_block() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Intro\\n<proposed_plan>\\n- Step 1\\n</proposed_plan>\\nOutro\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let (tx, mut rx) = mpsc::channel(16);
        process_messages_sse(build_stream(body), tx, Duration::from_secs(1), None).await;

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.expect("event should succeed"));
        }

        assert!(events.iter().any(|event| {
            matches!(
                event,
                ResponseEvent::ProposedPlanDelta(delta) if delta == "- Step 1\n"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                ResponseEvent::ProposedPlanDone(text) if text == "- Step 1\n"
            )
        }));
    }

    #[test]
    fn parses_non_streaming_messages_response_with_cache_usage_and_tool_call() {
        let completion = parse_completed_response(serde_json::json!({
            "id": "msg-1",
            "content": [
                {
                    "type": "text",
                    "text": "I will inspect it."
                },
                {
                    "type": "tool_use",
                    "id": "toolu-1",
                    "name": "read_file",
                    "input": {"path": "README.md"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 10,
                "cache_creation_input_tokens": 2,
                "cache_read_input_tokens": 3,
                "output_tokens": 4
            }
        }))
        .expect("completion should parse");

        assert_eq!(completion.response_id, "msg-1");
        assert_eq!(
            completion.token_usage,
            Some(TokenUsage {
                input_tokens: 15,
                cached_input_tokens: 3,
                output_tokens: 4,
                reasoning_output_tokens: 0,
                total_tokens: 19,
            })
        );
        assert_eq!(completion.output.len(), 2);
        assert!(matches!(
            &completion.output[0],
            TranscriptItem::Message { id: Some(id), role, content, .. }
                if id == "msg-1"
                    && role == "assistant"
                    && content == &vec![ContentItem::OutputText {
                        text: "I will inspect it.".to_string(),
                    }]
        ));
        assert!(matches!(
            &completion.output[1],
            TranscriptItem::ToolCall {
                call_id,
                tool_name,
                payload: ToolCallPayload::JsonArguments { arguments },
                ..
            } if call_id == "toolu-1" && tool_name == "read_file" && arguments == "{\"path\":\"README.md\"}"
        ));
    }

    #[test]
    fn non_streaming_messages_max_tokens_is_error() {
        let err = parse_completed_response(serde_json::json!({
            "id": "msg-1",
            "content": [{"type": "text", "text": "partial"}],
            "stop_reason": "max_tokens"
        }))
        .expect_err("max_tokens response should fail");

        assert_eq!(
            err.to_string(),
            "stream error: messages response reached max_tokens limit"
        );
    }

    #[test]
    fn non_streaming_messages_rejects_non_object_tool_input() {
        let err = parse_completed_response(serde_json::json!({
            "id": "msg-1",
            "content": [{
                "type": "tool_use",
                "id": "toolu-1",
                "name": "read_file",
                "input": "README.md"
            }],
            "stop_reason": "tool_use"
        }))
        .expect_err("non-object tool input should fail");

        assert_eq!(
            err.to_string(),
            "stream error: messages tool_use input did not form a JSON object"
        );
    }

    #[test]
    fn non_streaming_messages_error_envelope_maps_invalid_request() {
        let err = parse_completed_response(serde_json::json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": "bad request"
            }
        }))
        .expect_err("invalid request envelope should fail");

        assert_matches!(
            err,
            ApiError::InvalidRequest { message } if message == "bad request"
        );
    }

    #[test]
    fn non_streaming_messages_error_envelope_maps_rate_limit() {
        let err = parse_completed_response(serde_json::json!({
            "type": "error",
            "error": {
                "type": "rate_limit_error",
                "message": "slow down"
            }
        }))
        .expect_err("rate limit envelope should fail");

        assert_matches!(err, ApiError::RateLimit(message) if message == "slow down");
    }

    #[test]
    fn non_streaming_messages_error_envelope_maps_overloaded() {
        let err = parse_completed_response(serde_json::json!({
            "type": "error",
            "error": {
                "type": "overloaded_error",
                "message": "try again"
            }
        }))
        .expect_err("overloaded envelope should fail");

        assert_matches!(
            err,
            ApiError::Retryable {
                message,
                delay: None
            } if message == "try again"
        );
    }
}
