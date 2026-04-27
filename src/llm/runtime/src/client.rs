use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use adam_api::AggregateStreamExt;
use adam_api::ChatClient as ApiChatClient;
use adam_api::ChatRequestBuilder as ApiChatRequestBuilder;
use adam_api::CompactClient as ApiCompactClient;
use adam_api::CompactionInput as ApiCompactionInput;
use adam_api::DeveloperRoleHandling;
use adam_api::MessagesClient as ApiMessagesClient;
use adam_api::MessagesRequestBuilder as ApiMessagesRequestBuilder;
use adam_api::Prompt as ApiPrompt;
use adam_api::RequestTelemetry;
use adam_api::ReqwestTransport;
use adam_api::ResponseAppendWsRequest;
use adam_api::ResponseCreateWsRequest;
use adam_api::ResponseStream as ApiResponseStream;
use adam_api::ResponsesClient as ApiResponsesClient;
use adam_api::ResponsesOptions as ApiResponsesOptions;
use adam_api::ResponsesWebsocketClient as ApiWebSocketResponsesClient;
use adam_api::ResponsesWebsocketConnection as ApiWebSocketConnection;
use adam_api::SseTelemetry;
use adam_api::TransportError;
use adam_api::WebsocketTelemetry;
use adam_api::build_conversation_headers;
use adam_api::common::Reasoning;
use adam_api::common::ResponsesWsRequest;
use adam_api::create_text_param_for_request;
use adam_api::error::ApiError;
use adam_api::requests::responses::Compression;
use adam_llm_types::ContentItem;
use adam_otel::OtelManager;
use eventsource_stream::Event;
use eventsource_stream::EventStreamError;
use futures::StreamExt;
use http::HeaderMap as ApiHeaderMap;
use http::HeaderValue;
use http::StatusCode as HttpStatusCode;
use reqwest::Client as HttpClient;
use reqwest::StatusCode;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Error as WebsocketError;
use tokio_tungstenite::tungstenite::Message;
use tracing::info;
use tracing::warn;

use crate::Error;
use crate::ModelInfo;
use crate::ReasoningEffort;
use crate::ReasoningSummary;
use crate::Result;
use crate::TokenUsage;
use crate::TranscriptItem;
use crate::WebSearchMode;
use crate::auth::AuthContext;
use crate::auth::AuthSource;
use crate::auth::UnauthorizedRecovery;
use crate::auth::auth_provider_from_context;
use crate::compatibility::ChatRoleCompatibility;
use crate::compatibility::ChatRoleCompatibilityHandle;
use crate::config::LlmRuntimeConfig;
use crate::prompt::Prompt;
use crate::prompt::ResponseEvent;
use crate::prompt::ResponseStream;
use crate::provider::RuntimeEndpoint;
use crate::tool_json::create_tools_json_for_chat_completions_api;
use crate::tool_json::create_tools_json_for_messages_api;
use crate::tool_json::create_tools_json_for_responses_api;
use crate::transport::StreamingPreference;

pub const WEB_SEARCH_ELIGIBLE_HEADER: &str = "x-oai-web-search-eligible";
pub const X_CODEX_TURN_STATE_HEADER: &str = "x-codex-turn-state";
const APPROX_CHARS_PER_TOKEN: usize = 4;
const ANTHROPIC_MESSAGES_MAX_TOKENS: u32 = 8_192;

struct LlmClientState {
    runtime_config: Arc<LlmRuntimeConfig>,
    auth_source: Arc<dyn AuthSource>,
    http_client: HttpClient,
    model_info: ModelInfo,
    chat_role_compatibility: Option<ChatRoleCompatibilityHandle>,
    otel_manager: OtelManager,
    endpoint: RuntimeEndpoint,
    session_id: String,
    effort: Option<ReasoningEffort>,
    summary: ReasoningSummary,
    origin_tag: Option<String>,
    streaming_preference: StreamingPreference,
}

#[derive(Clone)]
pub struct LlmClient {
    state: Arc<LlmClientState>,
}

pub struct LlmClientSession {
    state: Arc<LlmClientState>,
    connection: Option<ApiWebSocketConnection>,
    websocket_last_items: Vec<TranscriptItem>,
    streaming_preference: StreamingPreference,
    turn_state: Arc<OnceLock<String>>,
}

#[allow(clippy::too_many_arguments)]
impl LlmClient {
    pub fn new(
        runtime_config: Arc<LlmRuntimeConfig>,
        auth_source: Arc<dyn AuthSource>,
        http_client: HttpClient,
        model_info: ModelInfo,
        chat_role_compatibility: Option<ChatRoleCompatibilityHandle>,
        otel_manager: OtelManager,
        endpoint: RuntimeEndpoint,
        effort: Option<ReasoningEffort>,
        summary: ReasoningSummary,
        session_id: String,
        origin_tag: Option<String>,
        streaming_preference: StreamingPreference,
    ) -> Self {
        Self {
            state: Arc::new(LlmClientState {
                runtime_config,
                auth_source,
                http_client,
                model_info,
                chat_role_compatibility,
                otel_manager,
                endpoint,
                session_id,
                effort,
                summary,
                origin_tag,
                streaming_preference,
            }),
        }
    }

    pub fn new_session(&self) -> LlmClientSession {
        LlmClientSession {
            state: Arc::clone(&self.state),
            connection: None,
            websocket_last_items: Vec::new(),
            streaming_preference: self.state.streaming_preference.clone(),
            turn_state: Arc::new(OnceLock::new()),
        }
    }

    pub fn estimated_input_tokens_for_prompt(&self, prompt: &Prompt) -> Option<i64> {
        if !self.state.endpoint.uses_chat_completions_api() {
            return None;
        }

        let instructions = prompt.base_instructions.text.clone();
        let tools_json = create_tools_json_for_chat_completions_api(&prompt.tools).ok()?;
        let api_prompt = build_api_prompt(prompt, instructions, tools_json);
        let api_provider = self.state.endpoint.to_api_provider(false).ok()?;
        let handling = self.developer_role_handling();
        let request = ApiChatRequestBuilder::new(
            &self.state.model_info.slug,
            &api_prompt.instructions,
            &api_prompt.input,
            &api_prompt.tools,
        )
        .conversation_id(Some(self.state.session_id.clone()))
        .origin_tag(self.state.origin_tag.clone())
        .developer_role_handling(handling)
        .build(&api_provider)
        .ok()?;

        Some(estimate_chat_input_tokens_from_request_body(&request.body))
    }

    pub async fn compact_conversation_history(
        &self,
        prompt: &Prompt,
    ) -> Result<Vec<TranscriptItem>> {
        if prompt.input.is_empty() {
            return Ok(Vec::new());
        }
        let auth = self.state.auth_source.current_auth().await?;
        let api_provider = self
            .state
            .endpoint
            .to_api_provider(auth.as_ref().is_some_and(|auth| auth.use_chatgpt_base_url))?;
        let api_auth = auth_provider_from_context(auth);
        let transport = ReqwestTransport::new(self.state.http_client.clone());
        let request_telemetry = self.build_request_telemetry();
        let client = ApiCompactClient::new(transport, api_provider, api_auth)
            .with_telemetry(Some(request_telemetry));

        let instructions = prompt.base_instructions.text.clone();
        let formatted_input = prompt.input.clone();
        let payload = ApiCompactionInput {
            model: &self.state.model_info.slug,
            input: &formatted_input,
            instructions: &instructions,
        };

        let mut extra_headers = ApiHeaderMap::new();
        if let Some(origin_tag) = self.state.origin_tag.as_deref()
            && let Ok(val) = HeaderValue::from_str(origin_tag)
        {
            extra_headers.insert("x-openai-subagent", val);
        }
        client
            .compact_input(&payload, extra_headers)
            .await
            .map(|items| items.into_iter().collect())
            .map_err(Into::into)
    }

    fn chat_role_compatibility_status(&self) -> ChatRoleCompatibility {
        self.state
            .chat_role_compatibility
            .as_ref()
            .map(|state| {
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .current()
            })
            .unwrap_or(ChatRoleCompatibility::Unknown)
    }

    fn developer_role_handling(&self) -> DeveloperRoleHandling {
        match self.chat_role_compatibility_status() {
            ChatRoleCompatibility::Unknown | ChatRoleCompatibility::SupportsDeveloper => {
                DeveloperRoleHandling::Preserve
            }
            ChatRoleCompatibility::RequiresSystemForDeveloper => {
                DeveloperRoleHandling::DowngradeToSystem
            }
        }
    }

    fn build_request_telemetry(&self) -> Arc<dyn RequestTelemetry> {
        (Arc::new(ApiTelemetry::new(self.state.otel_manager.clone()))) as _
    }
}

impl LlmClientSession {
    pub async fn stream(&mut self, prompt: &Prompt) -> Result<ResponseStream> {
        if self.state.endpoint.uses_responses_api() {
            let realtime_streaming_enabled = self.responses_websocket_enabled()
                && !self.streaming_preference.prefers_http_streaming();

            if realtime_streaming_enabled {
                return self.stream_responses_websocket(prompt).await;
            }

            return self.stream_responses_api(prompt).await;
        }

        if self.state.endpoint.uses_chat_completions_api() {
            let (api_stream, estimated_input_tokens) = self.stream_chat_completions(prompt).await?;

            return if self.state.runtime_config.show_raw_agent_reasoning {
                Ok(map_chat_response_stream(
                    api_stream.streaming_mode(),
                    self.state.otel_manager.clone(),
                    estimated_input_tokens,
                ))
            } else {
                Ok(map_chat_response_stream(
                    api_stream.aggregate(),
                    self.state.otel_manager.clone(),
                    estimated_input_tokens,
                ))
            };
        }

        self.stream_messages_api(prompt).await
    }

    pub(crate) fn try_switch_fallback_transport(&mut self) -> bool {
        let realtime_streaming_enabled = self.responses_websocket_enabled();
        let activated = self
            .streaming_preference
            .prefer_http_fallback(realtime_streaming_enabled);
        if activated {
            warn!("falling back to HTTP");
            self.state.otel_manager.counter(
                "codex.transport.fallback_to_http",
                1,
                &[("from_dialect", "responses_websocket")],
            );

            self.connection = None;
            self.websocket_last_items.clear();
        }
        activated
    }

    fn chat_role_compatibility_status(&self) -> ChatRoleCompatibility {
        self.state
            .chat_role_compatibility
            .as_ref()
            .map(|state| {
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .current()
            })
            .unwrap_or(ChatRoleCompatibility::Unknown)
    }

    fn developer_role_handling(&self) -> DeveloperRoleHandling {
        match self.chat_role_compatibility_status() {
            ChatRoleCompatibility::Unknown | ChatRoleCompatibility::SupportsDeveloper => {
                DeveloperRoleHandling::Preserve
            }
            ChatRoleCompatibility::RequiresSystemForDeveloper => {
                DeveloperRoleHandling::DowngradeToSystem
            }
        }
    }

    fn record_chat_role_supports_developer(&self) {
        if let Some(state) = &self.state.chat_role_compatibility {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_supports_developer();
        }
    }

    fn record_chat_role_requires_system(&self) {
        if let Some(state) = &self.state.chat_role_compatibility {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_requires_system();
        }
    }

    fn responses_websocket_enabled(&self) -> bool {
        self.state.endpoint.supports_realtime_turn_streaming()
    }

    fn build_responses_request(&self, prompt: &Prompt) -> Result<ApiPrompt> {
        let instructions = prompt.base_instructions.text.clone();
        let tools_json = create_tools_json_for_responses_api(&prompt.tools)?;
        Ok(build_api_prompt(prompt, instructions, tools_json))
    }

    fn build_messages_request(&self, prompt: &Prompt) -> Result<ApiPrompt> {
        let instructions = prompt.base_instructions.text.clone();
        let tools_json = create_tools_json_for_messages_api(&prompt.tools)?;
        Ok(build_api_prompt(prompt, instructions, tools_json))
    }

    fn build_responses_options(
        &self,
        prompt: &Prompt,
        compression: Compression,
    ) -> ApiResponsesOptions {
        let model_info = &self.state.model_info;
        let default_reasoning_effort = model_info.default_reasoning_level;
        let reasoning = if model_info.supports_reasoning_summaries {
            Some(Reasoning {
                effort: self.state.effort.or(default_reasoning_effort),
                summary: if self.state.summary == ReasoningSummary::None {
                    None
                } else {
                    Some(self.state.summary)
                },
            })
        } else {
            None
        };

        let include = if reasoning.is_some() {
            vec!["reasoning.encrypted_content".to_string()]
        } else {
            Vec::new()
        };

        let verbosity = if model_info.support_verbosity {
            self.state
                .runtime_config
                .model_verbosity
                .or(model_info.default_verbosity)
        } else {
            if self.state.runtime_config.model_verbosity.is_some() {
                warn!(
                    "model_verbosity is set but ignored as the model does not support verbosity: {}",
                    model_info.slug
                );
            }
            None
        };

        let text = create_text_param_for_request(verbosity, &prompt.output_schema);
        let session_id = self.state.session_id.clone();

        ApiResponsesOptions {
            reasoning,
            include,
            prompt_cache_key: Some(session_id.clone()),
            text,
            store_override: None,
            conversation_id: Some(session_id),
            origin_tag: self.state.origin_tag.clone(),
            extra_headers: build_responses_headers(
                &self.state.runtime_config,
                Some(&self.turn_state),
            ),
            compression,
            turn_state: Some(Arc::clone(&self.turn_state)),
        }
    }

    fn get_incremental_items(&self, input_items: &[TranscriptItem]) -> Option<Vec<TranscriptItem>> {
        let previous_len = self.websocket_last_items.len();
        let can_append = previous_len > 0
            && input_items.starts_with(&self.websocket_last_items)
            && previous_len < input_items.len();
        if can_append {
            Some(input_items[previous_len..].to_vec())
        } else {
            None
        }
    }

    fn prepare_websocket_request(
        &self,
        prompt_input: &[TranscriptItem],
        api_prompt: &ApiPrompt,
        options: &ApiResponsesOptions,
    ) -> ResponsesWsRequest {
        if let Some(append_items) = self.get_incremental_items(prompt_input) {
            return ResponsesWsRequest::ResponseAppend(ResponseAppendWsRequest {
                input: append_items.into_iter().collect(),
            });
        }

        let ApiResponsesOptions {
            reasoning,
            include,
            prompt_cache_key,
            text,
            store_override,
            ..
        } = options;

        let store = store_override.unwrap_or(false);
        let payload = ResponseCreateWsRequest {
            model: self.state.model_info.slug.clone(),
            instructions: api_prompt.instructions.clone(),
            input: api_prompt.input.clone(),
            tools: api_prompt.tools.clone(),
            tool_choice: "auto".to_string(),
            parallel_tool_calls: api_prompt.parallel_tool_calls,
            reasoning: reasoning.clone(),
            store,
            stream: true,
            include: include.clone(),
            prompt_cache_key: prompt_cache_key.clone(),
            text: text.clone(),
        };

        ResponsesWsRequest::ResponseCreate(payload)
    }

    async fn websocket_connection(
        &mut self,
        api_provider: adam_api::Provider,
        api_auth: crate::auth::LlmAuthProvider,
        options: &ApiResponsesOptions,
    ) -> std::result::Result<&ApiWebSocketConnection, ApiError> {
        let needs_new = match self.connection.as_ref() {
            Some(conn) => conn.is_closed().await,
            None => true,
        };

        if needs_new {
            let mut headers = options.extra_headers.clone();
            headers.extend(build_conversation_headers(options.conversation_id.clone()));
            let websocket_telemetry = self.build_websocket_telemetry();
            let new_conn = ApiWebSocketResponsesClient::new(api_provider, api_auth)
                .connect(
                    headers,
                    options.turn_state.clone(),
                    Some(websocket_telemetry),
                )
                .await?;
            self.connection = Some(new_conn);
        }

        self.connection.as_ref().ok_or(ApiError::Stream(
            "websocket connection is unavailable".to_string(),
        ))
    }

    fn responses_request_compression(&self, auth: Option<&AuthContext>) -> Compression {
        if auth.is_some_and(|auth| auth.use_chatgpt_base_url) && self.state.endpoint.is_openai() {
            Compression::Zstd
        } else {
            Compression::None
        }
    }

    async fn stream_chat_completions(&self, prompt: &Prompt) -> Result<(ApiResponseStream, i64)> {
        if prompt.output_schema.is_some() {
            return Err(Error::UnsupportedOperation(
                "output_schema is not supported for Chat Completions API".to_string(),
            ));
        }

        let instructions = prompt.base_instructions.text.clone();
        let tools_json = create_tools_json_for_chat_completions_api(&prompt.tools)?;
        let api_prompt = build_api_prompt(prompt, instructions, tools_json);
        let contains_developer_message = prompt_contains_developer_message(&prompt.input);
        let session_id = self.state.session_id.clone();
        let origin_tag = self.state.origin_tag.clone();

        let mut auth_recovery = self.state.auth_source.unauthorized_recovery();
        loop {
            let auth = self.state.auth_source.current_auth().await?;
            let api_provider = self
                .state
                .endpoint
                .to_api_provider(auth.as_ref().is_some_and(|auth| auth.use_chatgpt_base_url))?;
            let api_auth = auth_provider_from_context(auth.clone());
            let request_provider = api_provider.clone();
            let transport = ReqwestTransport::new(self.state.http_client.clone());
            let (request_telemetry, sse_telemetry) = self.build_streaming_telemetry();
            let client = ApiChatClient::new(transport, api_provider, api_auth)
                .with_telemetry(Some(request_telemetry), Some(sse_telemetry));

            let handling = self.developer_role_handling();
            let request = build_chat_request(
                &self.state.model_info.slug,
                &api_prompt,
                session_id.as_str(),
                origin_tag.as_deref(),
                handling,
                &request_provider,
            )?;
            let estimated_input_tokens =
                estimate_chat_input_tokens_from_request_body(&request.body);

            match client.stream_request(request).await {
                Ok(stream) => {
                    if handling == DeveloperRoleHandling::Preserve && contains_developer_message {
                        self.record_chat_role_supports_developer();
                    }
                    return Ok((stream, estimated_input_tokens));
                }
                Err(ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UNAUTHORIZED =>
                {
                    handle_unauthorized(status, &mut auth_recovery).await?;
                    continue;
                }
                Err(err)
                    if handling == DeveloperRoleHandling::Preserve
                        && contains_developer_message
                        && is_unsupported_developer_role_error(&err) =>
                {
                    info!(
                        provider = self.state.runtime_config.model_provider_id,
                        "chat provider rejected developer role; retrying with system"
                    );
                    self.record_chat_role_requires_system();
                    let retry_request = build_chat_request(
                        &self.state.model_info.slug,
                        &api_prompt,
                        session_id.as_str(),
                        origin_tag.as_deref(),
                        DeveloperRoleHandling::DowngradeToSystem,
                        &request_provider,
                    )?;
                    let retry_estimated_input_tokens =
                        estimate_chat_input_tokens_from_request_body(&retry_request.body);
                    match client.stream_request(retry_request).await {
                        Ok(stream) => return Ok((stream, retry_estimated_input_tokens)),
                        Err(ApiError::Transport(TransportError::Http { status, .. }))
                            if status == StatusCode::UNAUTHORIZED =>
                        {
                            handle_unauthorized(status, &mut auth_recovery).await?;
                            continue;
                        }
                        Err(err) => return Err(err.into()),
                    }
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    async fn stream_messages_api(&self, prompt: &Prompt) -> Result<ResponseStream> {
        if prompt.output_schema.is_some() {
            return Err(Error::UnsupportedOperation(
                "output_schema is not supported for Messages API".to_string(),
            ));
        }

        let api_prompt = self.build_messages_request(prompt)?;
        let mut auth_recovery = self.state.auth_source.unauthorized_recovery();
        loop {
            let auth = self.state.auth_source.current_auth().await?;
            let api_provider = self
                .state
                .endpoint
                .to_api_provider(auth.as_ref().is_some_and(|auth| auth.use_chatgpt_base_url))?;
            let api_auth = auth_provider_from_context(auth);
            let transport = ReqwestTransport::new(self.state.http_client.clone());
            let (request_telemetry, sse_telemetry) = self.build_streaming_telemetry();
            let request = ApiMessagesRequestBuilder::new(
                &self.state.model_info.slug,
                &api_prompt.instructions,
                &api_prompt.input,
                &api_prompt.tools,
            )
            .parallel_tool_calls(api_prompt.parallel_tool_calls)
            .max_tokens(ANTHROPIC_MESSAGES_MAX_TOKENS)
            .build(&api_provider)?;
            let client = ApiMessagesClient::new(transport, api_provider, api_auth)
                .with_telemetry(Some(request_telemetry), Some(sse_telemetry));

            match client.stream_request(request).await {
                Ok(stream) => {
                    return Ok(map_response_stream(stream, self.state.otel_manager.clone()));
                }
                Err(ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UNAUTHORIZED =>
                {
                    handle_unauthorized(status, &mut auth_recovery).await?;
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    async fn stream_responses_api(&self, prompt: &Prompt) -> Result<ResponseStream> {
        if let Some(path) = &self.state.runtime_config.sse_fixture_path {
            warn!(path, "Streaming from fixture");
            let stream =
                adam_api::stream_from_fixture(path, self.state.endpoint.stream_idle_timeout())?;
            return Ok(map_response_stream(stream, self.state.otel_manager.clone()));
        }

        let api_prompt = self.build_responses_request(prompt)?;
        let mut auth_recovery = self.state.auth_source.unauthorized_recovery();
        loop {
            let auth = self.state.auth_source.current_auth().await?;
            let api_provider = self
                .state
                .endpoint
                .to_api_provider(auth.as_ref().is_some_and(|auth| auth.use_chatgpt_base_url))?;
            let api_auth = auth_provider_from_context(auth.clone());
            let transport = ReqwestTransport::new(self.state.http_client.clone());
            let (request_telemetry, sse_telemetry) = self.build_streaming_telemetry();
            let compression = self.responses_request_compression(auth.as_ref());

            let client = ApiResponsesClient::new(transport, api_provider, api_auth)
                .with_telemetry(Some(request_telemetry), Some(sse_telemetry));

            let options = self.build_responses_options(prompt, compression);

            match client
                .stream_prompt(&self.state.model_info.slug, &api_prompt, options)
                .await
            {
                Ok(stream) => {
                    return Ok(map_response_stream(stream, self.state.otel_manager.clone()));
                }
                Err(ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UNAUTHORIZED =>
                {
                    handle_unauthorized(status, &mut auth_recovery).await?;
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    async fn stream_responses_websocket(&mut self, prompt: &Prompt) -> Result<ResponseStream> {
        let api_prompt = self.build_responses_request(prompt)?;
        let mut auth_recovery = self.state.auth_source.unauthorized_recovery();
        loop {
            let auth = self.state.auth_source.current_auth().await?;
            let api_provider = self
                .state
                .endpoint
                .to_api_provider(auth.as_ref().is_some_and(|auth| auth.use_chatgpt_base_url))?;
            let api_auth = auth_provider_from_context(auth.clone());
            let compression = self.responses_request_compression(auth.as_ref());

            let options = self.build_responses_options(prompt, compression);
            let request = self.prepare_websocket_request(&prompt.input, &api_prompt, &options);

            let connection = match self
                .websocket_connection(api_provider.clone(), api_auth.clone(), &options)
                .await
            {
                Ok(connection) => connection,
                Err(ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UNAUTHORIZED =>
                {
                    handle_unauthorized(status, &mut auth_recovery).await?;
                    continue;
                }
                Err(err) => return Err(err.into()),
            };

            let stream_result = connection.stream_request(request).await?;
            self.websocket_last_items = prompt.input.clone();
            return Ok(map_response_stream(
                stream_result,
                self.state.otel_manager.clone(),
            ));
        }
    }

    fn build_streaming_telemetry(&self) -> (Arc<dyn RequestTelemetry>, Arc<dyn SseTelemetry>) {
        let telemetry = Arc::new(ApiTelemetry::new(self.state.otel_manager.clone()));
        let request_telemetry: Arc<dyn RequestTelemetry> = telemetry.clone();
        let sse_telemetry: Arc<dyn SseTelemetry> = telemetry;
        (request_telemetry, sse_telemetry)
    }

    fn build_websocket_telemetry(&self) -> Arc<dyn WebsocketTelemetry> {
        (Arc::new(ApiTelemetry::new(self.state.otel_manager.clone()))) as _
    }
}

fn build_api_prompt(prompt: &Prompt, instructions: String, tools_json: Vec<Value>) -> ApiPrompt {
    ApiPrompt {
        instructions,
        input: prompt.input.clone(),
        tools: tools_json,
        parallel_tool_calls: prompt.parallel_tool_calls,
        output_schema: prompt.output_schema.clone(),
    }
}

fn experimental_feature_headers(config: &LlmRuntimeConfig) -> ApiHeaderMap {
    let mut headers = ApiHeaderMap::new();
    let value = config.experimental_beta_feature_keys.join(",");
    if !value.is_empty()
        && let Ok(header_value) = HeaderValue::from_str(value.as_str())
    {
        headers.insert("x-codex-beta-features", header_value);
    }
    headers
}

fn build_responses_headers(
    config: &LlmRuntimeConfig,
    turn_state: Option<&Arc<OnceLock<String>>>,
) -> ApiHeaderMap {
    let mut headers = experimental_feature_headers(config);
    headers.insert(
        WEB_SEARCH_ELIGIBLE_HEADER,
        HeaderValue::from_static(
            if matches!(config.web_search_mode, Some(WebSearchMode::Disabled)) {
                "false"
            } else {
                "true"
            },
        ),
    );
    if let Some(turn_state) = turn_state
        && let Some(state) = turn_state.get()
        && let Ok(header_value) = HeaderValue::from_str(state)
    {
        headers.insert(X_CODEX_TURN_STATE_HEADER, header_value);
    }
    headers
}

fn build_chat_request(
    model: &str,
    prompt: &ApiPrompt,
    conversation_id: &str,
    origin_tag: Option<&str>,
    developer_role_handling: DeveloperRoleHandling,
    provider: &adam_api::Provider,
) -> std::result::Result<adam_api::ChatRequest, Error> {
    ApiChatRequestBuilder::new(model, &prompt.instructions, &prompt.input, &prompt.tools)
        .conversation_id(Some(conversation_id.to_string()))
        .origin_tag(origin_tag.map(ToOwned::to_owned))
        .developer_role_handling(developer_role_handling)
        .build(provider)
        .map_err(Into::into)
}

fn prompt_contains_developer_message(input: &[TranscriptItem]) -> bool {
    input.iter().any(|item| {
        matches!(
            item,
            TranscriptItem::Message { role, .. } if role == "developer"
        )
    })
}

fn is_unsupported_developer_role_error(err: &ApiError) -> bool {
    let ApiError::Transport(TransportError::Http {
        status,
        body: Some(body),
        ..
    }) = err
    else {
        return false;
    };

    if *status != StatusCode::BAD_REQUEST {
        return false;
    }

    let message = try_parse_error_message(body).to_ascii_lowercase();
    message.contains("developer is not one of")
        && message.contains("messages")
        && message.contains("role")
}

fn try_parse_error_message(text: &str) -> String {
    let json = serde_json::from_str::<serde_json::Value>(text).unwrap_or_default();
    if let Some(error) = json.get("error")
        && let Some(message) = error.get("message")
        && let Some(message_str) = message.as_str()
    {
        return message_str.to_string();
    }
    if text.is_empty() {
        return "Unknown error".to_string();
    }
    text.to_string()
}

fn map_response_stream<S>(api_stream: S, otel_manager: OtelManager) -> ResponseStream
where
    S: futures::Stream<Item = std::result::Result<ResponseEvent, ApiError>>
        + Unpin
        + Send
        + 'static,
{
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent>>(1600);

    tokio::spawn(async move {
        let mut logged_error = false;
        let mut api_stream = api_stream;
        while let Some(event) = api_stream.next().await {
            match event {
                Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                }) => {
                    if let Some(usage) = &token_usage {
                        otel_manager.sse_event_completed(
                            usage.input_tokens,
                            usage.output_tokens,
                            Some(usage.cached_input_tokens),
                            Some(usage.reasoning_output_tokens),
                            usage.total_tokens,
                        );
                    }
                    if tx_event
                        .send(Ok(ResponseEvent::Completed {
                            response_id,
                            token_usage,
                        }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(event) => {
                    if tx_event.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let mapped = Error::from(err);
                    if !logged_error {
                        otel_manager.see_event_completed_failed(&mapped);
                        logged_error = true;
                    }
                    if tx_event.send(Err(mapped)).await.is_err() {
                        return;
                    }
                }
            }
        }
    });

    ResponseStream { rx_event }
}

fn map_chat_response_stream<S>(
    api_stream: S,
    otel_manager: OtelManager,
    estimated_input_tokens: i64,
) -> ResponseStream
where
    S: futures::Stream<Item = std::result::Result<ResponseEvent, ApiError>>
        + Unpin
        + Send
        + 'static,
{
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent>>(1600);

    tokio::spawn(async move {
        let mut logged_error = false;
        let mut api_stream = api_stream;
        let mut estimated_output_tokens = 0i64;
        while let Some(event) = api_stream.next().await {
            match event {
                Ok(ResponseEvent::OutputItemDone(item)) => {
                    estimated_output_tokens = estimated_output_tokens
                        .saturating_add(estimate_chat_output_tokens_for_item(&item.clone()));
                    if tx_event
                        .send(Ok(ResponseEvent::OutputItemDone(item)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(ResponseEvent::Completed {
                    response_id,
                    token_usage,
                }) => {
                    let token_usage = token_usage.or_else(|| {
                        let total_tokens =
                            estimated_input_tokens.saturating_add(estimated_output_tokens);
                        Some(TokenUsage {
                            input_tokens: estimated_input_tokens,
                            cached_input_tokens: 0,
                            output_tokens: estimated_output_tokens,
                            reasoning_output_tokens: 0,
                            total_tokens,
                        })
                    });
                    if let Some(usage) = &token_usage {
                        otel_manager.sse_event_completed(
                            usage.input_tokens,
                            usage.output_tokens,
                            Some(usage.cached_input_tokens),
                            Some(usage.reasoning_output_tokens),
                            usage.total_tokens,
                        );
                    }
                    if tx_event
                        .send(Ok(ResponseEvent::Completed {
                            response_id,
                            token_usage,
                        }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(event) => {
                    if tx_event.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let mapped = Error::from(err);
                    if !logged_error {
                        otel_manager.see_event_completed_failed(&mapped);
                        logged_error = true;
                    }
                    if tx_event.send(Err(mapped)).await.is_err() {
                        return;
                    }
                }
            }
        }
    });

    ResponseStream { rx_event }
}

fn estimate_chat_input_tokens_from_request_body(body: &Value) -> i64 {
    let message_chars = body
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .map(estimate_chat_message_chars)
                .fold(0usize, usize::saturating_add)
        })
        .unwrap_or(0);
    let tool_chars = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(estimate_chat_tool_schema_chars)
                .fold(0usize, usize::saturating_add)
        })
        .unwrap_or(0);

    approx_chars_to_tokens(message_chars.saturating_add(tool_chars))
}

fn estimate_chat_message_chars(message: &Value) -> usize {
    let content_chars = message
        .get("content")
        .map(estimate_chat_message_content_chars)
        .unwrap_or(0);
    let reasoning_chars = message
        .get("reasoning")
        .and_then(Value::as_str)
        .map(count_chars)
        .unwrap_or(0);
    let tool_call_chars = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|tool_calls| {
            tool_calls
                .iter()
                .map(estimate_chat_tool_call_chars)
                .fold(0usize, usize::saturating_add)
        })
        .unwrap_or(0);

    content_chars
        .saturating_add(reasoning_chars)
        .saturating_add(tool_call_chars)
}

fn estimate_chat_message_content_chars(content: &Value) -> usize {
    match content {
        Value::String(text) => count_chars(text),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                let text_chars = item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(count_chars)
                    .unwrap_or(0);
                let image_chars = item
                    .get("image_url")
                    .and_then(Value::as_object)
                    .and_then(|image| image.get("url"))
                    .and_then(Value::as_str)
                    .map(count_chars)
                    .unwrap_or(0);
                text_chars.saturating_add(image_chars)
            })
            .fold(0usize, usize::saturating_add),
        _ => 0,
    }
}

fn estimate_chat_tool_call_chars(tool_call: &Value) -> usize {
    let Some(object) = tool_call.as_object() else {
        return 0;
    };

    if let Some(function) = object.get("function") {
        return estimate_chat_function_call_chars(function);
    }
    if let Some(custom) = object.get("custom") {
        return estimate_chat_semantic_value_chars(custom);
    }
    if let Some(action) = object.get("action") {
        return object
            .get("status")
            .map(estimate_chat_semantic_value_chars)
            .unwrap_or(0)
            .saturating_add(estimate_chat_semantic_value_chars(action));
    }

    object
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "id" | "type"))
        .map(|(_, value)| estimate_chat_semantic_value_chars(value))
        .fold(0usize, usize::saturating_add)
}

fn estimate_chat_function_call_chars(function: &Value) -> usize {
    let Some(object) = function.as_object() else {
        return estimate_chat_semantic_value_chars(function);
    };

    object
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "name" | "arguments"))
        .map(|(_, value)| estimate_chat_semantic_value_chars(value))
        .fold(0usize, usize::saturating_add)
}

fn estimate_chat_tool_schema_chars(tool: &Value) -> usize {
    count_chars(&serde_json::to_string(tool).unwrap_or_default())
}

fn estimate_chat_semantic_value_chars(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Bool(boolean) => boolean.to_string().len(),
        Value::Number(number) => number.to_string().len(),
        Value::String(text) => count_chars(text),
        Value::Array(items) => items
            .iter()
            .map(estimate_chat_semantic_value_chars)
            .fold(0usize, usize::saturating_add),
        Value::Object(object) => object
            .values()
            .map(estimate_chat_semantic_value_chars)
            .fold(0usize, usize::saturating_add),
    }
}

fn estimate_chat_output_tokens_for_item(item: &TranscriptItem) -> i64 {
    let chars = match item {
        TranscriptItem::Message { role, content, .. } if role == "assistant" => content
            .iter()
            .map(|content_item| match content_item {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    count_chars(text)
                }
                ContentItem::InputImage { image_url } => count_chars(image_url),
            })
            .fold(0usize, usize::saturating_add),
        TranscriptItem::ToolCall {
            tool_name, payload, ..
        } => {
            let payload_chars = match payload {
                crate::ToolCallPayload::JsonArguments { arguments } => count_chars(arguments),
                crate::ToolCallPayload::TextInput { input } => count_chars(input),
            };
            count_chars(tool_name).saturating_add(payload_chars)
        }
        _ => 0,
    };

    approx_chars_to_tokens(chars)
}

fn approx_chars_to_tokens(chars: usize) -> i64 {
    let tokens =
        chars.saturating_add(APPROX_CHARS_PER_TOKEN.saturating_sub(1)) / APPROX_CHARS_PER_TOKEN;
    i64::try_from(tokens).unwrap_or(i64::MAX)
}

fn count_chars(text: &str) -> usize {
    text.chars().count()
}

async fn handle_unauthorized(
    status: StatusCode,
    auth_recovery: &mut Option<Box<dyn UnauthorizedRecovery>>,
) -> Result<()> {
    if let Some(recovery) = auth_recovery
        && recovery.has_next()
    {
        return recovery.recover().await;
    }

    Err(ApiError::Transport(TransportError::Http {
        status,
        url: None,
        headers: None,
        body: None,
    })
    .into())
}

struct ApiTelemetry {
    otel_manager: OtelManager,
}

impl ApiTelemetry {
    fn new(otel_manager: OtelManager) -> Self {
        Self { otel_manager }
    }
}

impl RequestTelemetry for ApiTelemetry {
    fn on_request(
        &self,
        attempt: u64,
        status: Option<HttpStatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    ) {
        let error_message = error.map(std::string::ToString::to_string);
        self.otel_manager.record_api_request(
            attempt,
            status.map(|s| s.as_u16()),
            error_message.as_deref(),
            duration,
        );
    }
}

impl SseTelemetry for ApiTelemetry {
    fn on_sse_poll(
        &self,
        result: &std::result::Result<
            Option<std::result::Result<Event, EventStreamError<TransportError>>>,
            tokio::time::error::Elapsed,
        >,
        duration: Duration,
    ) {
        self.otel_manager.log_sse_event(result, duration);
    }
}

impl WebsocketTelemetry for ApiTelemetry {
    fn on_ws_request(&self, duration: Duration, error: Option<&ApiError>) {
        let error_message = error.map(std::string::ToString::to_string);
        self.otel_manager
            .record_websocket_request(duration, error_message.as_deref());
    }

    fn on_ws_event(
        &self,
        result: &std::result::Result<
            Option<std::result::Result<Message, WebsocketError>>,
            ApiError,
        >,
        duration: Duration,
    ) {
        self.otel_manager.record_websocket_event(result, duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adam_llm_types::ReasoningItemContent;
    use serde_json::json;

    #[test]
    fn estimate_chat_input_tokens_counts_semantic_content() {
        let tool = json!({
            "type": "function",
            "name": "read_file",
            "description": "Read a file from disk",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path"
                    }
                }
            }
        });
        let body = json!({
            "model": "gpt-test",
            "messages": [
                {"role": "system", "content": "inst"},
                {"role": "user", "content": "hello"},
                {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning": "why",
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"src/lib.rs\"}"
                            }
                        }
                    ]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "content": [
                        {"type": "text", "text": "file body"},
                        {"type": "image_url", "image_url": {"url": "https://x/y.png"}}
                    ]
                }
            ],
            "stream": true,
            "tools": [tool]
        });

        let chars = count_chars("inst")
            + count_chars("hello")
            + count_chars("answer")
            + count_chars("why")
            + count_chars("read_file")
            + count_chars("{\"path\":\"src/lib.rs\"}")
            + count_chars("file body")
            + count_chars("https://x/y.png")
            + count_chars(&serde_json::to_string(&tool).unwrap());

        assert_eq!(
            estimate_chat_input_tokens_from_request_body(&body),
            approx_chars_to_tokens(chars)
        );
    }

    #[test]
    fn estimate_chat_output_tokens_counts_messages_and_tool_calls() {
        let assistant = TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "hello".to_string(),
            }],
            end_turn: None,
        };
        let tool_call = TranscriptItem::ToolCall {
            id: None,
            call_id: "call_1".to_string(),
            tool_name: "read_file".to_string(),
            payload: crate::ToolCallPayload::JsonArguments {
                arguments: "{\"path\":\"src/lib.rs\"}".to_string(),
            },
        };
        let reasoning = TranscriptItem::Reasoning {
            id: String::new(),
            summary: Vec::new(),
            content: Some(vec![ReasoningItemContent::ReasoningText {
                text: "internal".to_string(),
            }]),
            encrypted_content: None,
        };

        assert_eq!(
            estimate_chat_output_tokens_for_item(&assistant),
            approx_chars_to_tokens(count_chars("hello"))
        );
        assert_eq!(
            estimate_chat_output_tokens_for_item(&tool_call),
            approx_chars_to_tokens(
                count_chars("read_file") + count_chars("{\"path\":\"src/lib.rs\"}")
            )
        );
        assert_eq!(estimate_chat_output_tokens_for_item(&reasoning), 0);
    }
}
