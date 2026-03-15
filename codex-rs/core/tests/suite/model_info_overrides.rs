use codex_core::ModelProviderInfo;
use codex_core::auth::CodexAuth;
use codex_core::built_in_model_providers;
use codex_core::features::Feature;
use codex_core::models_manager::manager::ModelsManager;
use codex_protocol::openai_models::TruncationPolicyConfig;
use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_without_tool_output_override() {
    let codex_home = TempDir::new().expect("create temp dir");
    let config = load_default_config_for_test(&codex_home).await;

    let model_info = ModelsManager::construct_model_info_offline("gpt-5.1", &config);

    assert_eq!(model_info.description, None);
    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::bytes(10_000)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_with_tool_output_override() {
    let codex_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&codex_home).await;
    config.tool_output_token_limit = Some(123);

    let model_info = ModelsManager::construct_model_info_offline("gpt-5.1-codex", &config);

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::tokens(123)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundled_model_info_takes_precedence_over_builtin_fallback() {
    let codex_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&codex_home).await;
    config.features.enable(Feature::RemoteModels);

    let auth_manager = codex_core::auth::AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    );
    let models_manager = ModelsManager::with_provider(
        codex_home.path().to_path_buf(),
        auth_manager,
        "openai",
        ModelProviderInfo {
            ..built_in_model_providers()["openai"].clone()
        },
    );

    let model_info = models_manager.get_model_info("gpt-5.1", &config).await;

    assert_eq!(
        model_info.description.as_deref(),
        Some("Broad world knowledge with strong general reasoning.")
    );
}
