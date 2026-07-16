use crate::Error;
use crate::Result;
use crate::api::Provider as ApiProvider;
use crate::api::WireApi as ApiWireApi;
use crate::api::is_azure_responses_wire_base_url;
use crate::api::provider::RetryConfig as ApiRetryConfig;
use crate::env::LHA_API_KEY_ENV_VAR;
use crate::env::LHA_BASE_URL_ENV_VAR;
use crate::env::LHA_ENDPOINT_ENV_VAR;
use crate::env::read_required_env_with_lookup;
use http::HeaderMap;
use http::header::HeaderName;
use http::header::HeaderValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::env::VarError;
use std::str::FromStr;
use std::time::Duration;
use url::Url;

const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_STREAM_MAX_RETRIES: u64 = 5;
const DEFAULT_REQUEST_MAX_RETRIES: u64 = 4;
const MAX_STREAM_MAX_RETRIES: u64 = 100;
const MAX_REQUEST_MAX_RETRIES: u64 = 100;
const OPENAI_PROVIDER_NAME: &str = "OpenAI";
type NormalizedCompatibleBaseUrl = (
    String,
    Option<RuntimeEndpointKind>,
    Option<HashMap<String, String>>,
);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEndpointKind {
    Chat,
    Responses,
    Messages,
}

impl RuntimeEndpointKind {
    fn dialect(self) -> ConversationDialect {
        match self {
            Self::Chat => ConversationDialect::Chat,
            Self::Responses => ConversationDialect::Responses,
            Self::Messages => ConversationDialect::Messages,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

impl FromStr for RuntimeEndpointKind {
    type Err = Error;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "chat" => Ok(Self::Chat),
            "responses" => Ok(Self::Responses),
            "messages" => Ok(Self::Messages),
            _ => Err(Error::InvalidRequest {
                message: format!(
                    "invalid {LHA_ENDPOINT_ENV_VAR}: expected one of chat, responses, messages"
                ),
            }),
        }
    }
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
    /// Number of times to retry a recoverable response turn, including a
    /// dropped streaming response or a non-streaming retryable failure.
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

    pub fn from_lha_env(name: impl Into<String>) -> Result<Self> {
        Self::from_lha_env_with_lookup(name, |var| std::env::var(var).ok())
    }

    pub(crate) fn from_lha_env_with_lookup(
        name: impl Into<String>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let base_url = read_required_env_with_lookup(LHA_BASE_URL_ENV_VAR, &lookup)?;
        read_required_env_with_lookup(LHA_API_KEY_ENV_VAR, &lookup)?;
        let endpoint_kind = lookup(LHA_ENDPOINT_ENV_VAR)
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.parse())
            .transpose()?;

        Self::infer_compatible(name, base_url, LHA_API_KEY_ENV_VAR, endpoint_kind)
    }

    pub fn infer_compatible(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key_env: impl Into<String>,
        endpoint_kind: Option<RuntimeEndpointKind>,
    ) -> Result<Self> {
        let name = name.into();
        let api_key_env = api_key_env.into();
        if api_key_env.trim().is_empty() {
            return Err(Error::InvalidRequest {
                message: "api key environment variable name must not be empty".to_string(),
            });
        }

        let (base_url, inferred_kind, query_params) = normalize_compatible_base_url(base_url)?;
        if let (Some(inferred_kind), Some(endpoint_kind)) = (inferred_kind, endpoint_kind)
            && inferred_kind != endpoint_kind
        {
            return Err(Error::InvalidRequest {
                message: format!(
                    "base URL endpoint suffix implies {} but {} requested {}",
                    inferred_kind.label(),
                    LHA_ENDPOINT_ENV_VAR,
                    endpoint_kind.label()
                ),
            });
        }

        let endpoint_kind = endpoint_kind
            .or(inferred_kind)
            .unwrap_or(RuntimeEndpointKind::Chat);
        let mut endpoint = Self::new_custom(name, Some(base_url), endpoint_kind.dialect())
            .with_env_key(Some(api_key_env));
        endpoint.query_params = query_params;
        Ok(endpoint)
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
        if Url::parse(&base_url).is_ok_and(|url| url.query().is_some()) {
            return Err(Error::InvalidRequest {
                message: "base URL must not include a query string".to_string(),
            });
        }

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

fn normalize_compatible_base_url(
    base_url: impl Into<String>,
) -> Result<NormalizedCompatibleBaseUrl> {
    let base_url = base_url.into();
    let mut url = Url::parse(&base_url).map_err(|err| Error::InvalidRequest {
        message: format!("invalid base URL: {err}"),
    })?;
    if url.fragment().is_some() {
        return Err(Error::InvalidRequest {
            message: "base URL must not include a fragment".to_string(),
        });
    }
    if url.query().is_some() {
        return Err(Error::InvalidRequest {
            message: "base URL must not include a query string".to_string(),
        });
    }

    let mut segments = url
        .path_segments()
        .ok_or_else(|| Error::InvalidRequest {
            message: "base URL must use a hierarchical URL scheme".to_string(),
        })?
        .map(str::to_string)
        .collect::<Vec<_>>();
    while segments.last().is_some_and(std::string::String::is_empty) {
        segments.pop();
    }

    let inferred_kind = inferred_kind_from_segments(&segments);
    if let Some(kind) = inferred_kind {
        let strip_count = match kind {
            RuntimeEndpointKind::Chat => 2,
            RuntimeEndpointKind::Responses | RuntimeEndpointKind::Messages => 1,
        };
        for _ in 0..strip_count {
            segments.pop();
        }
        set_url_path_segments(&mut url, &segments)?;
    }

    Ok((base_url_string(url), inferred_kind, None))
}

fn inferred_kind_from_segments(segments: &[String]) -> Option<RuntimeEndpointKind> {
    if segments.len() >= 2
        && segments[segments.len() - 2] == "chat"
        && segments[segments.len() - 1] == "completions"
    {
        Some(RuntimeEndpointKind::Chat)
    } else if segments
        .last()
        .is_some_and(|segment| segment == "responses")
    {
        Some(RuntimeEndpointKind::Responses)
    } else if segments.last().is_some_and(|segment| segment == "messages") {
        Some(RuntimeEndpointKind::Messages)
    } else {
        None
    }
}

fn set_url_path_segments(url: &mut Url, segments: &[String]) -> Result<()> {
    let mut path_segments = url.path_segments_mut().map_err(|_| Error::InvalidRequest {
        message: "base URL must use a hierarchical URL scheme".to_string(),
    })?;
    path_segments.clear();
    path_segments.extend(segments.iter().map(String::as_str));
    Ok(())
}

fn base_url_string(url: Url) -> String {
    let root_path = url.path() == "/";
    let mut value = url.to_string();
    if root_path && value.ends_with('/') {
        value.pop();
    }
    value
}

pub fn built_in_runtime_endpoints() -> HashMap<String, RuntimeEndpoint> {
    use RuntimeEndpoint as P;

    [("openai", P::openai())]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars = vars
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<HashMap<_, _>>();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn from_lha_env_requires_base_url() {
        let err = RuntimeEndpoint::from_lha_env_with_lookup(
            "sdk",
            lookup(&[(LHA_API_KEY_ENV_VAR, "secret")]),
        )
        .expect_err("missing base url should fail");

        assert!(matches!(err, Error::EnvVar { var, .. } if var == LHA_BASE_URL_ENV_VAR));
    }

    #[test]
    fn from_lha_env_requires_api_key() {
        let err = RuntimeEndpoint::from_lha_env_with_lookup(
            "sdk",
            lookup(&[(LHA_BASE_URL_ENV_VAR, "https://api.example.com/v1")]),
        )
        .expect_err("missing api key should fail");

        assert!(matches!(err, Error::EnvVar { var, .. } if var == LHA_API_KEY_ENV_VAR));
    }

    #[test]
    fn from_lha_env_rejects_invalid_endpoint_kind() {
        let err = RuntimeEndpoint::from_lha_env_with_lookup(
            "sdk",
            lookup(&[
                (LHA_BASE_URL_ENV_VAR, "https://api.example.com/v1"),
                (LHA_API_KEY_ENV_VAR, "secret"),
                (LHA_ENDPOINT_ENV_VAR, "bad"),
            ]),
        )
        .expect_err("invalid endpoint should fail");

        assert!(
            matches!(err, Error::InvalidRequest { message } if message.contains("chat, responses, messages"))
        );
    }

    #[test]
    fn infer_compatible_defaults_to_chat_for_plain_base_url() {
        let endpoint = RuntimeEndpoint::infer_compatible(
            "sdk",
            "https://api.example.com/v1",
            "TEST_API_KEY",
            None,
        )
        .expect("endpoint should be inferred");

        assert!(endpoint.uses_chat_completions_api());
        assert_eq!(
            endpoint.base_url.as_deref(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(endpoint.env_key.as_deref(), Some("TEST_API_KEY"));
    }

    #[test]
    fn infer_compatible_detects_chat_completions_suffix() {
        let endpoint = RuntimeEndpoint::infer_compatible(
            "sdk",
            "https://api.example.com/v1/chat/completions",
            "TEST_API_KEY",
            None,
        )
        .expect("endpoint should be inferred");

        assert!(endpoint.uses_chat_completions_api());
        assert_eq!(
            endpoint.base_url.as_deref(),
            Some("https://api.example.com/v1")
        );
    }

    #[test]
    fn infer_compatible_detects_responses_suffix() {
        let endpoint = RuntimeEndpoint::infer_compatible(
            "sdk",
            "https://api.example.com/v1/responses",
            "TEST_API_KEY",
            None,
        )
        .expect("endpoint should be inferred");

        assert!(endpoint.uses_responses_api());
        assert_eq!(
            endpoint.base_url.as_deref(),
            Some("https://api.example.com/v1")
        );
    }

    #[test]
    fn infer_compatible_detects_messages_suffix() {
        let endpoint = RuntimeEndpoint::infer_compatible(
            "sdk",
            "https://api.anthropic.com/v1/messages",
            "TEST_API_KEY",
            None,
        )
        .expect("endpoint should be inferred");

        assert!(endpoint.uses_messages_api());
        assert_eq!(
            endpoint.base_url.as_deref(),
            Some("https://api.anthropic.com/v1")
        );
    }

    #[test]
    fn infer_compatible_strips_detected_endpoint_suffix() {
        let endpoint = RuntimeEndpoint::infer_compatible(
            "sdk",
            "https://api.example.com/v1/responses/",
            "TEST_API_KEY",
            None,
        )
        .expect("endpoint should be inferred");

        assert_eq!(
            endpoint.base_url.as_deref(),
            Some("https://api.example.com/v1")
        );
    }

    #[test]
    fn infer_compatible_rejects_query_string() {
        let err = RuntimeEndpoint::infer_compatible(
            "sdk",
            "https://api.example.com/v1/responses?api-version=2024-01-01&deployment=test",
            "TEST_API_KEY",
            None,
        )
        .expect_err("query string should fail");

        assert!(matches!(err, Error::InvalidRequest { message } if message.contains("query")));
    }

    #[test]
    fn custom_endpoint_rejects_query_string_when_building_provider() {
        let endpoint =
            RuntimeEndpoint::openai_compatible_chat("sdk", "https://api.example.com/v1?token=test");

        let err = endpoint
            .to_api_provider()
            .expect_err("query string should fail");

        assert!(matches!(err, Error::InvalidRequest { message } if message.contains("query")));
    }

    #[test]
    fn infer_compatible_rejects_fragment() {
        let err = RuntimeEndpoint::infer_compatible(
            "sdk",
            "https://api.example.com/v1#frag",
            "TEST_API_KEY",
            None,
        )
        .expect_err("fragment should fail");

        assert!(matches!(err, Error::InvalidRequest { message } if message.contains("fragment")));
    }

    #[test]
    fn infer_compatible_rejects_url_suffix_and_endpoint_override_conflict() {
        let err = RuntimeEndpoint::infer_compatible(
            "sdk",
            "https://api.example.com/v1/responses",
            "TEST_API_KEY",
            Some(RuntimeEndpointKind::Chat),
        )
        .expect_err("conflict should fail");

        assert!(
            matches!(err, Error::InvalidRequest { message } if message.contains("responses") && message.contains("chat"))
        );
    }
}
