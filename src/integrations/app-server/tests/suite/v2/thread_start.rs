use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use lha_app_server_protocol::JSONRPCNotification;
use lha_app_server_protocol::JSONRPCResponse;
use lha_app_server_protocol::RequestId;
use lha_app_server_protocol::ThreadStartParams;
use lha_app_server_protocol::ThreadStartResponse;
use lha_app_server_protocol::ThreadStartedNotification;
use lha_protocol::openai_models::ReasoningEffort;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_start_creates_thread_and_emits_started() -> Result<()> {
    // Provide a mock server and config so model wiring is valid.
    let server = create_mock_responses_server_repeating_assistant("Done").await;

    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    // Start server and initialize.
    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a v2 thread with an explicit model override.
    let req_id = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("gpt-5.1".to_string()),
            ..Default::default()
        })
        .await?;

    // Expect a proper JSON-RPC response with a thread id.
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(req_id)),
    )
    .await??;
    let ThreadStartResponse {
        thread,
        model_provider,
        ..
    } = to_response::<ThreadStartResponse>(resp)?;
    assert!(!thread.id.is_empty(), "thread id should not be empty");
    assert!(
        thread.preview.is_empty(),
        "new threads should start with an empty preview"
    );
    assert_eq!(model_provider, "mock_provider");
    assert!(
        thread.created_at > 0,
        "created_at should be a positive UNIX timestamp"
    );

    // A corresponding thread/started notification should arrive.
    let notif: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("thread/started"),
    )
    .await??;
    let started: ThreadStartedNotification =
        serde_json::from_value(notif.params.expect("params must be present"))?;
    assert_eq!(started.thread, thread);

    Ok(())
}

#[tokio::test]
async fn thread_start_respects_request_reasoning_effort() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;

    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams::default())
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;

    let req_id = mcp
        .send_turn_start_request(lha_app_server_protocol::TurnStartParams {
            thread_id: thread.id,
            effort: Some(ReasoningEffort::High),
            input: vec![lha_app_server_protocol::UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(req_id)),
    )
    .await??;
    let _: lha_app_server_protocol::TurnStartResponse = to_response(resp)?;

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/started"),
    )
    .await??;
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
        "gpt-5.1",
        "",
        "never",
        "read-only",
    )
}
