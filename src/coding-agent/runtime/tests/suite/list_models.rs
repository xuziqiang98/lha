use anyhow::Result;
use codex_agent::CodexAuth;
use codex_agent::ThreadManager;
use codex_agent::models_manager::manager::RefreshStrategy;
use codex_agent::models_manager::model_presets::all_model_presets;
use codex_llm::built_in_runtime_endpoints;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_returns_api_key_models() -> Result<()> {
    let adam_home = tempdir()?;
    let config = load_default_config_for_test(&adam_home).await;
    let manager = ThreadManager::with_models_provider(
        CodexAuth::from_api_key("sk-test"),
        built_in_runtime_endpoints()["openai"].clone(),
    );
    let models = manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;

    assert_eq!(expected_models(false), models);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_models_returns_chatgpt_models() -> Result<()> {
    let adam_home = tempdir()?;
    let config = load_default_config_for_test(&adam_home).await;
    let manager = ThreadManager::with_models_provider(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        built_in_runtime_endpoints()["openai"].clone(),
    );
    let models = manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;

    assert_eq!(expected_models(true), models);

    Ok(())
}

fn expected_models(chatgpt_mode: bool) -> Vec<ModelPreset> {
    let response: ModelsResponse = serde_json::from_str(include_str!("../../models.json"))
        .unwrap_or_else(|err| panic!("models.json should parse: {err}"));
    let builtin_presets = all_model_presets().clone();
    let remote_presets: Vec<ModelPreset> = response.models.into_iter().map(Into::into).collect();
    let mut merged = ModelPreset::merge(remote_presets, builtin_presets.clone());
    merged = ModelPreset::filter_by_auth(merged, chatgpt_mode);
    let builtin_model_slugs = builtin_presets
        .iter()
        .map(|preset| preset.model.as_str())
        .collect::<std::collections::HashSet<_>>();
    for preset in &mut merged {
        preset.model_provider_id = Some(
            if chatgpt_mode || builtin_model_slugs.contains(preset.model.as_str()) {
                "openai"
            } else {
                "test-provider"
            }
            .to_string(),
        );
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
