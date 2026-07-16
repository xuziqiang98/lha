use anyhow::Result;
use futures::StreamExt;
use lha_llm::ContentItem;
use lha_llm::DefaultRuntimeClientFactory;
use lha_llm::Error;
use lha_llm::ModelInfo;
use lha_llm::ResponseDelivery;
use lha_llm::RuntimeBuildSpec;
use lha_llm::RuntimeClientFactory;
use lha_llm::RuntimeEndpoint;
use lha_llm::RuntimeNotice;
use lha_llm::RuntimeNoticeKind;
use lha_llm::SemanticRuntimeSession;
use lha_llm::TranscriptItem;
use lha_llm::TurnEvent;
use lha_llm::TurnRequest;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

struct SeqResponder {
    num_calls: AtomicUsize,
    responses: Vec<ResponseTemplate>,
}

impl Respond for SeqResponder {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let call_num = self.num_calls.fetch_add(1, Ordering::SeqCst);
        self.responses
            .get(call_num)
            .unwrap_or_else(|| panic!("no response for request {call_num}"))
            .clone()
    }
}

fn turn_request() -> TurnRequest {
    TurnRequest {
        conversation: vec![TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hello".to_string(),
            }],
            end_turn: None,
        }],
        ..Default::default()
    }
}

fn runtime(endpoint: RuntimeEndpoint) -> std::sync::Arc<dyn lha_llm::SemanticRuntime> {
    DefaultRuntimeClientFactory::new()
        .build_client(RuntimeBuildSpec::builder(endpoint, ModelInfo::minimal("test-model")).build())
}

async fn collect_events(
    session: &mut Box<dyn SemanticRuntimeSession>,
    request: &TurnRequest,
) -> Result<Vec<TurnEvent>> {
    let mut stream = session
        .run_turn_with_delivery(request, ResponseDelivery::NonStreaming)
        .await?;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }
    Ok(events)
}

fn assert_non_streaming_message_events(events: &[TurnEvent], response_id: &str, text: &str) {
    assert!(events.iter().any(|event| {
        matches!(
            event,
            TurnEvent::ItemCompleted {
                item: lha_llm::SemanticOutputItem::AssistantMessage {
                    item: TranscriptItem::Message { content, .. },
                },
                ..
            } if content == &vec![ContentItem::OutputText {
                text: text.to_string(),
            }]
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            TurnEvent::Completed {
                response_id: actual,
                ..
            } if actual == response_id
        )
    }));
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            TurnEvent::OutputTextDelta { .. }
                | TurnEvent::ReasoningContentDelta { .. }
                | TurnEvent::ReasoningSummaryDelta { .. }
                | TurnEvent::ProposedPlanDelta { .. }
        )
    }));
}

async fn assert_stream_flags(server: &MockServer, expected: &[bool]) -> Result<()> {
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| anyhow::anyhow!("mock server should capture requests"))?;
    let actual = requests
        .iter()
        .map(|request| -> Result<bool> {
            let body: Value = serde_json::from_slice(&request.body)?;
            body.get("stream")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow::anyhow!("request should include a boolean stream field"))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(actual, expected);
    Ok(())
}

#[tokio::test]
async fn non_streaming_responses_uses_http_even_when_realtime_is_enabled() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .insert_header("x-models-etag", "models-etag")
                .insert_header("x-reasoning-included", "true")
                .set_body_json(serde_json::json!({
                    "id": "resp-1",
                    "status": "completed",
                    "output": [{
                        "type": "message",
                        "id": "msg-1",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "responses"}]
                    }],
                    "usage": {
                        "input_tokens": 3,
                        "output_tokens": 2,
                        "total_tokens": 5
                    }
                })),
        )
        .mount(&server)
        .await;

    let endpoint = RuntimeEndpoint::openai_compatible_responses("responses", server.uri())
        .with_bearer_token(Some("test-token".to_string()))
        .with_realtime_turn_streaming_enabled(true);
    let client = runtime(endpoint);
    let mut session = client.new_session();
    let events = collect_events(&mut session, &turn_request()).await?;

    assert_non_streaming_message_events(&events, "resp-1", "responses");
    assert!(
        events
            .iter()
            .any(|event| { matches!(event, TurnEvent::ModelsEtag(etag) if etag == "models-etag") })
    );
    assert!(
        events
            .iter()
            .any(|event| { matches!(event, TurnEvent::ServerReasoningIncluded(true)) })
    );
    assert_stream_flags(&server, &[false]).await?;
    Ok(())
}

#[tokio::test]
async fn non_streaming_chat_uses_complete_response() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(serde_json::json!({
                    "id": "chat-1",
                    "choices": [{
                        "message": {"role": "assistant", "content": "chat"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 3,
                        "completion_tokens": 1,
                        "total_tokens": 4
                    }
                })),
        )
        .mount(&server)
        .await;

    let endpoint = RuntimeEndpoint::openai_compatible_chat("chat", server.uri())
        .with_bearer_token(Some("test-token".to_string()));
    let client = runtime(endpoint);
    let mut session = client.new_session();
    let events = collect_events(&mut session, &turn_request()).await?;

    assert_non_streaming_message_events(&events, "chat-1", "chat");
    assert_stream_flags(&server, &[false]).await?;
    Ok(())
}

#[tokio::test]
async fn non_streaming_messages_uses_complete_response() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(serde_json::json!({
                    "id": "msg-1",
                    "content": [{"type": "text", "text": "messages"}],
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": 3,
                        "output_tokens": 1
                    }
                })),
        )
        .mount(&server)
        .await;

    let endpoint = RuntimeEndpoint::anthropic_compatible_messages("messages", server.uri())
        .with_bearer_token(Some("test-token".to_string()));
    let client = runtime(endpoint);
    let mut session = client.new_session();
    let events = collect_events(&mut session, &turn_request()).await?;

    assert_non_streaming_message_events(&events, "msg-1", "messages");
    assert_stream_flags(&server, &[false]).await?;
    Ok(())
}

#[tokio::test]
async fn default_run_turn_remains_streaming() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    concat!(
                        "event: response.created\n",
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\"}}\n\n",
                        "event: response.output_item.added\n",
                        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"id\":\"msg-1\",\"role\":\"assistant\",\"content\":[]}}\n\n",
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"streaming\"}\n\n",
                        "event: response.output_item.done\n",
                        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"id\":\"msg-1\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"streaming\"}]}}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\"}}\n\n"
                    ),
                ),
        )
        .mount(&server)
        .await;

    let endpoint = RuntimeEndpoint::openai_compatible_responses("responses", server.uri())
        .with_bearer_token(Some("test-token".to_string()));
    let client = runtime(endpoint);
    let mut session = client.new_session();
    let mut stream = session.run_turn(&turn_request()).await?;
    let mut saw_delta = false;
    while let Some(event) = stream.next().await {
        if matches!(event?, TurnEvent::OutputTextDelta { .. }) {
            saw_delta = true;
        }
    }

    assert!(saw_delta);
    assert_stream_flags(&server, &[true]).await?;
    Ok(())
}

#[tokio::test]
async fn non_streaming_responses_retries_retryable_failure() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(SeqResponder {
            num_calls: AtomicUsize::new(0),
            responses: vec![
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(serde_json::json!({
                        "id": "resp-rate-limited",
                        "status": "failed",
                        "error": {
                            "code": "rate_limit_exceeded",
                            "message": "Please try again in 0ms."
                        }
                    })),
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(serde_json::json!({
                        "id": "resp-retried",
                        "status": "completed",
                        "output": [{
                            "type": "message",
                            "id": "msg-retried",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "retried"}]
                        }],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 2,
                            "total_tokens": 5
                        }
                    })),
            ],
        })
        .mount(&server)
        .await;

    let endpoint = RuntimeEndpoint::openai_compatible_responses("responses", server.uri())
        .with_bearer_token(Some("test-token".to_string()))
        .with_request_max_retries(Some(0))
        .with_stream_max_retries(Some(1))
        .with_realtime_turn_streaming_enabled(true);
    let client = runtime(endpoint);
    let mut session = client.new_session();
    let events = collect_events(&mut session, &turn_request()).await?;

    assert_eq!(
        events.first(),
        Some(&TurnEvent::RuntimeNotice(RuntimeNotice {
            kind: RuntimeNoticeKind::Reconnecting,
            message: "Reconnecting... 1/1".to_string(),
        }))
    );
    assert_non_streaming_message_events(&events, "resp-retried", "retried");
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            TurnEvent::RuntimeNotice(RuntimeNotice {
                kind: RuntimeNoticeKind::TransportFallback,
                ..
            })
        )
    }));
    assert_stream_flags(&server, &[false, false]).await?;
    Ok(())
}

#[tokio::test]
async fn non_streaming_retryable_failure_does_not_fallback_when_retries_are_exhausted() -> Result<()>
{
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(SeqResponder {
            num_calls: AtomicUsize::new(0),
            responses: vec![
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(serde_json::json!({
                        "id": "resp-rate-limited",
                        "status": "failed",
                        "error": {
                            "code": "rate_limit_exceeded",
                            "message": "Please try again in 0ms."
                        }
                    })),
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(serde_json::json!({
                        "id": "resp-should-not-be-requested",
                        "status": "completed",
                        "output": []
                    })),
            ],
        })
        .mount(&server)
        .await;

    let endpoint = RuntimeEndpoint::openai_compatible_responses("responses", server.uri())
        .with_bearer_token(Some("test-token".to_string()))
        .with_request_max_retries(Some(0))
        .with_stream_max_retries(Some(0))
        .with_realtime_turn_streaming_enabled(true);
    let client = runtime(endpoint);
    let mut session = client.new_session();
    let stream = session
        .run_turn_with_delivery(&turn_request(), ResponseDelivery::NonStreaming)
        .await?;
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(events.len(), 1);
    assert!(matches!(
        events.as_slice(),
        [Err(Error::Retryable { message, delay })]
            if message == "Please try again in 0ms."
                && *delay == Some(std::time::Duration::ZERO)
    ));
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Ok(TurnEvent::RuntimeNotice(RuntimeNotice {
                kind: RuntimeNoticeKind::TransportFallback,
                ..
            }))
        )
    }));
    assert_stream_flags(&server, &[false]).await?;
    Ok(())
}

#[tokio::test]
async fn non_streaming_rejects_sse_fixture_path() -> Result<()> {
    let endpoint = RuntimeEndpoint::openai_compatible_responses("responses", "http://127.0.0.1:1")
        .with_bearer_token(Some("test-token".to_string()));
    let runtime = DefaultRuntimeClientFactory::new().build_client(
        RuntimeBuildSpec::builder(endpoint, ModelInfo::minimal("test-model"))
            .sse_fixture_path("fixture.sse")
            .build(),
    );
    let mut session = runtime.new_session();
    let mut stream = session
        .run_turn_with_delivery(&turn_request(), ResponseDelivery::NonStreaming)
        .await?;

    let event = stream
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("non-streaming fixture request should return an error"))?;
    let Err(err) = event else {
        return Err(anyhow::anyhow!("fixture request should fail"));
    };
    assert_eq!(
        err.to_string(),
        "unsupported operation: non-streaming response delivery is incompatible with sse_fixture_path"
    );
    Ok(())
}
