use adam_agent::CodexAuth;
use adam_agent::ThreadManager;
use adam_agent::models_manager::manager::RefreshStrategy;
use adam_agent::models_manager::model_presets::all_model_presets;
use adam_llm::built_in_runtime_endpoints;
use adam_protocol::openai_models::ModelPreset;
use adam_protocol::openai_models::ModelsResponse;
use anyhow::Result;
use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_returns_api_key_models() -> Result<()> {
    let adam_home = tempdir()?;
    let config = load_default_config_for_test(&adam_home).await;
    let manager = ThreadManager::with_models_provider(
        CodexAuth::from_api_key("sk-test"),
        test_openai_endpoint(),
    );
    let models = manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;

    assert_eq!(expected_models(), models);

    Ok(())
}
fn test_openai_endpoint() -> adam_llm::RuntimeEndpoint {
    let mut endpoint = built_in_runtime_endpoints()["openai"].clone();
    endpoint.env_key = None;
    endpoint.env_key_instructions = None;
    endpoint.bearer_token = Some("sk-test".to_string());
    endpoint
}

fn expected_models() -> Vec<ModelPreset> {
    let response: ModelsResponse = serde_json::from_str(include_str!("../../models.json"))
        .unwrap_or_else(|err| panic!("models.json should parse: {err}"));
    let builtin_presets = all_model_presets().clone();
    let remote_presets: Vec<ModelPreset> = response.models.into_iter().map(Into::into).collect();
    let mut merged = ModelPreset::merge(remote_presets, builtin_presets);
    merged = ModelPreset::filter_by_api_support(merged, false);
    for preset in &mut merged {
        preset.model_provider_id = Some("openai".to_string());
    }

    for preset in &mut merged {
        preset.is_default = false;
    }
    if let Some(default) = merged.iter_mut().find(|preset| preset.show_in_picker) {
        default.is_default = true;
    } else if let Some(default) = merged.first_mut() {
        default.is_default = true;
    }

    merged
}
