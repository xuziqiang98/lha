use crate::auth::AuthProvider;
use crate::common::Prompt as ApiPrompt;
use crate::common::ResponseStream;
use crate::endpoint::streaming::StreamingClient;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::provider::WireApi;
use crate::requests::MessagesRequest;
use crate::requests::MessagesRequestBuilder;
use crate::sse::spawn_messages_stream;
use crate::telemetry::SseTelemetry;
use codex_client::HttpTransport;
use codex_client::RequestCompression;
use codex_client::RequestTelemetry;
use http::HeaderMap;
use std::sync::Arc;

pub struct MessagesClient<T: HttpTransport, A: AuthProvider> {
    streaming: StreamingClient<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> MessagesClient<T, A> {
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

    pub async fn stream_request(
        &self,
        request: MessagesRequest,
    ) -> Result<ResponseStream, ApiError> {
        self.stream(request.body, request.headers).await
    }

    pub async fn stream_prompt(
        &self,
        model: &str,
        prompt: &ApiPrompt,
    ) -> Result<ResponseStream, ApiError> {
        let request =
            MessagesRequestBuilder::new(model, &prompt.instructions, &prompt.input, &prompt.tools)
                .parallel_tool_calls(prompt.parallel_tool_calls)
                .build(self.streaming.provider())?;
        self.stream_request(request).await
    }

    async fn stream(
        &self,
        body: serde_json::Value,
        extra_headers: HeaderMap,
    ) -> Result<ResponseStream, ApiError> {
        if self.streaming.provider().wire != WireApi::Messages {
            return Err(ApiError::Stream(
                "messages wire api requires MessagesClient".to_string(),
            ));
        }

        self.streaming
            .stream(
                "messages",
                body,
                extra_headers,
                RequestCompression::None,
                spawn_messages_stream,
                None,
            )
            .await
    }
}
