use adam_app_server_protocol::AddConversationListenerParams;
use adam_app_server_protocol::AddConversationSubscriptionResponse;
use adam_app_server_protocol::InputItem;
use adam_app_server_protocol::JSONRPCNotification;
use adam_app_server_protocol::JSONRPCResponse;
use adam_app_server_protocol::NewConversationParams;
use adam_app_server_protocol::NewConversationResponse;
use adam_app_server_protocol::RequestId;
use adam_app_server_protocol::SendUserMessageParams;
use adam_app_server_protocol::SendUserMessageResponse;
use adam_protocol::ThreadId;
use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn test_send_message_success() -> Result<()> {
    // Spin up a mock responses server that immediately ends the Adam turn.
    // Two Adam turns hit the mock model (session start + send-user-message). Provide two SSE responses.
    let server = responses::start_mock_server().await;
    let body1 = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let body2 = responses::sse(vec![
        responses::ev_response_created("resp-2"),
        responses::ev_assistant_message("msg-2", "Done"),
        responses::ev_completed("resp-2"),
    ]);
    let _response_mock1 = responses::mount_sse_once(&server, body1).await;
    let _response_mock2 = responses::mount_sse_once(&server, body2).await;

    // Create a temporary Adam home with config pointing at the mock server.
    let adam_home = TempDir::new()?;
    create_config_toml(adam_home.path(), &server.uri())?;

    // Start MCP server process and initialize.
    let mut mcp = McpProcess::new(adam_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a conversation using the new wire API.
    let new_conv_id = mcp
        .send_new_conversation_request(NewConversationParams {
            ..Default::default()
        })
        .await?;
    let new_conv_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(new_conv_id)),
    )
    .await??;
    let NewConversationResponse {
        conversation_id, ..
    } = to_response::<_>(new_conv_resp)?;

    // 2) addConversationListener
    let add_listener_id = mcp
        .send_add_conversation_listener_request(AddConversationListenerParams { conversation_id })
        .await?;
    let add_listener_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(add_listener_id)),
    )
    .await??;
    let AddConversationSubscriptionResponse { subscription_id: _ } =
        to_response::<_>(add_listener_resp)?;

    // Now exercise sendUserMessage twice.
    send_message("Hello", conversation_id, &mut mcp).await?;
    send_message("Hello again", conversation_id, &mut mcp).await?;
    Ok(())
}

#[expect(clippy::expect_used)]
async fn send_message(
    message: &str,
    conversation_id: ThreadId,
    mcp: &mut McpProcess,
) -> Result<()> {
    // Now exercise sendUserMessage.
    let send_id = mcp
        .send_send_user_message_request(SendUserMessageParams {
            conversation_id,
            items: vec![InputItem::Text {
                text: message.to_string(),
                text_elements: Vec::new(),
            }],
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(send_id)),
    )
    .await??;

    let _ok: SendUserMessageResponse = to_response::<SendUserMessageResponse>(response)?;

    // Verify the task_finished notification is received.
    // Note this also ensures that the final request to the server was made.
    let task_finished_notification: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("adam/event/task_complete"),
    )
    .await??;
    let serde_json::Value::Object(map) = task_finished_notification
        .params
        .expect("notification should have params")
    else {
        panic!("task_finished_notification should have params");
    };
    assert_eq!(
        map.get("conversationId")
            .expect("should have conversationId"),
        &serde_json::Value::String(conversation_id.to_string())
    );

    let raw_attempt = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        mcp.read_stream_until_notification_message("adam/event/raw_transcript_item"),
    )
    .await;
    assert!(
        raw_attempt.is_err(),
        "unexpected raw item notification when not opted in"
    );
    Ok(())
}

#[tokio::test]
async fn test_send_message_session_not_found() -> Result<()> {
    // Start MCP without creating a Adam session
    let adam_home = TempDir::new()?;
    let mut mcp = McpProcess::new(adam_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let unknown = ThreadId::new();
    let req_id = mcp
        .send_send_user_message_request(SendUserMessageParams {
            conversation_id: unknown,
            items: vec![InputItem::Text {
                text: "ping".to_string(),
                text_elements: Vec::new(),
            }],
        })
        .await?;

    // Expect an error response for unknown conversation.
    let err = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(req_id)),
    )
    .await??;
    assert_eq!(err.id, RequestId::Integer(req_id));
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn create_config_toml(adam_home: &Path, server_uri: &str) -> std::io::Result<()> {
    app_test_support::write_mock_responses_config_toml_with_options(
        adam_home,
        server_uri,
        &std::collections::BTreeMap::new(),
        20_000,
        Some(false),
        "mock_provider",
        "mock-model",
        "",
        "never",
        "danger-full-access",
    )
}
