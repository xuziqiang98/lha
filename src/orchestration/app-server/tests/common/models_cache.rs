use chrono::DateTime;
use chrono::Utc;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use serde_json::json;
use std::path::Path;

/// Write a models_cache.json file to the codex home directory.
/// This prevents ModelsManager from making network requests to refresh models.
/// The cache will be treated as fresh (within TTL) and used instead of fetching from the network.
pub fn write_models_cache(codex_home: &Path) -> std::io::Result<()> {
    let response: ModelsResponse =
        serde_json::from_str(include_str!("../../../../harness/agent/models.json"))
            .map_err(std::io::Error::other)?;
    let models: Vec<ModelInfo> = response
        .models
        .into_iter()
        .filter(|model| model.visibility == ModelVisibility::List)
        .collect();

    write_models_cache_with_models(codex_home, models)
}

/// Write a models_cache.json file with specific models.
/// Useful when tests need specific models to be available.
pub fn write_models_cache_with_models(
    codex_home: &Path,
    models: Vec<ModelInfo>,
) -> std::io::Result<()> {
    let cache_path = codex_home.join("models_cache.json");
    // DateTime<Utc> serializes to RFC3339 format by default with serde
    let fetched_at: DateTime<Utc> = Utc::now();
    let cache = json!({
        "fetched_at": fetched_at,
        "etag": null,
        "models": models
    });
    std::fs::write(cache_path, serde_json::to_string_pretty(&cache)?)
}
