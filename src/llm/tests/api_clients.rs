use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use http::HeaderMap;
use http::StatusCode;
use lha_llm::api::AuthProvider;
use lha_llm::api::ChatClient;
use lha_llm::api::MessagesClient;
use lha_llm::api::Provider;
use lha_llm::api::ResponsesClient;
use lha_llm::api::ResponsesOptions;
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

fn assert_path_ends_with(requests: &[Request], suffix: &str) {
    assert_eq!(requests.len(), 1);
    let url = &requests[0].url;
    assert!(
        url.ends_with(suffix),
        "expected url to end with {suffix}, got {url}"
    );
}

#[derive(Debug, Default, Clone)]
struct RecordingState {
    stream_requests: Arc<Mutex<Vec<Request>>>,
    execute_requests: Arc<Mutex<Vec<Request>>>,
}

impl RecordingState {
    fn record_stream(&self, req: Request) {
        let mut guard = self
            .stream_requests
            .lock()
            .unwrap_or_else(|err| panic!("mutex poisoned: {err}"));
        guard.push(req);
    }

    fn record_execute(&self, req: Request) {
        let mut guard = self
            .execute_requests
            .lock()
            .unwrap_or_else(|err| panic!("mutex poisoned: {err}"));
        guard.push(req);
    }

    fn take_stream_requests(&self) -> Vec<Request> {
        let mut guard = self
            .stream_requests
            .lock()
            .unwrap_or_else(|err| panic!("mutex poisoned: {err}"));
        std::mem::take(&mut *guard)
    }

    fn take_execute_requests(&self) -> Vec<Request> {
        let mut guard = self
            .execute_requests
            .lock()
            .unwrap_or_else(|err| panic!("mutex poisoned: {err}"));
        std::mem::take(&mut *guard)
    }
}

#[derive(Clone)]
struct RecordingTransport {
    state: RecordingState,
    execute_response: Response,
}

impl RecordingTransport {
    fn new(state: RecordingState) -> Self {
        Self::with_execute_body(
            state,
            serde_json::json!({
                "id": "resp-1",
                "output": [],
            }),
        )
    }

    fn with_execute_body(state: RecordingState, body: Value) -> Self {
        Self::with_execute_body_and_headers(state, body, HeaderMap::new())
    }

    fn with_execute_body_and_headers(
        state: RecordingState,
        body: Value,
        headers: HeaderMap,
    ) -> Self {
        Self {
            state,
            execute_response: Response {
                status: StatusCode::OK,
                headers,
                body: Bytes::from(body.to_string()),
            },
        }
    }
}

#[async_trait]
impl HttpTransport for RecordingTransport {
    async fn execute(&self, req: Request) -> Result<Response, TransportError> {
        self.state.record_execute(req);
        Ok(self.execute_response.clone())
    }

    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError> {
        self.state.record_stream(req);

        let stream = futures::stream::iter(Vec::<Result<Bytes, TransportError>>::new());
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

#[derive(Clone)]
struct StaticAuth {
    token: String,
}

impl StaticAuth {
    fn new(token: &str) -> Self {
        Self {
            token: token.to_string(),
        }
    }
}

impl AuthProvider for StaticAuth {
    fn bearer_token(&self) -> Option<String> {
        Some(self.token.clone())
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
        stream_idle_timeout: Duration::from_millis(10),
    }
}

#[derive(Clone)]
struct FlakyTransport {
    state: Arc<Mutex<i64>>,
}

impl Default for FlakyTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl FlakyTransport {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(0)),
        }
    }

    fn attempts(&self) -> i64 {
        *self
            .state
            .lock()
            .unwrap_or_else(|err| panic!("mutex poisoned: {err}"))
    }
}

#[async_trait]
impl HttpTransport for FlakyTransport {
    async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
        Err(TransportError::Build("execute should not run".to_string()))
    }

    async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
        let mut attempts = self
            .state
            .lock()
            .unwrap_or_else(|err| panic!("mutex poisoned: {err}"));
        *attempts += 1;

        if *attempts == 1 {
            return Err(TransportError::Network("first attempt fails".to_string()));
        }

        let stream = futures::stream::iter(vec![Ok(Bytes::from(
            r#"event: message
data: {"id":"resp-1","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}]}

"#,
        ))]);

        Ok(StreamResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(stream),
        })
    }
}

#[derive(Clone)]
struct FlakyExecuteTransport {
    state: Arc<Mutex<i64>>,
}

impl FlakyExecuteTransport {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(0)),
        }
    }

    fn attempts(&self) -> i64 {
        *self
            .state
            .lock()
            .unwrap_or_else(|err| panic!("mutex poisoned: {err}"))
    }
}

#[async_trait]
impl HttpTransport for FlakyExecuteTransport {
    async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
        let mut attempts = self
            .state
            .lock()
            .unwrap_or_else(|err| panic!("mutex poisoned: {err}"));
        *attempts += 1;

        if *attempts == 1 {
            return Err(TransportError::Network("first attempt fails".to_string()));
        }

        Ok(Response {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from(
                serde_json::json!({
                    "id": "resp-1",
                    "output": []
                })
                .to_string(),
            ),
        })
    }

    async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
        Err(TransportError::Build("stream should not run".to_string()))
    }
}

#[tokio::test]
async fn chat_client_uses_chat_completions_path_for_chat_wire() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::new(state.clone());
    let client = ChatClient::new(transport, provider("openai", WireApi::Chat), NoAuth);

    let body = serde_json::json!({ "echo": true });
    let _stream = client.stream(body, HeaderMap::new()).await?;

    let requests = state.take_stream_requests();
    assert_path_ends_with(&requests, "/chat/completions");
    Ok(())
}

#[tokio::test]
async fn chat_client_uses_responses_path_for_responses_wire() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::new(state.clone());
    let client = ChatClient::new(transport, provider("openai", WireApi::Responses), NoAuth);

    let body = serde_json::json!({ "echo": true });
    let _stream = client.stream(body, HeaderMap::new()).await?;

    let requests = state.take_stream_requests();
    assert_path_ends_with(&requests, "/responses");
    Ok(())
}

#[tokio::test]
async fn responses_client_uses_responses_path_for_responses_wire() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::new(state.clone());
    let client = ResponsesClient::new(transport, provider("openai", WireApi::Responses), NoAuth);

    let body = serde_json::json!({ "echo": true });
    let _stream = client
        .stream(body, HeaderMap::new(), Compression::None, None)
        .await?;

    let requests = state.take_stream_requests();
    assert_path_ends_with(&requests, "/responses");
    Ok(())
}

#[tokio::test]
async fn responses_client_uses_chat_path_for_chat_wire() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::new(state.clone());
    let client = ResponsesClient::new(transport, provider("openai", WireApi::Chat), NoAuth);

    let body = serde_json::json!({ "echo": true });
    let _stream = client
        .stream(body, HeaderMap::new(), Compression::None, None)
        .await?;

    let requests = state.take_stream_requests();
    assert_path_ends_with(&requests, "/chat/completions");
    Ok(())
}

#[tokio::test]
async fn messages_client_uses_messages_path_for_messages_wire() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::new(state.clone());
    let client = MessagesClient::new(transport, provider("anthropic", WireApi::Messages), NoAuth);

    let body = serde_json::json!({ "echo": true });
    let _stream = client
        .stream_request(lha_llm::api::MessagesRequest {
            body,
            headers: HeaderMap::new(),
        })
        .await?;

    let requests = state.take_stream_requests();
    assert_path_ends_with(&requests, "/messages");
    Ok(())
}

#[tokio::test]
async fn messages_wire_uses_x_api_key_auth() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::new(state.clone());
    let auth = StaticAuth::new("secret-token");
    let client = MessagesClient::new(transport, provider("anthropic", WireApi::Messages), auth);

    let body = serde_json::json!({ "model": "claude-test" });
    let _stream = client
        .stream_request(lha_llm::api::MessagesRequest {
            body,
            headers: HeaderMap::new(),
        })
        .await?;

    let requests = state.take_stream_requests();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];

    assert_eq!(
        req.headers.get("x-api-key").and_then(|h| h.to_str().ok()),
        Some("secret-token")
    );
    assert!(req.headers.get(http::header::AUTHORIZATION).is_none());
    Ok(())
}

#[tokio::test]
async fn streaming_client_adds_auth_headers() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::new(state.clone());
    let auth = StaticAuth::new("secret-token");
    let client = ResponsesClient::new(transport, provider("openai", WireApi::Responses), auth);

    let body = serde_json::json!({ "model": "gpt-test" });
    let _stream = client
        .stream(body, HeaderMap::new(), Compression::None, None)
        .await?;

    let requests = state.take_stream_requests();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];

    let auth_header = req.headers.get(http::header::AUTHORIZATION);
    assert!(auth_header.is_some(), "missing auth header");
    assert_eq!(
        auth_header.unwrap().to_str().ok(),
        Some("Bearer secret-token")
    );

    let accept_header = req.headers.get(http::header::ACCEPT);
    assert!(accept_header.is_some(), "missing Accept header");
    assert_eq!(
        accept_header.unwrap().to_str().ok(),
        Some("text/event-stream")
    );
    assert_eq!(
        req.body
            .as_ref()
            .and_then(|body| body.get("stream"))
            .and_then(Value::as_bool),
        Some(true)
    );
    Ok(())
}

#[tokio::test]
async fn non_streaming_responses_client_uses_execute_and_forces_stream_false() -> Result<()> {
    let state = RecordingState::default();
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "x-codex-turn-state",
        http::HeaderValue::from_static("next-turn-state"),
    );
    let transport = RecordingTransport::with_execute_body_and_headers(
        state.clone(),
        serde_json::json!({
            "id": "resp-1",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]
            }]
        }),
        response_headers,
    );
    let client = ResponsesClient::new(
        transport,
        provider("openai", WireApi::Responses),
        StaticAuth::new("secret-token"),
    );

    let turn_state = Arc::new(OnceLock::new());
    let response = client
        .complete_request(
            lha_llm::api::ResponsesRequest {
                body: serde_json::json!({ "model": "gpt-test", "stream": true }),
                headers: HeaderMap::new(),
                compression: Compression::Zstd,
            },
            Some(Arc::clone(&turn_state)),
        )
        .await?;

    assert_eq!(response.response_id, "resp-1");
    assert_eq!(
        turn_state.get().map(String::as_str),
        Some("next-turn-state")
    );
    assert!(state.take_stream_requests().is_empty());
    let requests = state.take_execute_requests();
    assert_path_ends_with(&requests, "/responses");
    let request = &requests[0];
    assert_eq!(
        request
            .body
            .as_ref()
            .and_then(|body| body.get("stream"))
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        request.compression,
        lha_llm::client::RequestCompression::Zstd
    );
    assert!(request.headers.get(http::header::ACCEPT).is_none());
    assert_eq!(
        request
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|header| header.to_str().ok()),
        Some("Bearer secret-token")
    );
    Ok(())
}

#[tokio::test]
async fn non_streaming_chat_client_uses_execute_and_forces_stream_false() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::with_execute_body(
        state.clone(),
        serde_json::json!({
            "id": "chat-1",
            "choices": [{
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }]
        }),
    );
    let client = ChatClient::new(transport, provider("openai", WireApi::Chat), NoAuth);

    let response = client
        .complete_request(lha_llm::api::ChatRequest {
            body: serde_json::json!({ "model": "gpt-test", "stream": true }),
            headers: HeaderMap::new(),
        })
        .await?;

    assert_eq!(response.response_id, "chat-1");
    let requests = state.take_execute_requests();
    assert_path_ends_with(&requests, "/chat/completions");
    assert_eq!(
        requests[0]
            .body
            .as_ref()
            .and_then(|body| body.get("stream"))
            .and_then(Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[tokio::test]
async fn non_streaming_messages_client_uses_execute_and_forces_stream_false() -> Result<()> {
    let state = RecordingState::default();
    let transport = RecordingTransport::with_execute_body(
        state.clone(),
        serde_json::json!({
            "id": "msg-1",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 1}
        }),
    );
    let client = MessagesClient::new(transport, provider("anthropic", WireApi::Messages), NoAuth);

    let response = client
        .complete_request(lha_llm::api::MessagesRequest {
            body: serde_json::json!({ "model": "claude-test", "stream": true }),
            headers: HeaderMap::new(),
        })
        .await?;

    assert_eq!(response.response_id, "msg-1");
    let requests = state.take_execute_requests();
    assert_path_ends_with(&requests, "/messages");
    assert_eq!(
        requests[0]
            .body
            .as_ref()
            .and_then(|body| body.get("stream"))
            .and_then(Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[tokio::test]
async fn streaming_client_retries_on_transport_error() -> Result<()> {
    let transport = FlakyTransport::new();

    let mut provider = provider("openai", WireApi::Responses);
    provider.retry.max_attempts = 2;

    let client = ResponsesClient::new(transport.clone(), provider, NoAuth);

    let prompt = lha_llm::api::Prompt {
        instructions: "Say hi".to_string(),
        input: vec![TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hi".to_string(),
            }],
            end_turn: None,
        }],
        tools: Vec::<Value>::new(),
        parallel_tool_calls: false,
        output_schema: None,
    };

    let options = ResponsesOptions::default();

    let _stream = client.stream_prompt("gpt-test", &prompt, options).await?;
    assert_eq!(transport.attempts(), 2);
    Ok(())
}

#[tokio::test]
async fn non_streaming_client_retries_on_transport_error() -> Result<()> {
    let transport = FlakyExecuteTransport::new();
    let mut provider = provider("openai", WireApi::Responses);
    provider.retry.max_attempts = 2;
    let client = ResponsesClient::new(transport.clone(), provider, NoAuth);

    let response = client
        .complete_request(
            lha_llm::api::ResponsesRequest {
                body: serde_json::json!({ "model": "gpt-test" }),
                headers: HeaderMap::new(),
                compression: Compression::None,
            },
            None,
        )
        .await?;

    assert_eq!(response.response_id, "resp-1");
    assert_eq!(transport.attempts(), 2);
    Ok(())
}
