use crate::api::auth::AuthProvider;
use crate::api::common::CompletedResponse;
use crate::api::common::Prompt as ApiPrompt;
use crate::api::common::Reasoning;
use crate::api::common::ResponseEvent;
use crate::api::common::ResponseStream;
use crate::api::common::TextControls;
use crate::api::common::with_streaming;
use crate::api::endpoint::streaming::StreamingClient;
use crate::api::error::ApiError;
use crate::api::provider::Provider;
use crate::api::provider::WireApi;
use crate::api::requests::ResponsesRequest;
use crate::api::requests::ResponsesRequestBuilder;
use crate::api::requests::responses::Compression;
use crate::api::sse::responses::parse_completed_response;
use crate::api::sse::responses::response_header_events;
use crate::api::sse::spawn_response_stream;
use crate::api::telemetry::SseTelemetry;
use crate::client::HttpTransport;
use crate::client::RequestCompression;
use crate::client::RequestTelemetry;
use http::HeaderMap;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::instrument;

pub struct ResponsesClient<T: HttpTransport, A: AuthProvider> {
    streaming: StreamingClient<T, A>,
}

#[derive(Default)]
pub struct ResponsesOptions {
    pub reasoning: Option<Reasoning>,
    pub include: Vec<String>,
    pub prompt_cache_key: Option<String>,
    pub text: Option<TextControls>,
    pub store_override: Option<bool>,
    pub conversation_id: Option<String>,
    pub origin_tag: Option<String>,
    pub extra_headers: HeaderMap,
    pub compression: Compression,
    pub turn_state: Option<Arc<OnceLock<String>>>,
}

impl<T: HttpTransport, A: AuthProvider> ResponsesClient<T, A> {
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
        request: ResponsesRequest,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        self.stream(
            request.body,
            request.headers,
            request.compression,
            turn_state,
        )
        .await
    }

    pub async fn complete_request(
        &self,
        request: ResponsesRequest,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<CompletedResponse, ApiError> {
        self.complete_request_with_events(request, turn_state)
            .await
            .map(|(completion, _)| completion)
    }

    pub(crate) async fn complete_request_with_events(
        &self,
        request: ResponsesRequest,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<(CompletedResponse, Vec<ResponseEvent>), ApiError> {
        self.complete(
            request.body,
            request.headers,
            request.compression,
            turn_state,
        )
        .await
    }

    #[instrument(level = "trace", skip_all, err)]
    pub async fn stream_prompt(
        &self,
        model: &str,
        prompt: &ApiPrompt,
        options: ResponsesOptions,
    ) -> Result<ResponseStream, ApiError> {
        let ResponsesOptions {
            reasoning,
            include,
            prompt_cache_key,
            text,
            store_override,
            conversation_id,
            origin_tag,
            extra_headers,
            compression,
            turn_state,
        } = options;

        let request = ResponsesRequestBuilder::new(model, &prompt.instructions, &prompt.input)
            .tools(&prompt.tools)
            .parallel_tool_calls(prompt.parallel_tool_calls)
            .reasoning(reasoning)
            .include(include)
            .prompt_cache_key(prompt_cache_key)
            .text(text)
            .conversation(conversation_id)
            .origin_tag(origin_tag)
            .store_override(store_override)
            .extra_headers(extra_headers)
            .compression(compression)
            .build(self.streaming.provider())?;

        self.stream_request(request, turn_state).await
    }

    #[instrument(level = "trace", skip_all, err)]
    pub async fn complete_prompt(
        &self,
        model: &str,
        prompt: &ApiPrompt,
        options: ResponsesOptions,
    ) -> Result<CompletedResponse, ApiError> {
        self.complete_prompt_with_events(model, prompt, options)
            .await
            .map(|(completion, _)| completion)
    }

    pub(crate) async fn complete_prompt_with_events(
        &self,
        model: &str,
        prompt: &ApiPrompt,
        options: ResponsesOptions,
    ) -> Result<(CompletedResponse, Vec<ResponseEvent>), ApiError> {
        let ResponsesOptions {
            reasoning,
            include,
            prompt_cache_key,
            text,
            store_override,
            conversation_id,
            origin_tag,
            extra_headers,
            compression,
            turn_state,
        } = options;

        let request = ResponsesRequestBuilder::new(model, &prompt.instructions, &prompt.input)
            .tools(&prompt.tools)
            .parallel_tool_calls(prompt.parallel_tool_calls)
            .reasoning(reasoning)
            .include(include)
            .prompt_cache_key(prompt_cache_key)
            .text(text)
            .conversation(conversation_id)
            .origin_tag(origin_tag)
            .store_override(store_override)
            .extra_headers(extra_headers)
            .compression(compression)
            .build(self.streaming.provider())?;

        self.complete_request_with_events(request, turn_state).await
    }

    fn path(&self) -> &'static str {
        match self.streaming.provider().wire {
            WireApi::Responses | WireApi::Compact => "responses",
            WireApi::Chat => "chat/completions",
            WireApi::Messages => "messages",
        }
    }

    pub async fn stream(
        &self,
        body: Value,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        if self.streaming.provider().wire == WireApi::Messages {
            return Err(ApiError::Stream(
                "messages wire api requires MessagesClient".to_string(),
            ));
        }

        let compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };
        let body = with_streaming(body, true)?;

        self.streaming
            .stream(
                self.path(),
                body,
                extra_headers,
                compression,
                spawn_response_stream,
                turn_state,
            )
            .await
    }

    async fn complete(
        &self,
        body: Value,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<(CompletedResponse, Vec<ResponseEvent>), ApiError> {
        if self.streaming.provider().wire == WireApi::Messages {
            return Err(ApiError::Stream(
                "messages wire api requires MessagesClient".to_string(),
            ));
        }

        let compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };
        let response = self
            .streaming
            .execute(
                self.path(),
                with_streaming(body, false)?,
                extra_headers,
                compression,
            )
            .await?;
        let prefix_events = response_header_events(&response.headers, turn_state);
        let completion =
            parse_completed_response(serde_json::from_slice(&response.body).map_err(|err| {
                ApiError::Stream(format!(
                    "failed to decode non-streaming responses response body: {err}"
                ))
            })?)?;

        Ok((completion, prefix_events))
    }
}
