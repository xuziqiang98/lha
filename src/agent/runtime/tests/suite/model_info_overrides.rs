use adam_agent::auth::CodexAuth;
use adam_agent::features::Feature;
use adam_agent::models_manager::manager::ModelsManager;
use adam_llm::built_in_runtime_endpoints;
use adam_protocol::openai_models::ApplyPatchToolType;
use adam_protocol::openai_models::ConfigShellToolType;
use adam_protocol::openai_models::TruncationPolicyConfig;
use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_without_tool_output_override() {
    let adam_home = TempDir::new().expect("create temp dir");
    let config = load_default_config_for_test(&adam_home).await;

    let model_info = ModelsManager::construct_model_info_offline("gpt-5.1", &config);

    assert_eq!(model_info.description, None);
    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::bytes(10_000)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_with_tool_output_override() {
    let adam_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&adam_home).await;
    config.tool_output_token_limit = Some(123);

    let model_info = ModelsManager::construct_model_info_offline("gpt-5.1-codex", &config);

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::tokens(123)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_gpt_5_3_codex_uses_codex_fallback_metadata() {
    let adam_home = TempDir::new().expect("create temp dir");
    let config = load_default_config_for_test(&adam_home).await;

    let model_info = ModelsManager::construct_model_info_offline("gpt-5.3-codex", &config);

    assert_eq!(
        model_info.apply_patch_tool_type,
        Some(ApplyPatchToolType::Freeform)
    );
    assert_eq!(model_info.shell_type, ConfigShellToolType::ShellCommand);
    assert!(model_info.supports_parallel_tool_calls);
    assert!(model_info.supports_reasoning_summaries);
    assert_eq!(model_info.supported_reasoning_levels.len(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundled_model_info_takes_precedence_over_builtin_fallback() {
    let adam_home = TempDir::new().expect("create temp dir");
    let mut config = load_default_config_for_test(&adam_home).await;
    config.features.enable(Feature::RemoteModels);

    let auth_manager = adam_agent::auth::AuthManager::from_auth_for_testing(
        CodexAuth::from_api_key("Test API Key"),
    );
    let models_manager = ModelsManager::with_provider(
        adam_home.path().to_path_buf(),
        auth_manager,
        "openai",
        built_in_runtime_endpoints()["openai"].clone(),
    );

    let model_info = models_manager.get_model_info("gpt-5.1", &config).await;

    assert_eq!(
        model_info.description.as_deref(),
        Some("Broad world knowledge with strong general reasoning.")
    );
}
