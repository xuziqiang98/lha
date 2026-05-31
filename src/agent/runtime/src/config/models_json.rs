use crate::config::model_ref::ModelRef;
use crate::path_utils::write_atomically;
use lha_llm::RuntimeEndpoint;
use lha_protocol::config_types::ReasoningSummary;
use lha_protocol::config_types::Verbosity;
use lha_protocol::openai_models::ReasoningEffort;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::io;
use std::path::Path;

pub const MODELS_JSON_FILE: &str = "models.json";

pub fn models_json_path(lha_home: &Path) -> std::path::PathBuf {
    lha_home.join(MODELS_JSON_FILE)
}

pub fn has_models_json(lha_home: &Path) -> bool {
    models_json_path(lha_home).is_file()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ModelsJson {
    #[serde(default)]
    pub providers: BTreeMap<String, ModelsProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveModelProviderError {
    model: String,
    provider_ids: Vec<String>,
}

impl ResolveModelProviderError {
    fn ambiguous(model: &str, provider_ids: BTreeSet<String>) -> Self {
        Self {
            model: model.to_string(),
            provider_ids: provider_ids.into_iter().collect(),
        }
    }
}

impl std::fmt::Display for ResolveModelProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "model `{}` is configured for multiple providers: {}",
            self.model,
            self.provider_ids.join(", ")
        )
    }
}

impl std::error::Error for ResolveModelProviderError {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ModelsProvider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub endpoints: BTreeMap<String, ModelsEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ModelsEndpoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub dialect: ModelsDialect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_params: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_http_headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_max_retries: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_max_retries: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_ms: Option<u64>,
    #[serde(default)]
    pub supports_realtime_streaming: bool,
    #[serde(default)]
    pub models: BTreeMap<String, ModelMetadata>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ModelsDialect {
    Responses,
    #[default]
    Chat,
    Messages,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ModelMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<ReasoningSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<Verbosity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_summaries: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_verbosity: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
}

impl ModelsJson {
    pub fn load_from_lha_home(lha_home: &Path) -> io::Result<Self> {
        let path = models_json_path(lha_home);
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let config: Self = serde_json::from_str(&contents).map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("failed to parse {}: {err}", path.display()),
                    )
                })?;
                config.validate()?;
                Ok(config)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub fn save_to_lha_home(&self, lha_home: &Path) -> io::Result<()> {
        self.validate()?;
        let contents = serde_json::to_string_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        write_atomically(&lha_home.join(MODELS_JSON_FILE), &format!("{contents}\n"))
    }

    pub fn validate(&self) -> io::Result<()> {
        for (provider_id, provider) in &self.providers {
            validate_id("provider", provider_id)?;
            if provider.endpoints.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "models.json provider `{provider_id}` must define at least one endpoint"
                    ),
                ));
            }
            for endpoint_id in provider.endpoints.keys() {
                validate_id("endpoint", endpoint_id)?;
            }
        }
        Ok(())
    }

    pub fn to_runtime_endpoints(&self) -> HashMap<String, RuntimeEndpoint> {
        let mut endpoints = HashMap::new();
        for (provider_id, provider) in &self.providers {
            for (endpoint_id, endpoint) in &provider.endpoints {
                let provider_ref = provider_ref(provider_id, endpoint_id);
                let name = endpoint
                    .name
                    .clone()
                    .or_else(|| provider.name.clone())
                    .unwrap_or_else(|| provider_ref.clone());
                let mut runtime = match endpoint.dialect {
                    ModelsDialect::Responses => RuntimeEndpoint::openai_compatible_responses(
                        name,
                        endpoint.base_url.clone().unwrap_or_default(),
                    ),
                    ModelsDialect::Chat => RuntimeEndpoint::openai_compatible_chat(
                        name,
                        endpoint.base_url.clone().unwrap_or_default(),
                    ),
                    ModelsDialect::Messages => RuntimeEndpoint::anthropic_compatible_messages(
                        name,
                        endpoint.base_url.clone().unwrap_or_default(),
                    ),
                };
                if endpoint.base_url.is_none() {
                    runtime.base_url = None;
                }
                runtime.env_key = endpoint.env_key.clone();
                runtime.env_key_instructions = endpoint.env_key_instructions.clone();
                runtime.bearer_token = endpoint.bearer_token.clone();
                runtime.query_params = endpoint.query_params.clone();
                runtime.http_headers = endpoint.http_headers.clone();
                runtime.env_http_headers = endpoint.env_http_headers.clone();
                runtime.request_max_retries = endpoint.request_max_retries;
                runtime.stream_max_retries = endpoint.stream_max_retries;
                runtime.stream_idle_timeout_ms = endpoint.stream_idle_timeout_ms;
                runtime.set_realtime_turn_streaming_enabled(endpoint.supports_realtime_streaming);
                endpoints.insert(provider_ref, runtime);
            }
        }
        endpoints
    }

    pub fn model_entries(&self) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        for (provider_id, provider) in &self.providers {
            for (endpoint_id, endpoint) in &provider.endpoints {
                for model_id in endpoint.models.keys() {
                    entries.push((model_id.clone(), provider_ref(provider_id, endpoint_id)));
                }
            }
        }
        entries
    }

    pub fn resolve_model_provider_for_model(
        &self,
        model: &str,
    ) -> Result<Option<String>, ResolveModelProviderError> {
        let model = model.trim();
        if model.is_empty() {
            return Ok(None);
        }

        let provider_ids: BTreeSet<String> = self
            .model_entries()
            .into_iter()
            .filter_map(|(configured_model, provider_id)| {
                (configured_model == model).then_some(provider_id)
            })
            .collect();

        match provider_ids.len() {
            0 => Ok(None),
            1 => Ok(provider_ids.into_iter().next()),
            _ => Err(ResolveModelProviderError::ambiguous(model, provider_ids)),
        }
    }

    pub fn model_metadata(&self, model_ref: &ModelRef) -> Option<&ModelMetadata> {
        self.providers
            .get(&model_ref.provider_id)?
            .endpoints
            .get(&model_ref.endpoint_id)?
            .models
            .get(&model_ref.model_id)
    }
}

pub fn provider_ref(provider_id: &str, endpoint_id: &str) -> String {
    if endpoint_id == "main" {
        provider_id.to_string()
    } else {
        format!("{provider_id}.{endpoint_id}")
    }
}

fn validate_id(kind: &str, value: &str) -> io::Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "models.json {kind} id `{value}` must contain only letters, digits, '_' or '-'"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::MODELS_JSON_FILE;
    use super::ModelsJson;
    use pretty_assertions::assert_eq;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn parses_provider_endpoint_models() {
        let config: ModelsJson = serde_json::from_str(
            r#"{
              "providers": {
                "openrouter": {
                  "name": "OpenRouter",
                  "endpoints": {
                    "main": {
                      "base_url": "https://openrouter.ai/api/v1",
                      "env_key": "OPENROUTER_API_KEY",
                      "models": {
                        "anthropic/claude-sonnet-4": { "context_window": 200000 }
                      }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();
        assert_eq!(
            config.model_entries(),
            vec![(
                "anthropic/claude-sonnet-4".to_string(),
                "openrouter".to_string()
            )]
        );
    }

    #[test]
    fn resolve_model_provider_for_model_uses_unique_model_entry() {
        let config: ModelsJson = serde_json::from_str(
            r#"{
              "providers": {
                "iie": {
                  "endpoints": {
                    "main": {
                      "models": {
                        "deepseek-v3": {}
                      }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config
                .resolve_model_provider_for_model("deepseek-v3")
                .expect("resolve provider"),
            Some("iie".to_string())
        );
    }

    #[test]
    fn resolve_model_provider_for_model_returns_none_when_unmapped() {
        let config: ModelsJson = serde_json::from_str(
            r#"{
              "providers": {
                "iie": {
                  "endpoints": {
                    "main": {
                      "models": {
                        "deepseek-v3": {}
                      }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();

        assert_eq!(
            config
                .resolve_model_provider_for_model("claude-3.7")
                .expect("resolve provider"),
            None
        );
    }

    #[test]
    fn resolve_model_provider_for_model_rejects_ambiguous_mapping() {
        let config: ModelsJson = serde_json::from_str(
            r#"{
              "providers": {
                "iie": {
                  "endpoints": {
                    "main": {
                      "models": {
                        "deepseek-v3": {}
                      }
                    }
                  }
                },
                "chatanywhere": {
                  "endpoints": {
                    "main": {
                      "models": {
                        "deepseek-v3": {}
                      }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();

        let err = config
            .resolve_model_provider_for_model("deepseek-v3")
            .expect_err("expected ambiguous provider mapping");
        assert_eq!(
            err.to_string(),
            "model `deepseek-v3` is configured for multiple providers: chatanywhere, iie"
        );
    }

    #[test]
    fn resolve_model_provider_for_model_rejects_ambiguous_variant_mapping() {
        let config: ModelsJson = serde_json::from_str(
            r#"{
              "providers": {
                "anthropic": {
                  "endpoints": {
                    "messages": {
                      "models": {
                        "claude-sonnet-4-5": {}
                      }
                    },
                    "chat": {
                      "models": {
                        "claude-sonnet-4-5": {}
                      }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();

        let err = config
            .resolve_model_provider_for_model("claude-sonnet-4-5")
            .expect_err("expected ambiguous provider mapping");
        assert_eq!(
            err.to_string(),
            "model `claude-sonnet-4-5` is configured for multiple providers: anthropic.chat, anthropic.messages"
        );
    }

    #[test]
    fn explicit_null_fields_match_missing_fields() {
        let with_nulls: ModelsJson = serde_json::from_str(
            r#"{
              "providers": {
                "custom": {
                  "name": null,
                  "endpoints": {
                    "chat": {
                      "name": null,
                      "base_url": "https://example.com/v1",
                      "env_key": null,
                      "env_key_instructions": null,
                      "bearer_token": "sk-test",
                      "query_params": null,
                      "http_headers": null,
                      "env_http_headers": null,
                      "request_max_retries": null,
                      "stream_max_retries": null,
                      "stream_idle_timeout_ms": null,
                      "models": {
                        "gpt-test": {
                          "display_name": null,
                          "context_window": null,
                          "auto_compact_token_limit": null,
                          "reasoning_effort": null,
                          "reasoning_summary": null,
                          "verbosity": null,
                          "supports_reasoning_summaries": null,
                          "supports_verbosity": null,
                          "input_modalities": null
                        }
                      }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();
        let without_nulls: ModelsJson = serde_json::from_str(
            r#"{
              "providers": {
                "custom": {
                  "endpoints": {
                    "chat": {
                      "base_url": "https://example.com/v1",
                      "bearer_token": "sk-test",
                      "models": {
                        "gpt-test": {}
                      }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();

        assert_eq!(with_nulls, without_nulls);
    }

    #[test]
    fn missing_file_loads_empty() {
        let temp = TempDir::new().unwrap();
        let config = ModelsJson::load_from_lha_home(temp.path()).unwrap();
        assert_eq!(config, ModelsJson::default());
    }

    #[test]
    fn invalid_json_returns_invalid_data() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(MODELS_JSON_FILE), "{").unwrap();
        let err = ModelsJson::load_from_lha_home(temp.path()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(MODELS_JSON_FILE), r#"{"unknown":true}"#).unwrap();
        let err = ModelsJson::load_from_lha_home(temp.path()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn old_experimental_bearer_token_field_is_rejected() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join(MODELS_JSON_FILE),
            r#"{
              "providers": {
                "custom": {
                  "endpoints": {
                    "chat": {
                      "base_url": "https://example.com/v1",
                      "experimental_bearer_token": "sk-test",
                      "models": {
                        "gpt-test": {}
                      }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();
        let err = ModelsJson::load_from_lha_home(temp.path()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert!(err.to_string().contains("experimental_bearer_token"));
    }

    #[test]
    fn invalid_provider_id_is_rejected() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join(MODELS_JSON_FILE),
            r#"{"providers":{"bad.provider":{"endpoints":{"main":{}}}}}"#,
        )
        .unwrap();
        let err = ModelsJson::load_from_lha_home(temp.path()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn invalid_endpoint_id_is_rejected() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join(MODELS_JSON_FILE),
            r#"{"providers":{"openrouter":{"endpoints":{"bad.endpoint":{}}}}}"#,
        )
        .unwrap();
        let err = ModelsJson::load_from_lha_home(temp.path()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }
}
