#![cfg(unix)]
// Support code lives in the `app_test_support` crate under tests/common.

use std::path::Path;

use adam_agent::protocol::TurnAbortReason;
use adam_app_server_protocol::AddConversationListenerParams;
use adam_app_server_protocol::InterruptConversationParams;
use adam_app_server_protocol::InterruptConversationResponse;
use adam_app_server_protocol::JSONRPCResponse;
use adam_app_server_protocol::NewConversationParams;
use adam_app_server_protocol::NewConversationResponse;
use adam_app_server_protocol::RequestId;
use adam_app_server_protocol::SendUserMessageParams;
use adam_app_server_protocol::SendUserMessageResponse;
use core_test_support::skip_if_no_network;
use tempfile::TempDir;
use tokio::time::timeout;

use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_shell_command_sse_response;
use app_test_support::to_response;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shell_command_interruption() {
    skip_if_no_network!();

    if let Err(err) = shell_command_interruption().await {
        panic!("failure: {err}");
    }
}

async fn shell_command_interruption() -> anyhow::Result<()> {
    // Use a cross-platform blocking command. On Windows plain `sleep` is not guaranteed to exist
    // (MSYS/GNU coreutils may be absent) and the failure causes the tool call to finish immediately,
    // which triggers a second model request before the test sends the explicit follow-up. That
    // prematurely consumes the second mocked SSE response and leads to a third POST (panic: no response for 2).
    // Powershell Start-Sleep is always available on Windows runners. On Unix we keep using `sleep`.
    #[cfg(target_os = "windows")]
    let shell_command = vec![
        "powershell".to_string(),
        "-Command".to_string(),
        "Start-Sleep -Seconds 10".to_string(),
    ];
    #[cfg(not(target_os = "windows"))]
    let shell_command = vec!["sleep".to_string(), "10".to_string()];

    let tmp = TempDir::new()?;
    // Temporary Adam home with config pointing at the mock server.
    let adam_home = tmp.path().join("adam_home");
    std::fs::create_dir(&adam_home)?;
    let working_directory = tmp.path().join("workdir");
    std::fs::create_dir(&working_directory)?;

    // Create mock server with a single SSE response: the long sleep command
    let server = create_mock_responses_server_sequence(vec![create_shell_command_sse_response(
        shell_command.clone(),
        Some(&working_directory),
        Some(10_000), // 10 seconds timeout in ms
        "call_sleep",
    )?])
    .await;
    create_config_toml(&adam_home, server.uri())?;

    // Start MCP server and initialize.
    let mut mcp = McpProcess::new(&adam_home).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // 1) newConversation
    let new_conv_id = mcp
        .send_new_conversation_request(NewConversationParams {
            cwd: Some(working_directory.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let new_conv_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(new_conv_id)),
    )
    .await??;
    let new_conv_resp = to_response::<NewConversationResponse>(new_conv_resp)?;
    let NewConversationResponse {
        conversation_id, ..
    } = new_conv_resp;

    // 2) addConversationListener
    let add_listener_id = mcp
        .send_add_conversation_listener_request(AddConversationListenerParams { conversation_id })
        .await?;
    let _add_listener_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(add_listener_id)),
    )
    .await??;

    // 3) sendUserMessage (should trigger notifications; we only validate an OK response)
    let send_user_id = mcp
        .send_send_user_message_request(SendUserMessageParams {
            conversation_id,
            items: vec![adam_app_server_protocol::InputItem::Text {
                text: "run first sleep command".to_string(),
                text_elements: Vec::new(),
            }],
        })
        .await?;
    let send_user_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(send_user_id)),
    )
    .await??;
    let SendUserMessageResponse {} = to_response::<SendUserMessageResponse>(send_user_resp)?;

    // Give the command a moment to start
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // 4) send interrupt request
    let interrupt_id = mcp
        .send_interrupt_conversation_request(InterruptConversationParams { conversation_id })
        .await?;
    let interrupt_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(interrupt_id)),
    )
    .await??;
    let InterruptConversationResponse { abort_reason } =
        to_response::<InterruptConversationResponse>(interrupt_resp)?;
    assert_eq!(TurnAbortReason::Interrupted, abort_reason);

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn create_config_toml(adam_home: &Path, server_uri: String) -> std::io::Result<()> {
    app_test_support::write_mock_responses_config_toml_with_options(
        adam_home,
        &server_uri,
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
