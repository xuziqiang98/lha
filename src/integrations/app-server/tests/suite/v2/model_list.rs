use std::time::Duration;

use anyhow::Result;
use anyhow::anyhow;
use app_test_support::ChatGptAuthFixture;
use app_test_support::McpProcess;
use app_test_support::to_response;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_mock_responses_config_toml;
use app_test_support::write_mock_responses_config_toml_with_options;
use app_test_support::write_models_cache;
use codex_agent::auth::AuthCredentialsStoreMode;
use codex_agent::features::Feature;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::Model;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::ReasoningEffortOption;
use codex_app_server_protocol::RequestId;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const OFFICIAL_OPENAI_PROVIDER_DESCRIPTION: &str = "Official model from OpenAI provider.";

fn model_from_preset(preset: &ModelPreset) -> Model {
    Model {
        id: preset.id.clone(),
        model: preset.model.clone(),
        display_name: preset.display_name.clone(),
        description: preset.description.clone(),
        supported_reasoning_efforts: preset
            .supported_reasoning_efforts
            .iter()
            .map(|preset| ReasoningEffortOption {
                reasoning_effort: preset.effort,
                description: preset.description.clone(),
            })
            .collect(),
        default_reasoning_effort: preset.default_reasoning_effort,
        supports_personality: preset.supports_personality,
        is_default: preset.is_default,
    }
}

fn expected_visible_models() -> Vec<Model> {
    let response: ModelsResponse = serde_json::from_str(include_str!(
        "../../../../../coding-agent/runtime/models.json"
    ))
    .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"));
    let mut presets: Vec<ModelPreset> = response.models.into_iter().map(Into::into).collect();

    for preset in &mut presets {
        preset.is_default = false;
        if preset.show_in_picker && preset.id == preset.model {
            preset.description = OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string();
        }
    }
    if let Some(default) = presets.iter_mut().find(|preset| preset.show_in_picker) {
        default.is_default = true;
    }

    presets
        .into_iter()
        .filter(|preset| preset.show_in_picker)
        .map(|preset| model_from_preset(&preset))
        .collect()
}

#[tokio::test]
async fn list_models_returns_all_models_with_large_limit() -> Result<()> {
    let adam_home = TempDir::new()?;
    write_models_cache(adam_home.path())?;
    let mut mcp = McpProcess::new(adam_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            limit: Some(100),
            cursor: None,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let ModelListResponse {
        data: items,
        next_cursor,
    } = to_response::<ModelListResponse>(response)?;

    let expected_models = expected_visible_models();

    assert_eq!(items, expected_models);
    assert!(next_cursor.is_none());
    Ok(())
}

#[tokio::test]
async fn list_models_pagination_works() -> Result<()> {
    let adam_home = TempDir::new()?;
    write_models_cache(adam_home.path())?;
    let mut mcp = McpProcess::new(adam_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let expected_models = expected_visible_models();
    let mut cursor = None;
    let mut items = Vec::new();

    for _ in 0..expected_models.len() {
        let request_id = mcp
            .send_list_models_request(ModelListParams {
                limit: Some(1),
                cursor: cursor.clone(),
            })
            .await?;

        let response: JSONRPCResponse = timeout(
            DEFAULT_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
        )
        .await??;

        let ModelListResponse {
            data: page_items,
            next_cursor,
        } = to_response::<ModelListResponse>(response)?;

        assert_eq!(page_items.len(), 1);
        items.extend(page_items);

        if let Some(next_cursor) = next_cursor {
            cursor = Some(next_cursor);
        } else {
            assert_eq!(items, expected_models);
            return Ok(());
        }
    }

    return Err(anyhow!(
        "model pagination did not terminate after {} pages",
        expected_visible_models().len()
    ));
}

#[tokio::test]
async fn list_models_rejects_invalid_cursor() -> Result<()> {
    let adam_home = TempDir::new()?;
    write_models_cache(adam_home.path())?;
    let mut mcp = McpProcess::new(adam_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            limit: None,
            cursor: Some("invalid".to_string()),
        })
        .await?;

    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.id, RequestId::Integer(request_id));
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "invalid cursor: invalid");
    Ok(())
}

#[tokio::test]
async fn list_models_without_auth_appends_configured_custom_model() -> Result<()> {
    let adam_home = TempDir::new()?;
    write_models_cache(adam_home.path())?;
    write_mock_responses_config_toml(
        adam_home.path(),
        "http://unused.test",
        &BTreeMap::from([(Feature::RemoteModels, false)]),
        10_000,
        Some(false),
        "mock_provider",
        "compact",
    )?;
    let mut mcp = McpProcess::new(adam_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            limit: Some(100),
            cursor: None,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let ModelListResponse {
        data: items,
        next_cursor,
    } = to_response::<ModelListResponse>(response)?;

    assert!(items.iter().any(|model| model.id == "gpt-5.3-codex"));
    assert_eq!(
        items.last(),
        Some(&Model {
            id: "_provider.mock_provider.mock-model".to_string(),
            model: "mock-model".to_string(),
            display_name: "mock-model".to_string(),
            description: "User-defined model from mock_provider provider.".to_string(),
            supported_reasoning_efforts: vec![],
            default_reasoning_effort: ReasoningEffort::None,
            supports_personality: false,
            is_default: false,
        })
    );
    assert!(next_cursor.is_none());
    Ok(())
}

#[tokio::test]
async fn list_models_with_auth_appends_configured_custom_model() -> Result<()> {
    let adam_home = TempDir::new()?;
    write_models_cache(adam_home.path())?;
    write_mock_responses_config_toml(
        adam_home.path(),
        "http://unused.test",
        &BTreeMap::from([(Feature::RemoteModels, false)]),
        10_000,
        Some(false),
        "mock_provider",
        "compact",
    )?;
    write_chatgpt_auth(
        adam_home.path(),
        ChatGptAuthFixture::new("chatgpt-access"),
        AuthCredentialsStoreMode::File,
    )?;
    let mut mcp = McpProcess::new(adam_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            limit: Some(100),
            cursor: None,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let ModelListResponse { data: items, .. } = to_response::<ModelListResponse>(response)?;
    let expected_models = expected_visible_models();

    assert_eq!(items.len(), expected_models.len() + 1);
    assert_eq!(
        items.first().map(|model| model.model.as_str()),
        expected_models.first().map(|model| model.model.as_str())
    );
    assert_eq!(
        items.last().map(|model| model.model.as_str()),
        Some("mock-model")
    );
    assert_eq!(
        items.last().map(|model| model.description.as_str()),
        Some("User-defined model from mock_provider provider.")
    );
    Ok(())
}

#[tokio::test]
async fn list_models_with_auth_keeps_same_slug_custom_provider_entry() -> Result<()> {
    let adam_home = TempDir::new()?;
    write_models_cache(adam_home.path())?;
    write_mock_responses_config_toml_with_options(
        adam_home.path(),
        "https://example.test/a",
        &BTreeMap::new(),
        20_000,
        Some(false),
        "provider_a",
        "gpt-5.2",
        "",
        "never",
        "read-only",
    )?;
    write_chatgpt_auth(
        adam_home.path(),
        ChatGptAuthFixture::new("chatgpt-access"),
        AuthCredentialsStoreMode::File,
    )?;
    let mut mcp = McpProcess::new(adam_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_list_models_request(ModelListParams {
            limit: Some(100),
            cursor: None,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let ModelListResponse { data: items, .. } = to_response::<ModelListResponse>(response)?;

    let gpt_5_2_models = items
        .iter()
        .filter(|model| model.model == "gpt-5.2")
        .map(|model| (model.id.clone(), model.description.clone()))
        .collect::<Vec<_>>();

    assert_eq!(gpt_5_2_models.len(), 2);
    assert_eq!(items.len(), expected_visible_models().len() + 1);
    assert_eq!(
        gpt_5_2_models,
        vec![
            (
                "gpt-5.2".to_string(),
                OFFICIAL_OPENAI_PROVIDER_DESCRIPTION.to_string(),
            ),
            (
                "_provider.provider_a.gpt-5.2".to_string(),
                "User-defined model from provider_a provider.".to_string(),
            ),
        ]
    );
    Ok(())
}
