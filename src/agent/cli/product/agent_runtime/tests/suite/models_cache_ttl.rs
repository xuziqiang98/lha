use std::path::Path;
use std::sync::Arc;

use crate::product::agent::CodexAuth;
use crate::product::agent::config::model_provider_cache_key;
use crate::product::agent::features::Feature;
use crate::product::agent::models_manager::manager::RefreshStrategy;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::Op;
use crate::product::agent::protocol::SandboxPolicy;
use crate::product::protocol::config_types::ReasoningSummary;
use crate::product::protocol::openai_models::ConfigShellToolType;
use crate::product::protocol::openai_models::ModelInfo;
use crate::product::protocol::openai_models::ModelVisibility;
use crate::product::protocol::openai_models::ModelsResponse;
use crate::product::protocol::openai_models::ReasoningEffort;
use crate::product::protocol::openai_models::ReasoningEffortPreset;
use crate::product::protocol::openai_models::TruncationPolicyConfig;
use crate::product::protocol::user_input::UserInput;
use crate::test_support::core::responses;
use crate::test_support::core::responses::ev_assistant_message;
use crate::test_support::core::responses::ev_completed;
use crate::test_support::core::responses::ev_response_created;
use crate::test_support::core::responses::sse;
use crate::test_support::core::responses::sse_response;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event;
use anyhow::Result;
use chrono::DateTime;
use chrono::TimeZone;
use chrono::Utc;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde::Serialize;
use wiremock::MockServer;

const ETAG: &str = "\"models-etag-ttl\"";
const CACHE_FILE: &str = "models_cache.json";
const REMOTE_MODEL: &str = "codex-test-ttl";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renews_cache_ttl_on_matching_models_etag() -> Result<()> {
    crate::test_support::core::skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;

    let remote_model = test_remote_model(REMOTE_MODEL, 1);
    let models_mock = responses::mount_models_once_with_etag(
        &server,
        ModelsResponse {
            models: vec![remote_model.clone()],
        },
        ETAG,
    )
    .await;

    let mut builder = test_codex().with_auth(CodexAuth::from_api_key("Test API Key"));
    builder = builder.with_config(|config| {
        config.features.enable(Feature::RemoteModels);
        config.model = Some("gpt-5".to_string());
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(1);
    });

    let test = builder.build(&server).await?;
    let codex = Arc::clone(&test.codex);
    let config = test.config.clone();

    // Populate cache via initial refresh.
    let models_manager = test.thread_manager.get_models_manager();
    let _ = models_manager
        .list_models(&config, RefreshStrategy::OnlineIfUncached)
        .await;

    let cache_path = cache_path_for_provider(&config.lha_home, &config.model_provider_id);
    let stale_time = Utc.timestamp_opt(0, 0).single().expect("valid epoch");
    rewrite_cache_timestamp(&cache_path, stale_time).await?;

    // Trigger responses with matching ETag, which should renew the cache TTL without another /models.
    let response_body = sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-1"),
    ]);
    let _responses_mock = responses::mount_response_once(
        &server,
        sse_response(response_body).insert_header("X-Models-Etag", ETAG),
    )
    .await;

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hi".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let _ = wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let refreshed_cache = read_cache(&cache_path).await?;
    assert!(
        refreshed_cache.fetched_at > stale_time,
        "cache TTL should be renewed"
    );
    assert_eq!(
        models_mock.requests().len(),
        1,
        "/models should not refetch on matching etag"
    );

    // Cached models remain usable offline.
    let offline_models = test
        .thread_manager
        .list_models(&config, RefreshStrategy::Offline)
        .await;
    assert!(
        offline_models
            .iter()
            .any(|preset| preset.model == REMOTE_MODEL),
        "offline listing should use renewed cache"
    );

    Ok(())
}

async fn rewrite_cache_timestamp(path: &Path, fetched_at: DateTime<Utc>) -> Result<()> {
    let mut cache = read_cache(path).await?;
    cache.fetched_at = fetched_at;
    let contents = serde_json::to_vec_pretty(&cache)?;
    tokio::fs::write(path, contents).await?;
    Ok(())
}

async fn read_cache(path: &Path) -> Result<ModelsCache> {
    let contents = tokio::fs::read(path).await?;
    let cache = serde_json::from_slice(&contents)?;
    Ok(cache)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelsCache {
    fetched_at: DateTime<Utc>,
    #[serde(default)]
    etag: Option<String>,
    models: Vec<ModelInfo>,
}

fn cache_path_for_provider(lha_home: &Path, model_provider_id: &str) -> std::path::PathBuf {
    lha_home
        .join("remote_models")
        .join(model_provider_cache_key(model_provider_id))
        .join(CACHE_FILE)
}

fn test_remote_model(slug: &str, priority: i32) -> ModelInfo {
    ModelInfo {
        slug: slug.to_string(),
        display_name: "Remote Test".to_string(),
        description: Some("remote model".to_string()),
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffort::Low,
                description: "low".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Medium,
                description: "medium".to_string(),
            },
        ],
        shell_type: ConfigShellToolType::ShellCommand,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority,
        upgrade: None,
        base_instructions: "base instructions".to_string(),
        model_messages: None,
        supports_reasoning_summaries: false,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        truncation_policy: TruncationPolicyConfig::bytes(10_000),
        supports_parallel_tool_calls: false,
        context_window: Some(272_000),
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
    }
}
