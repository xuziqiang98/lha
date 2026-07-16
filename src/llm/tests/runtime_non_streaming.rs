use anyhow::Result;
use futures::StreamExt;
use lha_llm::ContentItem;
use lha_llm::DefaultRuntimeClientFactory;
use lha_llm::ModelInfo;
use lha_llm::ResponseDelivery;
use lha_llm::RuntimeBuildSpec;
use lha_llm::RuntimeClientFactory;
use lha_llm::RuntimeEndpoint;
use lha_llm::SemanticRuntimeSession;
use lha_llm::TranscriptItem;
use lha_llm::TurnEvent;
use lha_llm::TurnRequest;
use pretty_assertions::assert_eq;
use serde_json::Value;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

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

async fn assert_stream_flag(server: &MockServer, expected: bool) -> Result<()> {
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| anyhow::anyhow!("mock server should capture requests"))?;
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(body.get("stream").and_then(Value::as_bool), Some(expected));
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
    assert_stream_flag(&server, false).await?;
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
    assert_stream_flag(&server, false).await?;
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
    assert_stream_flag(&server, false).await?;
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
    assert_stream_flag(&server, true).await?;
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
