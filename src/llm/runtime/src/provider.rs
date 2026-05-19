use crate::Error;
use crate::Result;
use adam_api::Provider as ApiProvider;
use adam_api::WireApi as ApiWireApi;
use adam_api::is_azure_responses_wire_base_url;
use adam_api::provider::RetryConfig as ApiRetryConfig;
use http::HeaderMap;
use http::header::HeaderName;
use http::header::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::env::VarError;
use std::time::Duration;

const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_STREAM_MAX_RETRIES: u64 = 5;
const DEFAULT_REQUEST_MAX_RETRIES: u64 = 4;
const MAX_STREAM_MAX_RETRIES: u64 = 100;
const MAX_REQUEST_MAX_RETRIES: u64 = 100;
const OPENAI_PROVIDER_NAME: &str = "OpenAI";

/// Internal protocol profile spoken by the target runtime endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ConversationDialect {
    /// The Responses API exposed by OpenAI at `/v1/responses`.
    Responses,

    /// Regular Chat Completions compatible with `/v1/chat/completions`.
    #[default]
    Chat,

    /// Anthropic-compatible Messages API exposed at `/v1/messages`.
    Messages,
}

/// Serializable description of a runtime endpoint.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RuntimeEndpoint {
    /// Friendly display name.
    pub name: String,
    /// Base URL for the provider's OpenAI-compatible API.
    pub base_url: Option<String>,
    /// Environment variable that stores the user's API key for this provider.
    pub env_key: Option<String>,
    /// Optional instructions to help the user get a valid value for the
    /// variable and set it.
    pub env_key_instructions: Option<String>,
    /// Value to use with `Authorization: Bearer <token>` header. Use of this
    /// config is discouraged in favor of `env_key` for security reasons, but
    /// this may be necessary when using this programmatically.
    pub bearer_token: Option<String>,
    /// Which conversation dialect this endpoint expects.
    #[serde(default)]
    pub(crate) dialect: ConversationDialect,
    /// Optional query parameters to append to the base URL.
    pub query_params: Option<HashMap<String, String>>,
    /// Additional HTTP headers to include in requests to this provider where
    /// the (key, value) pairs are the header name and value.
    pub http_headers: Option<HashMap<String, String>>,
    /// Optional HTTP headers to include in requests to this provider where the
    /// (key, value) pairs are the header name and _environment variable_ whose
    /// value should be used. If the environment variable is not set, or the
    /// value is empty, the header will not be included in the request.
    pub env_http_headers: Option<HashMap<String, String>>,
    /// Maximum number of times to retry a failed HTTP request to this provider.
    pub request_max_retries: Option<u64>,
    /// Number of times to retry reconnecting a dropped streaming response
    /// before failing.
    pub stream_max_retries: Option<u64>,
    /// Idle timeout (in milliseconds) to wait for activity on a streaming
    /// response before treating the connection as lost.
    pub stream_idle_timeout_ms: Option<u64>,
    /// Whether this endpoint supports realtime streaming.
    #[serde(default)]
    supports_realtime_streaming: bool,
}

impl RuntimeEndpoint {
    pub fn openai() -> RuntimeEndpoint {
        Self::create_openai_endpoint()
    }

    pub fn openai_compatible_chat(
        name: impl Into<String>,
        base_url: impl Into<String>,
    ) -> RuntimeEndpoint {
        Self::new_custom(
            name.into(),
            Some(base_url.into()),
            ConversationDialect::Chat,
        )
    }

    pub fn openai_compatible_responses(
        name: impl Into<String>,
        base_url: impl Into<String>,
    ) -> RuntimeEndpoint {
        Self::new_custom(
            name.into(),
            Some(base_url.into()),
            ConversationDialect::Responses,
        )
    }

    pub fn anthropic_compatible_messages(
        name: impl Into<String>,
        base_url: impl Into<String>,
    ) -> RuntimeEndpoint {
        Self::new_custom(
            name.into(),
            Some(base_url.into()),
            ConversationDialect::Messages,
        )
    }

    pub fn uses_responses_api(&self) -> bool {
        matches!(self.dialect, ConversationDialect::Responses)
    }

    pub fn uses_chat_completions_api(&self) -> bool {
        matches!(self.dialect, ConversationDialect::Chat)
    }

    pub fn uses_messages_api(&self) -> bool {
        matches!(self.dialect, ConversationDialect::Messages)
    }

    pub fn supports_model_catalog(&self) -> bool {
        !self.uses_messages_api()
    }

    pub fn requires_explicit_model_selection(&self) -> bool {
        self.uses_messages_api()
    }

    pub fn supports_dynamic_context_window_probe(&self) -> bool {
        self.uses_chat_completions_api() || self.uses_messages_api()
    }

    pub fn supports_output_schema(&self) -> bool {
        self.uses_responses_api()
    }

    pub fn supports_realtime_turn_streaming(&self) -> bool {
        self.supports_realtime_streaming && self.uses_responses_api()
    }

    pub fn realtime_turn_streaming_enabled(&self) -> bool {
        self.supports_realtime_streaming
    }

    pub fn set_chat_turns(&mut self) {
        self.dialect = ConversationDialect::Chat;
    }

    pub fn set_response_turns(&mut self) {
        self.dialect = ConversationDialect::Responses;
    }

    pub fn set_message_turns(&mut self) {
        self.dialect = ConversationDialect::Messages;
    }

    pub fn with_chat_turns(mut self) -> Self {
        self.set_chat_turns();
        self
    }

    pub fn with_response_turns(mut self) -> Self {
        self.set_response_turns();
        self
    }

    pub fn with_message_turns(mut self) -> Self {
        self.set_message_turns();
        self
    }

    pub fn with_request_max_retries(mut self, request_max_retries: Option<u64>) -> Self {
        self.request_max_retries = request_max_retries;
        self
    }

    pub fn with_stream_max_retries(mut self, stream_max_retries: Option<u64>) -> Self {
        self.stream_max_retries = stream_max_retries;
        self
    }

    pub fn with_stream_idle_timeout_ms(mut self, stream_idle_timeout_ms: Option<u64>) -> Self {
        self.stream_idle_timeout_ms = stream_idle_timeout_ms;
        self
    }

    pub fn with_realtime_turn_streaming_enabled(
        mut self,
        supports_realtime_streaming: bool,
    ) -> Self {
        self.supports_realtime_streaming = supports_realtime_streaming;
        self
    }

    pub fn set_realtime_turn_streaming_enabled(&mut self, supports_realtime_streaming: bool) {
        self.supports_realtime_streaming = supports_realtime_streaming;
    }

    pub fn with_env_key(mut self, env_key: Option<String>) -> Self {
        self.env_key = env_key;
        self
    }

    pub fn with_env_key_instructions(mut self, env_key_instructions: Option<String>) -> Self {
        self.env_key_instructions = env_key_instructions;
        self
    }

    pub fn with_bearer_token(mut self, bearer_token: Option<String>) -> Self {
        self.bearer_token = bearer_token;
        self
    }

    pub fn with_query_params(mut self, query_params: Option<HashMap<String, String>>) -> Self {
        self.query_params = query_params;
        self
    }

    pub fn with_http_headers(mut self, http_headers: Option<HashMap<String, String>>) -> Self {
        self.http_headers = http_headers;
        self
    }

    pub fn with_env_http_headers(
        mut self,
        env_http_headers: Option<HashMap<String, String>>,
    ) -> Self {
        self.env_http_headers = env_http_headers;
        self
    }

    pub fn supports_live_web_search(&self) -> bool {
        !self.is_azure_responses_endpoint()
    }

    fn build_header_map(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(extra) = &self.http_headers {
            for (k, v) in extra {
                if let (Ok(name), Ok(value)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
                    headers.insert(name, value);
                }
            }
        }

        if let Some(env_headers) = &self.env_http_headers {
            for (header, env_var) in env_headers {
                if let Ok(val) = std::env::var(env_var)
                    && !val.trim().is_empty()
                    && let (Ok(name), Ok(value)) =
                        (HeaderName::try_from(header), HeaderValue::try_from(val))
                {
                    headers.insert(name, value);
                }
            }
        }

        headers
    }

    pub(crate) fn to_api_provider(&self) -> Result<ApiProvider> {
        let default_base_url = "https://api.openai.com/v1";
        let base_url = self
            .base_url
            .clone()
            .unwrap_or_else(|| default_base_url.to_string());

        let retry = ApiRetryConfig {
            max_attempts: self.request_max_retries(),
            base_delay: Duration::from_millis(200),
            retry_429: false,
            retry_5xx: true,
            retry_transport: true,
        };

        Ok(ApiProvider {
            name: self.name.clone(),
            base_url,
            query_params: self.query_params.clone(),
            wire: self.api_wire(),
            headers: self.build_header_map(),
            retry,
            stream_idle_timeout: self.stream_idle_timeout(),
        })
    }

    pub(crate) fn is_azure_responses_endpoint(&self) -> bool {
        is_azure_responses_wire_base_url(self.api_wire(), &self.name, self.base_url.as_deref())
    }

    pub fn api_key(&self) -> Result<Option<String>> {
        match &self.env_key {
            Some(env_key) => {
                let env_value = std::env::var(env_key);
                env_value
                    .and_then(|v| {
                        if v.trim().is_empty() {
                            Err(VarError::NotPresent)
                        } else {
                            Ok(Some(v))
                        }
                    })
                    .map_err(|_| Error::EnvVar {
                        var: env_key.clone(),
                        instructions: self.env_key_instructions.clone(),
                    })
            }
            None => Ok(None),
        }
    }

    pub fn has_local_auth(&self) -> bool {
        self.bearer_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
            || self
                .env_key
                .as_deref()
                .and_then(|env_key| std::env::var(env_key).ok())
                .is_some_and(|value| !value.trim().is_empty())
    }

    pub fn request_max_retries(&self) -> u64 {
        self.request_max_retries
            .unwrap_or(DEFAULT_REQUEST_MAX_RETRIES)
            .min(MAX_REQUEST_MAX_RETRIES)
    }

    pub fn stream_max_retries(&self) -> u64 {
        self.stream_max_retries
            .unwrap_or(DEFAULT_STREAM_MAX_RETRIES)
            .min(MAX_STREAM_MAX_RETRIES)
    }

    pub fn stream_idle_timeout(&self) -> Duration {
        self.stream_idle_timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(DEFAULT_STREAM_IDLE_TIMEOUT_MS))
    }

    fn create_openai_endpoint() -> RuntimeEndpoint {
        RuntimeEndpoint {
            name: OPENAI_PROVIDER_NAME.into(),
            base_url: std::env::var("OPENAI_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            env_key: Some("OPENAI_API_KEY".to_string()),
            env_key_instructions: None,
            bearer_token: None,
            dialect: ConversationDialect::Responses,
            query_params: None,
            http_headers: Some(
                [("version".to_string(), env!("CARGO_PKG_VERSION").to_string())]
                    .into_iter()
                    .collect(),
            ),
            env_http_headers: Some(
                [
                    (
                        "OpenAI-Organization".to_string(),
                        "OPENAI_ORGANIZATION".to_string(),
                    ),
                    ("OpenAI-Project".to_string(), "OPENAI_PROJECT".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            supports_realtime_streaming: true,
        }
    }

    pub fn is_openai(&self) -> bool {
        self.name == OPENAI_PROVIDER_NAME
    }

    pub fn supports_remote_compaction(&self) -> bool {
        self.is_openai() && self.uses_responses_api()
    }

    pub fn enforce_declared_tool_names(&self) -> bool {
        self.uses_messages_api()
    }

    fn new_custom(
        name: String,
        base_url: Option<String>,
        dialect: ConversationDialect,
    ) -> RuntimeEndpoint {
        RuntimeEndpoint {
            name,
            base_url,
            env_key: None,
            env_key_instructions: None,
            bearer_token: None,
            dialect,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            supports_realtime_streaming: false,
        }
    }

    fn api_wire(&self) -> ApiWireApi {
        match self.dialect {
            ConversationDialect::Responses => ApiWireApi::Responses,
            ConversationDialect::Chat => ApiWireApi::Chat,
            ConversationDialect::Messages => ApiWireApi::Messages,
        }
    }
}

pub fn built_in_runtime_endpoints() -> HashMap<String, RuntimeEndpoint> {
    use RuntimeEndpoint as P;

    [("openai", P::openai())]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}
