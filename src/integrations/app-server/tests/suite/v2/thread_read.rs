use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_fake_rollout_with_schema_version;
use app_test_support::create_fake_rollout_with_text_elements;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use lha_app_server_protocol::JSONRPCError;
use lha_app_server_protocol::JSONRPCResponse;
use lha_app_server_protocol::RequestId;
use lha_app_server_protocol::SessionSource;
use lha_app_server_protocol::ThreadItem;
use lha_app_server_protocol::ThreadReadParams;
use lha_app_server_protocol::ThreadReadResponse;
use lha_app_server_protocol::TurnStatus;
use lha_app_server_protocol::UserInput;
use lha_protocol::user_input::ByteRange;
use lha_protocol::user_input::TextElement;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_read_returns_summary_without_turns() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let preview = "Saved user message";
    let text_elements = [TextElement::new(
        ByteRange { start: 0, end: 5 },
        Some("<note>".into()),
    )];
    let conversation_id = create_fake_rollout_with_text_elements(
        lha_home.path(),
        "2025-01-05T12-00-00",
        "2025-01-05T12:00:00Z",
        preview,
        text_elements
            .iter()
            .map(|elem| serde_json::to_value(elem).expect("serialize text element"))
            .collect(),
        Some("mock_provider"),
        None,
    )?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let read_id = mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: conversation_id.clone(),
            include_turns: false,
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let ThreadReadResponse { thread } = to_response::<ThreadReadResponse>(read_resp)?;

    assert_eq!(thread.id, conversation_id);
    assert_eq!(thread.preview, preview);
    assert_eq!(thread.model_provider, "mock_provider");
    assert!(thread.path.as_ref().expect("thread path").is_absolute());
    assert_eq!(thread.cwd, PathBuf::from("/"));
    assert_eq!(thread.cli_version, "0.0.0");
    assert_eq!(thread.source, SessionSource::Cli);
    assert_eq!(thread.git_info, None);
    assert_eq!(thread.turns.len(), 0);

    Ok(())
}

#[tokio::test]
async fn thread_read_can_include_turns() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let preview = "Saved user message";
    let text_elements = vec![TextElement::new(
        ByteRange { start: 0, end: 5 },
        Some("<note>".into()),
    )];
    let conversation_id = create_fake_rollout_with_text_elements(
        lha_home.path(),
        "2025-01-05T12-00-00",
        "2025-01-05T12:00:00Z",
        preview,
        text_elements
            .iter()
            .map(|elem| serde_json::to_value(elem).expect("serialize text element"))
            .collect(),
        Some("mock_provider"),
        None,
    )?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let read_id = mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: conversation_id.clone(),
            include_turns: true,
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let ThreadReadResponse { thread } = to_response::<ThreadReadResponse>(read_resp)?;

    assert_eq!(thread.turns.len(), 1);
    let turn = &thread.turns[0];
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(turn.items.len(), 1, "expected user message item");
    match &turn.items[0] {
        ThreadItem::UserMessage { content, .. } => {
            assert_eq!(
                content,
                &vec![UserInput::Text {
                    text: preview.to_string(),
                    text_elements: text_elements.clone().into_iter().map(Into::into).collect(),
                }]
            );
        }
        other => panic!("expected user message item, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn thread_read_rejects_v2_rollout() -> Result<()> {
    thread_read_rejects_unsupported_rollout(Some(2)).await
}

#[tokio::test]
async fn thread_read_rejects_missing_schema_version_rollout() -> Result<()> {
    thread_read_rejects_unsupported_rollout(None).await
}

async fn thread_read_rejects_unsupported_rollout(schema_version: Option<u32>) -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let conversation_id = create_fake_rollout_with_schema_version(
        lha_home.path(),
        "2025-01-06T12-00-00",
        "2025-01-06T12:00:00Z",
        "legacy message",
        Some("mock_provider"),
        schema_version,
    )?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let read_id = mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: conversation_id,
            include_turns: false,
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(read_id)),
    )
    .await??;

    assert!(
        error.error.message.contains("legacy rollout unsupported"),
        "unexpected error message: {}",
        error.error.message
    );

    Ok(())
}

// Helper to create a config.toml pointing at the mock model server.
fn create_config_toml(lha_home: &Path, server_uri: &str) -> std::io::Result<()> {
    app_test_support::write_mock_responses_config_toml_with_options(
        lha_home,
        server_uri,
        &std::collections::BTreeMap::new(),
        20_000,
        Some(false),
        "mock_provider",
        "mock-model",
        "",
        "never",
        "read-only",
    )
}
