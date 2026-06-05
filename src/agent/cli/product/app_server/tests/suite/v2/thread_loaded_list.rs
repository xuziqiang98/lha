use crate::product::app_server_protocol::JSONRPCResponse;
use crate::product::app_server_protocol::RequestId;
use crate::product::app_server_protocol::ThreadLoadedListParams;
use crate::product::app_server_protocol::ThreadLoadedListResponse;
use crate::product::app_server_protocol::ThreadStartParams;
use crate::product::app_server_protocol::ThreadStartResponse;
use crate::test_support::app_server::McpProcess;
use crate::test_support::app_server::create_mock_responses_server_repeating_assistant;
use crate::test_support::app_server::to_response;
use anyhow::Result;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_loaded_list_returns_loaded_thread_ids() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_id = start_thread(&mut mcp).await?;

    let list_id = mcp
        .send_thread_loaded_list_request(ThreadLoadedListParams::default())
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_id)),
    )
    .await??;
    let ThreadLoadedListResponse {
        mut data,
        next_cursor,
    } = to_response::<ThreadLoadedListResponse>(resp)?;
    data.sort();
    assert_eq!(data, vec![thread_id]);
    assert_eq!(next_cursor, None);

    Ok(())
}

#[tokio::test]
async fn thread_loaded_list_paginates() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let first = start_thread(&mut mcp).await?;
    let second = start_thread(&mut mcp).await?;

    let mut expected = [first, second];
    expected.sort();

    let list_id = mcp
        .send_thread_loaded_list_request(ThreadLoadedListParams {
            cursor: None,
            limit: Some(1),
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_id)),
    )
    .await??;
    let ThreadLoadedListResponse {
        data: first_page,
        next_cursor,
    } = to_response::<ThreadLoadedListResponse>(resp)?;
    assert_eq!(first_page, vec![expected[0].clone()]);
    assert_eq!(next_cursor, Some(expected[0].clone()));

    let list_id = mcp
        .send_thread_loaded_list_request(ThreadLoadedListParams {
            cursor: next_cursor,
            limit: Some(1),
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(list_id)),
    )
    .await??;
    let ThreadLoadedListResponse {
        data: second_page,
        next_cursor,
    } = to_response::<ThreadLoadedListResponse>(resp)?;
    assert_eq!(second_page, vec![expected[1].clone()]);
    assert_eq!(next_cursor, None);

    Ok(())
}

fn create_config_toml(lha_home: &Path, server_uri: &str) -> std::io::Result<()> {
    crate::test_support::app_server::write_mock_responses_config_toml_with_options(
        lha_home,
        server_uri,
        &std::collections::BTreeMap::new(),
        20_000,
        Some(false),
        "mock_provider",
        crate::test_support::app_server::MockResponsesConfigTomlOptions {
            model: "mock-model",
            compact_prompt: "",
            approval_policy: "never",
            sandbox_mode: "read-only",
        },
    )
}

async fn start_thread(mcp: &mut McpProcess) -> Result<String> {
    let req_id = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(req_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(resp)?;
    Ok(thread.id)
}
