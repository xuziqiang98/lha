use std::path::Path;
use std::time::Duration;

use codex_api::ApiError;
use codex_api::AuthProvider;
use codex_api::ModelsClient;
use codex_api::Provider;
use codex_api::ReqwestTransport;
use codex_api::TransportError;
use codex_api::WireApi as ApiWireApi;
use codex_api::provider::RetryConfig;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_core::config::generated_provider_profile_name;
use codex_core::default_client::build_reqwest_client;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use toml_edit::value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ApiProviderWizardStep {
    #[default]
    ProviderId,
    WireApi,
    BaseUrl,
    ApiKey,
    Model,
    ContextWindow,
}

impl ApiProviderWizardStep {
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::ProviderId => 1,
            Self::WireApi => 2,
            Self::BaseUrl => 3,
            Self::ApiKey => 4,
            Self::Model => 5,
            Self::ContextWindow => 6,
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::ProviderId => "Provider ID",
            Self::WireApi => "Wire API",
            Self::BaseUrl => "Base URL",
            Self::ApiKey => "API Key",
            Self::Model => "Model",
            Self::ContextWindow => "Context Window",
        }
    }

    pub(crate) const fn placeholder(self) -> &'static str {
        match self {
            Self::ProviderId => "my-provider",
            Self::WireApi => "",
            Self::BaseUrl => "https://example.com/v1",
            Self::ApiKey => "Paste or type your API key",
            Self::Model => "gpt-4.1",
            Self::ContextWindow => "Optional, e.g. 128000",
        }
    }

    pub(crate) const fn previous(self) -> Option<Self> {
        match self {
            Self::ProviderId => None,
            Self::WireApi => Some(Self::ProviderId),
            Self::BaseUrl => Some(Self::WireApi),
            Self::ApiKey => Some(Self::BaseUrl),
            Self::Model => Some(Self::ApiKey),
            Self::ContextWindow => Some(Self::Model),
        }
    }

    pub(crate) const fn next(self) -> Option<Self> {
        match self {
            Self::ProviderId => Some(Self::WireApi),
            Self::WireApi => Some(Self::BaseUrl),
            Self::BaseUrl => Some(Self::ApiKey),
            Self::ApiKey => Some(Self::Model),
            Self::Model => Some(Self::ContextWindow),
            Self::ContextWindow => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ApiProviderWireApi {
    #[default]
    Chat,
    Responses,
    Messages,
}

impl ApiProviderWireApi {
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

    pub(crate) fn as_api_wire(self) -> ApiWireApi {
        match self {
            Self::Chat => ApiWireApi::Chat,
            Self::Responses => ApiWireApi::Responses,
            Self::Messages => ApiWireApi::Messages,
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
    pub(crate) wire_api: ApiProviderWireApi,
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
    pub(crate) wire_api: ApiProviderWireApi,
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

pub(crate) fn generated_profile_name(provider_id: &str, model: &str) -> String {
    generated_provider_profile_name(provider_id, model)
}

pub(crate) fn current_step_value_mut(state: &mut ApiKeyInputState) -> Option<&mut String> {
    match state.step {
        ApiProviderWizardStep::ProviderId => Some(&mut state.provider_id),
        ApiProviderWizardStep::WireApi => None,
        ApiProviderWizardStep::BaseUrl => Some(&mut state.base_url),
        ApiProviderWizardStep::ApiKey => Some(&mut state.api_key),
        ApiProviderWizardStep::Model => Some(&mut state.model),
        ApiProviderWizardStep::ContextWindow => Some(&mut state.model_context_window),
    }
}

pub(crate) fn validate_current_step(state: &ApiKeyInputState) -> Result<(), String> {
    match state.step {
        ApiProviderWizardStep::ProviderId => validate_provider_id(state.provider_id.trim()),
        ApiProviderWizardStep::WireApi => Ok(()),
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
        wire_api: state.wire_api,
        base_url,
        api_key,
        model,
        model_context_window,
    })
}

pub(crate) async fn persist_custom_provider_config(
    codex_home: &Path,
    config: &CustomProviderConfig,
) -> Result<(), String> {
    validate_custom_provider_config(config).await?;

    ConfigEditsBuilder::new(codex_home)
        .with_edits(build_custom_provider_edits(config))
        .apply()
        .await
        .map_err(|err| format!("Failed to write config.toml: {err}"))
}

async fn validate_custom_provider_config(config: &CustomProviderConfig) -> Result<(), String> {
    let provider = Provider {
        name: config.provider_id.clone(),
        base_url: config.base_url.clone(),
        query_params: None,
        wire: config.wire_api.as_api_wire(),
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

pub(crate) fn build_custom_provider_edits(config: &CustomProviderConfig) -> Vec<ConfigEdit> {
    let profile_name = generated_profile_name(&config.provider_id, &config.model);
    let mut edits = vec![
        ConfigEdit::SetPath {
            segments: vec!["model_provider".to_string()],
            value: value(config.provider_id.clone()),
        },
        ConfigEdit::SetPath {
            segments: vec!["model".to_string()],
            value: value(config.model.clone()),
        },
        ConfigEdit::SetPath {
            segments: vec![
                "model_providers".to_string(),
                config.provider_id.clone(),
                "name".to_string(),
            ],
            value: value(config.provider_id.clone()),
        },
        ConfigEdit::SetPath {
            segments: vec![
                "model_providers".to_string(),
                config.provider_id.clone(),
                "base_url".to_string(),
            ],
            value: value(config.base_url.clone()),
        },
        ConfigEdit::SetPath {
            segments: vec![
                "model_providers".to_string(),
                config.provider_id.clone(),
                "wire_api".to_string(),
            ],
            value: value(config.wire_api.as_config_value()),
        },
        ConfigEdit::SetPath {
            segments: vec![
                "model_providers".to_string(),
                config.provider_id.clone(),
                "experimental_bearer_token".to_string(),
            ],
            value: value(config.api_key.clone()),
        },
        ConfigEdit::SetPath {
            segments: vec![
                "model_providers".to_string(),
                config.provider_id.clone(),
                "requires_openai_auth".to_string(),
            ],
            value: value(false),
        },
        ConfigEdit::SetPath {
            segments: vec![
                "profiles".to_string(),
                profile_name.clone(),
                "model_provider".to_string(),
            ],
            value: value(config.provider_id.clone()),
        },
        ConfigEdit::SetPath {
            segments: vec!["profiles".to_string(), profile_name, "model".to_string()],
            value: value(config.model.clone()),
        },
    ];

    match config.model_context_window {
        Some(model_context_window) => {
            edits.push(ConfigEdit::SetPath {
                segments: vec!["model_context_window".to_string()],
                value: value(model_context_window),
            });
            edits.push(ConfigEdit::SetPath {
                segments: vec![
                    "profiles".to_string(),
                    generated_profile_name(&config.provider_id, &config.model),
                    "model_context_window".to_string(),
                ],
                value: value(model_context_window),
            });
        }
        None => {
            edits.push(ConfigEdit::ClearPath {
                segments: vec!["model_context_window".to_string()],
            });
        }
    }

    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;
    use toml_edit::DocumentMut;

    #[test]
    fn custom_provider_edits_write_expected_config() {
        let codex_home = tempdir().expect("temp dir");
        let config = CustomProviderConfig {
            provider_id: "custom_1".to_string(),
            wire_api: ApiProviderWireApi::Responses,
            base_url: "https://example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "gpt-test".to_string(),
            model_context_window: None,
        };

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&config))
            .apply_blocking()
            .expect("write config");

        let raw = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
        assert_eq!(
            raw,
            r#"model_provider = "custom_1"
model = "gpt-test"

[model_providers.custom_1]
name = "custom_1"
base_url = "https://example.com/v1"
wire_api = "responses"
experimental_bearer_token = "sk-test"
requires_openai_auth = false

[profiles."_provider.custom_1.gpt-test"]
model_provider = "custom_1"
model = "gpt-test"
"#
        );
    }

    #[test]
    fn api_provider_wire_api_messages_helpers_are_consistent() {
        assert_eq!(ApiProviderWireApi::default(), ApiProviderWireApi::Chat);
        assert_eq!(
            ApiProviderWireApi::all(),
            [
                ApiProviderWireApi::Chat,
                ApiProviderWireApi::Responses,
                ApiProviderWireApi::Messages,
            ]
        );
        assert_eq!(
            ApiProviderWireApi::Chat.next(),
            ApiProviderWireApi::Responses
        );
        assert_eq!(
            ApiProviderWireApi::Responses.next(),
            ApiProviderWireApi::Messages
        );
        assert_eq!(
            ApiProviderWireApi::Messages.next(),
            ApiProviderWireApi::Chat
        );
        assert_eq!(
            ApiProviderWireApi::Chat.previous(),
            ApiProviderWireApi::Messages
        );
        assert_eq!(
            ApiProviderWireApi::Messages.previous(),
            ApiProviderWireApi::Responses
        );
        assert_eq!(
            ApiProviderWireApi::from_shortcut_digit('3'),
            Some(ApiProviderWireApi::Messages)
        );
        assert_eq!(
            ApiProviderWireApi::from_shortcut_letter('m'),
            Some(ApiProviderWireApi::Messages)
        );
        assert_eq!(ApiProviderWireApi::Messages.as_config_value(), "messages");
        assert_eq!(
            ApiProviderWireApi::Messages.as_api_wire(),
            ApiWireApi::Messages
        );
        assert_eq!(ApiProviderWireApi::Messages.label(), "messages");
    }

    #[test]
    fn custom_provider_edits_write_messages_wire_api() {
        let codex_home = tempdir().expect("temp dir");
        let config = CustomProviderConfig {
            provider_id: "anthropic".to_string(),
            wire_api: ApiProviderWireApi::Messages,
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key: "sk-ant-test".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            model_context_window: None,
        };

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&config))
            .apply_blocking()
            .expect("write config");

        let raw = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
        let doc = raw.parse::<DocumentMut>().expect("parse config");

        assert_eq!(
            doc["model_providers"]["anthropic"]["wire_api"].as_str(),
            Some("messages")
        );
        assert_eq!(doc["model_provider"].as_str(), Some("anthropic"));
        assert_eq!(
            doc["profiles"]["_provider.anthropic.claude-sonnet-4-20250514"]["model"].as_str(),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn saving_second_model_for_same_provider_keeps_one_provider_entry() {
        let codex_home = tempdir().expect("temp dir");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-old".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: None,
            }))
            .apply_blocking()
            .expect("write initial config");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Chat,
                base_url: "https://example.com/chat".to_string(),
                api_key: "sk-new".to_string(),
                model: "gpt-other".to_string(),
                model_context_window: None,
            }))
            .apply_blocking()
            .expect("write updated config");

        let raw = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
        assert_eq!(raw.matches("[model_providers.custom_1]").count(), 1);

        let doc = raw.parse::<DocumentMut>().expect("parse config");
        assert_eq!(doc["model_provider"].as_str(), Some("custom_1"));
        assert_eq!(doc["model"].as_str(), Some("gpt-other"));
        assert_eq!(
            doc["model_providers"]["custom_1"]["wire_api"].as_str(),
            Some("chat")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["base_url"].as_str(),
            Some("https://example.com/chat")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["experimental_bearer_token"].as_str(),
            Some("sk-new")
        );
        assert_eq!(
            doc["profiles"]["_provider.custom_1.gpt-test"]["model"].as_str(),
            Some("gpt-test")
        );
        assert_eq!(
            doc["profiles"]["_provider.custom_1.gpt-other"]["model"].as_str(),
            Some("gpt-other")
        );
        assert_eq!(
            doc["profiles"].as_table().map(toml_edit::Table::len),
            Some(2)
        );
    }

    #[test]
    fn resaving_same_provider_model_pair_updates_generated_profile_in_place() {
        let codex_home = tempdir().expect("temp dir");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-old".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: None,
            }))
            .apply_blocking()
            .expect("write initial config");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Chat,
                base_url: "https://example.com/chat".to_string(),
                api_key: "sk-new".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: None,
            }))
            .apply_blocking()
            .expect("rewrite same pair");

        let raw = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
        assert_eq!(
            raw.matches("[profiles.\"_provider.custom_1.gpt-test\"]")
                .count(),
            1
        );

        let doc = raw.parse::<DocumentMut>().expect("parse config");
        assert_eq!(doc["model_provider"].as_str(), Some("custom_1"));
        assert_eq!(doc["model"].as_str(), Some("gpt-test"));
        assert_eq!(
            doc["model_providers"]["custom_1"]["wire_api"].as_str(),
            Some("chat")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["base_url"].as_str(),
            Some("https://example.com/chat")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["experimental_bearer_token"].as_str(),
            Some("sk-new")
        );
        assert_eq!(
            doc["profiles"]["_provider.custom_1.gpt-test"]["model_provider"].as_str(),
            Some("custom_1")
        );
        assert_eq!(
            doc["profiles"].as_table().map(toml_edit::Table::len),
            Some(1)
        );
    }

    #[test]
    fn saving_existing_provider_preserves_unmanaged_fields() {
        let codex_home = tempdir().expect("temp dir");
        let config_path = codex_home.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"model_provider = "custom_1"
model = "gpt-test"

[model_providers.custom_1]
name = "custom_1"
base_url = "https://example.com/v1"
wire_api = "responses"
experimental_bearer_token = "sk-old"
env_key = "CUSTOM_API_KEY"

[model_providers.custom_1.query_params]
api-version = "2025-04-01-preview"

[model_providers.custom_1.http_headers]
X-Test = "1"
"#,
        )
        .expect("write initial config");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Chat,
                base_url: "https://example.com/chat".to_string(),
                api_key: "sk-new".to_string(),
                model: "gpt-other".to_string(),
                model_context_window: None,
            }))
            .apply_blocking()
            .expect("rewrite provider");

        let raw = std::fs::read_to_string(config_path).expect("read config");
        let doc = raw.parse::<DocumentMut>().expect("parse config");

        assert_eq!(
            doc["model_providers"]["custom_1"]["env_key"].as_str(),
            Some("CUSTOM_API_KEY")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["query_params"]["api-version"].as_str(),
            Some("2025-04-01-preview")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["http_headers"]["X-Test"].as_str(),
            Some("1")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["wire_api"].as_str(),
            Some("chat")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["base_url"].as_str(),
            Some("https://example.com/chat")
        );
        assert_eq!(
            doc["model_providers"]["custom_1"]["experimental_bearer_token"].as_str(),
            Some("sk-new")
        );
        assert_eq!(
            doc["profiles"]["_provider.custom_1.gpt-other"]["model"].as_str(),
            Some("gpt-other")
        );
    }

    #[test]
    fn custom_provider_edits_write_context_window_when_set() {
        let codex_home = tempdir().expect("temp dir");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-test".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: Some(128_000),
            }))
            .apply_blocking()
            .expect("write config");

        let raw = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
        let doc = raw.parse::<DocumentMut>().expect("parse config");
        assert_eq!(doc["model_context_window"].as_integer(), Some(128_000));
        assert_eq!(
            doc["profiles"]["_provider.custom_1.gpt-test"]["model_context_window"].as_integer(),
            Some(128_000)
        );
    }

    #[test]
    fn resaving_same_provider_model_with_blank_context_window_clears_top_level_and_preserves_profile()
     {
        let codex_home = tempdir().expect("temp dir");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-old".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: Some(64_000),
            }))
            .apply_blocking()
            .expect("write initial config");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Chat,
                base_url: "https://example.com/chat".to_string(),
                api_key: "sk-new".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: None,
            }))
            .apply_blocking()
            .expect("rewrite same pair");

        let raw = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
        let doc = raw.parse::<DocumentMut>().expect("parse config");
        assert!(doc.get("model_context_window").is_none());
        assert_eq!(
            doc["profiles"]["_provider.custom_1.gpt-test"]["model_context_window"].as_integer(),
            Some(64_000)
        );
    }

    #[test]
    fn saving_different_model_with_blank_context_window_clears_stale_top_level_value() {
        let codex_home = tempdir().expect("temp dir");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_1".to_string(),
                wire_api: ApiProviderWireApi::Responses,
                base_url: "https://example.com/v1".to_string(),
                api_key: "sk-old".to_string(),
                model: "gpt-test".to_string(),
                model_context_window: Some(64_000),
            }))
            .apply_blocking()
            .expect("write initial config");

        ConfigEditsBuilder::new(codex_home.path())
            .with_edits(build_custom_provider_edits(&CustomProviderConfig {
                provider_id: "custom_2".to_string(),
                wire_api: ApiProviderWireApi::Chat,
                base_url: "https://example.com/chat".to_string(),
                api_key: "sk-new".to_string(),
                model: "gpt-other".to_string(),
                model_context_window: None,
            }))
            .apply_blocking()
            .expect("rewrite different pair");

        let raw = std::fs::read_to_string(codex_home.path().join("config.toml")).unwrap();
        let doc = raw.parse::<DocumentMut>().expect("parse config");
        assert!(doc.get("model_context_window").is_none());
        assert_eq!(
            doc["profiles"]["_provider.custom_1.gpt-test"]["model_context_window"].as_integer(),
            Some(64_000)
        );
        assert!(
            doc["profiles"]["_provider.custom_2.gpt-other"]
                .get("model_context_window")
                .is_none()
        );
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
