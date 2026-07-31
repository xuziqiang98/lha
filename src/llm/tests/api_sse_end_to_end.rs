use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use http::HeaderMap;
use http::StatusCode;
use lha_llm::api::AggregateStreamExt;
use lha_llm::api::AuthProvider;
use lha_llm::api::Provider;
use lha_llm::api::ResponseEvent;
use lha_llm::api::ResponsesClient;
use lha_llm::api::WireApi;
use lha_llm::api::requests::responses::Compression;
use lha_llm::client::HttpTransport;
use lha_llm::client::Request;
use lha_llm::client::Response;
use lha_llm::client::StreamResponse;
use lha_llm::client::TransportError;
use lha_llm::types::ContentItem;
use lha_llm::types::TranscriptItem;
use pretty_assertions::assert_eq;
use serde_json::Value;

#[derive(Clone)]
struct FixtureSseTransport {
    body: String,
}

impl FixtureSseTransport {
    fn new(body: String) -> Self {
        Self { body }
    }
}

#[async_trait]
impl HttpTransport for FixtureSseTransport {
    async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
        Err(TransportError::Build("execute should not run".to_string()))
    }

    async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
        let stream = futures::stream::iter(vec![Ok::<Bytes, TransportError>(Bytes::from(
            self.body.clone(),
        ))]);
        Ok(StreamResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(stream),
        })
    }
}

#[derive(Clone, Default)]
struct NoAuth;

impl AuthProvider for NoAuth {
    fn bearer_token(&self) -> Option<String> {
        None
    }
}

fn provider(name: &str, wire: WireApi) -> Provider {
    Provider {
        name: name.to_string(),
        base_url: "https://example.com/v1".to_string(),
        query_params: None,
        wire,
        headers: HeaderMap::new(),
        retry: lha_llm::api::provider::RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: true,
        },
        stream_idle_timeout: Duration::from_millis(50),
    }
}

fn build_responses_body(events: Vec<Value>) -> String {
    let mut body = String::new();
    for e in events {
        let kind = e
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("fixture event missing type in SSE fixture: {e}"));
        if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
            body.push_str(&format!("event: {kind}\n\n"));
        } else {
            body.push_str(&format!("event: {kind}\ndata: {e}\n\n"));
        }
    }
    body
}

#[tokio::test]
async fn responses_stream_parses_items_and_completed_end_to_end() -> Result<()> {
    let item1 = serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Hello"}]
        }
    });

    let item2 = serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "World"}]
        }
    });

    let completed = serde_json::json!({
        "type": "response.completed",
        "response": { "id": "resp1" }
    });

    let body = build_responses_body(vec![item1, item2, completed]);
    let transport = FixtureSseTransport::new(body);
    let client = ResponsesClient::new(transport, provider("openai", WireApi::Responses), NoAuth);

    let mut stream = client
        .stream(
            serde_json::json!({"echo": true}),
            HeaderMap::new(),
            Compression::None,
            None,
        )
        .await?;

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev?);
    }

    let events: Vec<ResponseEvent> = events.into_iter().collect();

    assert_eq!(events.len(), 3);

    match &events[0] {
        ResponseEvent::OutputItemDone(TranscriptItem::Message { role, .. }) => {
            assert_eq!(role, "assistant");
        }
        other => panic!("unexpected first event: {other:?}"),
    }

    match &events[1] {
        ResponseEvent::OutputItemDone(TranscriptItem::Message { role, .. }) => {
            assert_eq!(role, "assistant");
        }
        other => panic!("unexpected second event: {other:?}"),
    }

    match &events[2] {
        ResponseEvent::Completed {
            response_id,
            token_usage,
        } => {
            assert_eq!(response_id, "resp1");
            assert!(token_usage.is_none());
        }
        other => panic!("unexpected third event: {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn responses_stream_aggregates_output_text_deltas() -> Result<()> {
    let delta1 = serde_json::json!({
        "type": "response.output_text.delta",
        "delta": "Hello, "
    });

    let delta2 = serde_json::json!({
        "type": "response.output_text.delta",
        "delta": "world"
    });

    let completed = serde_json::json!({
        "type": "response.completed",
        "response": { "id": "resp-agg" }
    });

    let body = build_responses_body(vec![delta1, delta2, completed]);
    let transport = FixtureSseTransport::new(body);
    let client = ResponsesClient::new(transport, provider("openai", WireApi::Responses), NoAuth);

    let stream = client
        .stream(
            serde_json::json!({"echo": true}),
            HeaderMap::new(),
            Compression::None,
            None,
        )
        .await?;

    let mut stream = stream.aggregate();
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev?);
    }

    let events: Vec<ResponseEvent> = events.into_iter().collect();

    assert_eq!(events.len(), 2);

    match &events[0] {
        ResponseEvent::OutputItemDone(TranscriptItem::Message { content, .. }) => {
            let mut aggregated = String::new();
            for item in content {
                if let ContentItem::OutputText { text } = item {
                    aggregated.push_str(text);
                }
            }
            assert_eq!(aggregated, "Hello, world");
        }
        other => panic!("unexpected first event: {other:?}"),
    }

    match &events[1] {
        ResponseEvent::Completed { response_id, .. } => {
            assert_eq!(response_id, "resp-agg");
        }
        other => panic!("unexpected second event: {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn responses_stream_normalizes_hosted_web_citations_end_to_end() -> Result<()> {
    let marker = "\u{e200}cite\u{e202}turn0search0\u{e201}";
    let raw = format!("Answer{marker}");
    let body = build_responses_body(vec![
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg-1",
                "role": "assistant",
                "content": []
            }
        }),
        serde_json::json!({
            "type": "response.output_text.delta",
            "item_id": "msg-1",
            "output_index": 0,
            "content_index": 0,
            "delta": raw,
        }),
        serde_json::json!({
            "type": "response.output_text.annotation.added",
            "item_id": "msg-1",
            "output_index": 0,
            "content_index": 0,
            "annotation_index": 0,
            "annotation": {
                "type": "url_citation",
                "start_index": 6,
                "end_index": 28,
                "title": "Source",
                "url": "https://example.com/end-to-end"
            }
        }),
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg-1",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": format!("Answer{marker}"),
                    "annotations": [{
                        "type": "url_citation",
                        "start_index": 6,
                        "end_index": 28,
                        "title": "Source",
                        "url": "https://example.com/end-to-end"
                    }]
                }]
            }
        }),
        serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp-citation"}
        }),
    ]);
    let transport = FixtureSseTransport::new(body);
    let client = ResponsesClient::new(transport, provider("openai", WireApi::Responses), NoAuth);
    let mut stream = client
        .stream(
            serde_json::json!({"echo": true}),
            HeaderMap::new(),
            Compression::None,
            None,
        )
        .await?;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }

    let streamed = events
        .iter()
        .filter_map(|event| match event {
            ResponseEvent::OutputTextDelta(delta) => Some(delta.as_str()),
            _ => None,
        })
        .collect::<String>();
    let completed = events.iter().find_map(|event| match event {
        ResponseEvent::OutputItemDone(TranscriptItem::Message { content, .. }) => {
            let [ContentItem::OutputText { text }] = content.as_slice() else {
                return None;
            };
            Some(text.as_str())
        }
        _ => None,
    });
    let expected = "Answer[Source](<https://example.com/end-to-end>)";

    assert_eq!(streamed, expected);
    assert_eq!(completed, Some(expected));
    Ok(())
}
