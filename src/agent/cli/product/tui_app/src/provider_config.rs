use std::path::Path;
use std::time::Duration;

use crate::product::agent::config::model_ref::ModelRef;
use crate::product::agent::config::models_json::ModelsDialect;
use crate::product::agent::config::models_json::ModelsEndpoint;
use crate::product::agent::config::models_json::ModelsJson;
use crate::product::agent::config::models_json::ModelsProvider;
use crate::product::agent::config::state_json::LHAStateStore;
use crate::product::agent::default_client::build_reqwest_client;
use lha_llm::api::ApiError;
use lha_llm::api::AuthProvider;
use lha_llm::api::ModelsClient;
use lha_llm::api::Provider;
use lha_llm::api::ReqwestTransport;
use lha_llm::api::TransportError;
use lha_llm::api::WireApi as ApiConversationDialect;
use lha_llm::api::provider::RetryConfig;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ApiProviderWizardStep {
    #[default]
    ProviderId,
    ConversationDialect,
    BaseUrl,
    ApiKey,
    Model,
    ContextWindow,
}

impl ApiProviderWizardStep {
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::ProviderId => 1,
            Self::ConversationDialect => 2,
            Self::BaseUrl => 3,
            Self::ApiKey => 4,
            Self::Model => 5,
            Self::ContextWindow => 6,
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::ProviderId => "Provider ID",
            Self::ConversationDialect => "Dialect",
            Self::BaseUrl => "Base URL",
            Self::ApiKey => "API Key",
            Self::Model => "Model",
            Self::ContextWindow => "Context Window",
        }
    }

    pub(crate) const fn placeholder(self) -> &'static str {
        match self {
            Self::ProviderId => "my-provider",
            Self::ConversationDialect => "",
            Self::BaseUrl => "https://example.com/v1",
            Self::ApiKey => "Paste or type your API key",
            Self::Model => "gpt-4.1",
            Self::ContextWindow => "Optional, e.g. 128000",
        }
    }

    pub(crate) const fn previous(self) -> Option<Self> {
        match self {
            Self::ProviderId => None,
            Self::ConversationDialect => Some(Self::ProviderId),
            Self::BaseUrl => Some(Self::ConversationDialect),
            Self::ApiKey => Some(Self::BaseUrl),
            Self::Model => Some(Self::ApiKey),
            Self::ContextWindow => Some(Self::Model),
        }
    }

    pub(crate) const fn next(self) -> Option<Self> {
        match self {
            Self::ProviderId => Some(Self::ConversationDialect),
            Self::ConversationDialect => Some(Self::BaseUrl),
            Self::BaseUrl => Some(Self::ApiKey),
            Self::ApiKey => Some(Self::Model),
            Self::Model => Some(Self::ContextWindow),
            Self::ContextWindow => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ApiProviderDialect {
    #[default]
    Chat,
    Responses,
    Messages,
}

impl ApiProviderDialect {
    pub(crate) const fn all() -> [Self; 3] {
        [Self::Chat, Self::Responses, Self::Messages]
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Chat => Self::Responses,
            Self::Responses => Self::Messages,
            Self::Messages => Self::Chat,
        }
    }

    pub(crate) const fn previous(self) -> Self {
        match self {
            Self::Chat => Self::Messages,
            Self::Responses => Self::Chat,
            Self::Messages => Self::Responses,
        }
    }

    pub(crate) fn toggle(&mut self) {
        *self = self.next();
    }

    pub(crate) const fn from_shortcut_digit(c: char) -> Option<Self> {
        match c {
            '1' => Some(Self::Chat),
            '2' => Some(Self::Responses),
            '3' => Some(Self::Messages),
            _ => None,
        }
    }

    pub(crate) const fn from_shortcut_letter(c: char) -> Option<Self> {
        match c {
            'c' | 'C' => Some(Self::Chat),
            'r' | 'R' => Some(Self::Responses),
            'm' | 'M' => Some(Self::Messages),
            _ => None,
        }
    }

    pub(crate) fn as_config_value(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }

    pub(crate) const fn as_models_dialect(self) -> ModelsDialect {
        match self {
            Self::Chat => ModelsDialect::Chat,
            Self::Responses => ModelsDialect::Responses,
            Self::Messages => ModelsDialect::Messages,
        }
    }

    pub(crate) fn as_api_dialect(self) -> ApiConversationDialect {
        match self {
            Self::Chat => ApiConversationDialect::Chat,
            Self::Responses => ApiConversationDialect::Responses,
            Self::Messages => ApiConversationDialect::Messages,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct ApiKeyInputState {
    pub(crate) provider_id: String,
    pub(crate) dialect: ApiProviderDialect,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) model: String,
    pub(crate) model_context_window: String,
    pub(crate) step: ApiProviderWizardStep,
    pub(crate) validating: bool,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CustomProviderConfig {
    pub(crate) provider_id: String,
    pub(crate) dialect: ApiProviderDialect,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) model: String,
    pub(crate) model_context_window: Option<i64>,
}

#[derive(Clone)]
struct StaticBearerAuth {
    api_key: String,
}

impl AuthProvider for StaticBearerAuth {
    fn bearer_token(&self) -> Option<String> {
        Some(self.api_key.clone())
    }
}

pub(crate) fn custom_provider_ref(config: &CustomProviderConfig) -> String {
    format!(
        "{}.{}",
        config.provider_id,
        config.dialect.as_config_value()
    )
}

pub(crate) fn current_step_value_mut(state: &mut ApiKeyInputState) -> Option<&mut String> {
    match state.step {
        ApiProviderWizardStep::ProviderId => Some(&mut state.provider_id),
        ApiProviderWizardStep::ConversationDialect => None,
        ApiProviderWizardStep::BaseUrl => Some(&mut state.base_url),
        ApiProviderWizardStep::ApiKey => Some(&mut state.api_key),
        ApiProviderWizardStep::Model => Some(&mut state.model),
        ApiProviderWizardStep::ContextWindow => Some(&mut state.model_context_window),
    }
}

pub(crate) fn validate_current_step(state: &ApiKeyInputState) -> Result<(), String> {
    match state.step {
        ApiProviderWizardStep::ProviderId => validate_provider_id(state.provider_id.trim()),
        ApiProviderWizardStep::ConversationDialect => Ok(()),
        ApiProviderWizardStep::BaseUrl => validate_base_url(state.base_url.trim()),
        ApiProviderWizardStep::ApiKey => validate_non_empty(state.api_key.trim(), "API key"),
        ApiProviderWizardStep::Model => validate_non_empty(state.model.trim(), "Model"),
        ApiProviderWizardStep::ContextWindow => {
            validate_context_window(state.model_context_window.trim())
        }
    }
}

fn validate_non_empty(value: &str, field_name: &str) -> Result<(), String> {
    if value.is_empty() {
        Err(format!("{field_name} cannot be empty"))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_provider_id(provider_id: &str) -> Result<(), String> {
    validate_non_empty(provider_id, "Provider ID")?;
    if provider_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        Ok(())
    } else {
        Err("Provider ID can only contain letters, numbers, '_' or '-'".to_string())
    }
}

fn validate_base_url(base_url: &str) -> Result<(), String> {
    validate_non_empty(base_url, "Base URL")?;
    let url = url::Url::parse(base_url).map_err(|err| format!("Invalid base URL: {err}"))?;
    match url.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(format!("Base URL must use http or https, got {scheme}")),
    }
}

fn validate_context_window(value: &str) -> Result<(), String> {
    let _ = parse_context_window(value)?;
    Ok(())
}

fn parse_context_window(value: &str) -> Result<Option<i64>, String> {
    if value.is_empty() {
        return Ok(None);
    }

    let normalized = value.replace('_', "");
    let parsed = normalized
        .parse::<i64>()
        .map_err(|_| "Context Window must be a positive integer".to_string())?;
    if parsed <= 0 {
        return Err("Context Window must be a positive integer".to_string());
    }

    Ok(Some(parsed))
}

pub(crate) fn snapshot_custom_provider_config(
    state: &ApiKeyInputState,
) -> Result<CustomProviderConfig, String> {
    let provider_id = state.provider_id.trim().to_string();
    let base_url = state.base_url.trim().to_string();
    let api_key = state.api_key.trim().to_string();
    let model = state.model.trim().to_string();
    let model_context_window = parse_context_window(state.model_context_window.trim())?;

    validate_provider_id(&provider_id)?;
    validate_base_url(&base_url)?;
    validate_non_empty(&api_key, "API key")?;
    validate_non_empty(&model, "Model")?;

    Ok(CustomProviderConfig {
        provider_id,
        dialect: state.dialect,
        base_url,
        api_key,
        model,
        model_context_window,
    })
}

pub(crate) async fn persist_custom_provider_config(
    lha_home: &Path,
    config: &CustomProviderConfig,
) -> Result<(), String> {
    validate_custom_provider_config(config).await?;

    persist_custom_provider_files(lha_home, config)
}

pub(crate) fn persist_custom_provider_files(
    lha_home: &Path,
    config: &CustomProviderConfig,
) -> Result<(), String> {
    let provider_ref = custom_provider_ref(config);
    let mut models_json = ModelsJson::load_from_lha_home(lha_home)
        .map_err(|err| format!("Failed to read models.json: {err}"))?;
    let provider = models_json
        .providers
        .entry(config.provider_id.clone())
        .or_insert_with(ModelsProvider::default);
    provider.name = Some(config.provider_id.clone());
    let endpoint = provider
        .endpoints
        .entry(config.dialect.as_config_value().to_string())
        .or_insert_with(|| ModelsEndpoint {
            name: None,
            base_url: None,
            env_key: None,
            env_key_instructions: None,
            bearer_token: None,
            dialect: config.dialect.as_models_dialect(),
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            supports_realtime_streaming: false,
            models: Default::default(),
        });
    endpoint.name = Some(config.provider_id.clone());
    endpoint.base_url = Some(config.base_url.clone());
    endpoint.env_key = None;
    endpoint.env_key_instructions = None;
    endpoint.bearer_token = Some(config.api_key.clone());
    endpoint.dialect = config.dialect.as_models_dialect();
    endpoint.query_params = None;
    endpoint.http_headers = None;
    endpoint.env_http_headers = None;
    let model_metadata = endpoint.models.entry(config.model.clone()).or_default();
    model_metadata.context_window = config.model_context_window;

    models_json
        .save_to_lha_home(lha_home)
        .map_err(|err| format!("Failed to write models.json: {err}"))?;
    let model_ref = ModelRef::parse(&format!("{provider_ref}:{}", config.model))
        .map_err(|err| format!("Invalid model selection: {err}"))?;
    LHAStateStore::new(lha_home)
        .set_last_selected_model(&model_ref, None, None)
        .map_err(|err| format!("Failed to write state.json: {err}"))
}

async fn validate_custom_provider_config(config: &CustomProviderConfig) -> Result<(), String> {
    let provider = Provider {
        name: config.provider_id.clone(),
        base_url: config.base_url.clone(),
        query_params: None,
        wire: config.dialect.as_api_dialect(),
        headers: HeaderMap::new(),
        retry: RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(200),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: Duration::from_secs(5),
    };
    let client = ModelsClient::new(
        ReqwestTransport::new(build_reqwest_client()),
        provider,
        StaticBearerAuth {
            api_key: config.api_key.clone(),
        },
    );

    match client
        .list_models(env!("CARGO_PKG_VERSION"), HeaderMap::new())
        .await
    {
        Ok(_) => Ok(()),
        Err(ApiError::Transport(TransportError::Http {
            status:
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED,
            ..
        })) => Ok(()),
        Err(ApiError::Transport(TransportError::Http {
            status: StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN,
            ..
        })) => Err(
            "Authentication failed while checking the provider. Verify the API key.".to_string(),
        ),
        Err(ApiError::Transport(TransportError::Timeout)) => {
            Err("Timed out while connecting to the provider.".to_string())
        }
        Err(ApiError::Transport(TransportError::RetryLimit)) => {
            Err("Could not connect to the provider after retrying.".to_string())
        }
        Err(ApiError::Transport(TransportError::Build(err)))
        | Err(ApiError::Transport(TransportError::Network(err))) => {
            Err(format!("Could not connect to the provider: {err}"))
        }
        Err(ApiError::Transport(TransportError::Http { .. })) | Err(ApiError::Stream(_)) => Ok(()),
        Err(ApiError::Api { status, message }) => {
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                Err(format!(
                    "Authentication failed while checking the provider: {message}"
                ))
            } else {
                Ok(())
            }
        }
        Err(
            ApiError::ContextWindowExceeded
            | ApiError::QuotaExceeded
            | ApiError::UsageNotIncluded
            | ApiError::Retryable { .. }
            | ApiError::RateLimit(_)
            | ApiError::InvalidRequest { .. },
        ) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn api_provider_dialect_messages_helpers_are_consistent() {
        assert_eq!(ApiProviderDialect::default(), ApiProviderDialect::Chat);
        assert_eq!(
            ApiProviderDialect::all(),
            [
                ApiProviderDialect::Chat,
                ApiProviderDialect::Responses,
                ApiProviderDialect::Messages,
            ]
        );
        assert_eq!(
            ApiProviderDialect::Chat.next(),
            ApiProviderDialect::Responses
        );
        assert_eq!(
            ApiProviderDialect::Responses.next(),
            ApiProviderDialect::Messages
        );
        assert_eq!(
            ApiProviderDialect::Messages.next(),
            ApiProviderDialect::Chat
        );
        assert_eq!(
            ApiProviderDialect::Chat.previous(),
            ApiProviderDialect::Messages
        );
        assert_eq!(
            ApiProviderDialect::Messages.previous(),
            ApiProviderDialect::Responses
        );
        assert_eq!(
            ApiProviderDialect::from_shortcut_digit('3'),
            Some(ApiProviderDialect::Messages)
        );
        assert_eq!(
            ApiProviderDialect::from_shortcut_letter('m'),
            Some(ApiProviderDialect::Messages)
        );
        assert_eq!(ApiProviderDialect::Messages.as_config_value(), "messages");
        assert_eq!(
            ApiProviderDialect::Messages.as_api_dialect(),
            ApiConversationDialect::Messages
        );
        assert_eq!(ApiProviderDialect::Messages.label(), "messages");
    }

    #[test]
    fn custom_provider_edits_write_context_window_when_set() {
        let lha_home = tempdir().expect("temp dir");

        persist_custom_provider_files(
            lha_home.path(),
            &CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                dialect: ApiProviderDialect::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-test".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: Some(128_000),
            },
        )
        .expect("write provider files");

        let models_raw = std::fs::read_to_string(lha_home.path().join("models.json")).unwrap();
        let models: serde_json::Value = serde_json::from_str(&models_raw).unwrap();
        assert_eq!(
            models["providers"]["custom_1"]["endpoints"]["responses"]["models"]["gpt-test"]
                ["context_window"]
                .as_i64(),
            Some(128_000)
        );
        let state_raw = std::fs::read_to_string(lha_home.path().join("state.json")).unwrap();
        let state: serde_json::Value = serde_json::from_str(&state_raw).unwrap();
        assert_eq!(
            state["last_selected_model"]["model_ref"].as_str(),
            Some("custom_1.responses:gpt-test")
        );
    }

    #[test]
    fn resaving_same_provider_model_with_blank_context_window_clears_metadata() {
        let lha_home = tempdir().expect("temp dir");

        persist_custom_provider_files(
            lha_home.path(),
            &CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                dialect: ApiProviderDialect::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-old".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: Some(64_000),
            },
        )
        .expect("write initial provider files");

        persist_custom_provider_files(
            lha_home.path(),
            &CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                dialect: ApiProviderDialect::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-new".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: None,
            },
        )
        .expect("rewrite provider files");

        let models_raw = std::fs::read_to_string(lha_home.path().join("models.json")).unwrap();
        let models: serde_json::Value = serde_json::from_str(&models_raw).unwrap();
        let metadata =
            models["providers"]["custom_1"]["endpoints"]["responses"]["models"]["gpt-test"]
                .as_object()
                .expect("model metadata should be an object");
        assert!(!metadata.contains_key("context_window"));
    }

    #[test]
    fn saving_different_model_with_blank_context_window_omits_empty_values() {
        let lha_home = tempdir().expect("temp dir");

        persist_custom_provider_files(
            lha_home.path(),
            &CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                dialect: ApiProviderDialect::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-old".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: Some(64_000),
            },
        )
        .expect("write initial provider files");

        persist_custom_provider_files(
            lha_home.path(),
            &CustomProviderConfig {
                provider_id: "custom_2".to_string(),
                dialect: ApiProviderDialect::Chat,
                base_url: "https://example.com/chat".to_string(),
                api_key: "sk-new".to_string(),
                model: "gpt-other".to_string(),
                model_context_window: None,
            },
        )
        .expect("rewrite provider files");

        let models_raw = std::fs::read_to_string(lha_home.path().join("models.json")).unwrap();
        let models: serde_json::Value = serde_json::from_str(&models_raw).unwrap();
        assert_eq!(
            models["providers"]["custom_1"]["endpoints"]["responses"]["models"]["gpt-test"]
                ["context_window"]
                .as_i64(),
            Some(64_000)
        );
        let endpoint = models["providers"]["custom_2"]["endpoints"]["chat"]
            .as_object()
            .expect("endpoint should be an object");
        assert!(!endpoint.contains_key("env_key"));
        assert!(!endpoint.contains_key("query_params"));
        assert!(!endpoint.contains_key("http_headers"));

        let metadata = endpoint["models"]["gpt-other"]
            .as_object()
            .expect("model metadata should be an object");
        assert!(!metadata.contains_key("context_window"));
    }

    #[test]
    fn context_window_validation_accepts_blank_and_rejects_invalid_values() {
        assert_eq!(parse_context_window("").unwrap(), None);
        assert_eq!(parse_context_window("128_000").unwrap(), Some(128_000));
        assert_eq!(
            parse_context_window("abc").unwrap_err(),
            "Context Window must be a positive integer"
        );
        assert_eq!(
            parse_context_window("0").unwrap_err(),
            "Context Window must be a positive integer"
        );
        assert_eq!(
            parse_context_window("-1").unwrap_err(),
            "Context Window must be a positive integer"
        );
    }
}
