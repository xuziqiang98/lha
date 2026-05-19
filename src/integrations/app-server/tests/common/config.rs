use adam_agent::features::FEATURES;
use adam_agent::features::Feature;
use std::collections::BTreeMap;
use std::path::Path;

pub fn write_mock_responses_config_toml(
    adam_home: &Path,
    server_uri: &str,
    feature_flags: &BTreeMap<Feature, bool>,
    auto_compact_limit: i64,
    requires_openai_auth: Option<bool>,
    model_provider_id: &str,
    compact_prompt: &str,
) -> std::io::Result<()> {
    write_mock_responses_config_toml_with_options(
        adam_home,
        server_uri,
        feature_flags,
        auto_compact_limit,
        requires_openai_auth,
        model_provider_id,
        "mock-model",
        compact_prompt,
        "never",
        "read-only",
    )
}

pub fn write_mock_responses_config_toml_with_options(
    adam_home: &Path,
    server_uri: &str,
    feature_flags: &BTreeMap<Feature, bool>,
    auto_compact_limit: i64,
    requires_openai_auth: Option<bool>,
    model_provider_id: &str,
    model: &str,
    compact_prompt: &str,
    approval_policy: &str,
    sandbox_mode: &str,
) -> std::io::Result<()> {
    // Phase 1: build the features block for config.toml.
    let mut features = BTreeMap::from([(Feature::RemoteModels, false)]);
    for (feature, enabled) in feature_flags {
        features.insert(*feature, *enabled);
    }
    let feature_entries = features
        .into_iter()
        .map(|(feature, enabled)| {
            let key = FEATURES
                .iter()
                .find(|spec| spec.id == feature)
                .map(|spec| spec.key)
                .unwrap_or_else(|| panic!("missing feature key for {feature:?}"));
            format!("{key} = {enabled}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    write_mock_responses_models_json(
        adam_home,
        server_uri,
        model_provider_id,
        requires_openai_auth.unwrap_or(false),
        Some(auto_compact_limit),
        model,
    )?;
    write_state_json(adam_home, &format!("{model_provider_id}.main:{model}"))?;
    let config_toml = adam_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
approval_policy = "{approval_policy}"
sandbox_mode = "{sandbox_mode}"
compact_prompt = "{compact_prompt}"

[features]
{feature_entries}
"#
        ),
    )
}

pub fn write_state_json(adam_home: &Path, model_ref: &str) -> std::io::Result<()> {
    std::fs::write(
        adam_home.join("state.json"),
        format!(
            r#"{{
  "last_selected_model": {{
    "model_ref": "{model_ref}",
    "selected_at": null
  }},
  "last_reasoning_effort": null,
  "last_model_verbosity": null,
  "last_selected_identity": null
}}
"#
        ),
    )
}

pub fn write_mock_responses_models_json(
    adam_home: &Path,
    server_uri: &str,
    provider_id: &str,
    _requires_openai_auth: bool,
    auto_compact_limit: Option<i64>,
    model: &str,
) -> std::io::Result<()> {
    let auto_compact = auto_compact_limit
        .map(|limit| format!(",\n              \"auto_compact_token_limit\": {limit}"))
        .unwrap_or_default();
    std::fs::write(
        adam_home.join("models.json"),
        format!(
            r#"{{
  "providers": {{
    "{provider_id}": {{
      "name": "{provider_id}",
      "endpoints": {{
        "main": {{
          "name": "{provider_id}",
          "base_url": "{server_uri}/v1",
          "dialect": "responses",
          "bearer_token": "sk-test",
          "request_max_retries": 0,
          "stream_max_retries": 0,
          "models": {{
            "{model}": {{
              "context_window": 100000{auto_compact}
            }}
          }}
        }}
      }}
    }}
  }}
}}
"#
        ),
    )
}
