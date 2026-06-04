use crate::product::app_server_protocol::JSONRPCNotification;
use crate::product::app_server_protocol::JSONRPCResponse;
use crate::product::app_server_protocol::RawTranscriptItemCompletedNotification;
use crate::product::app_server_protocol::RequestId;
use crate::product::app_server_protocol::Thread;
use crate::product::app_server_protocol::ThreadStartParams;
use crate::product::app_server_protocol::ThreadStartResponse;
use crate::product::app_server_protocol::TurnStartParams;
use crate::product::app_server_protocol::TurnStartResponse;
use crate::product::app_server_protocol::UserInput as V2UserInput;
use crate::product::protocol::models::ContentItem;
use crate::product::protocol::models::TranscriptItem;
use crate::test_support::app_server::McpProcess;
use crate::test_support::app_server::to_response;
use crate::test_support::core::responses;
use anyhow::Context;
use anyhow::Result;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_start_emits_raw_transcript_notifications_when_opted_in() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let _response_mock = responses::mount_sse_once(&server, body).await;

    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread = start_thread(&mut mcp, true).await?;
    let turn = start_turn(&mut mcp, &thread.id).await?;

    let mut saw_user_message = false;
    let mut saw_assistant_message = false;
    for _ in 0..8 {
        let raw_item = read_raw_transcript_item(&mut mcp).await?;
        assert_eq!(raw_item.thread_id, thread.id);
        assert!(!raw_item.turn_id.is_empty());
        if raw_item.turn_id == turn.turn.id {
            saw_user_message |= item_contains_text(&raw_item.item, "Hello");
            saw_assistant_message |= item_contains_text(&raw_item.item, "Done");
        }
        if saw_user_message && saw_assistant_message {
            return Ok(());
        }
    }

    panic!("expected v2 raw transcript notifications for user and assistant messages");
}

#[tokio::test]
async fn thread_start_does_not_emit_raw_transcript_notifications_by_default() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ]);
    let _response_mock = responses::mount_sse_once(&server, body).await;

    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread = start_thread(&mut mcp, false).await?;
    let _turn = start_turn(&mut mcp, &thread.id).await?;

    let raw_attempt = timeout(
        std::time::Duration::from_millis(200),
        mcp.read_stream_until_notification_message("rawTranscriptItem/completed"),
    )
    .await;
    assert!(
        raw_attempt.is_err(),
        "unexpected v2 raw transcript notification without opt-in"
    );
    Ok(())
}

async fn start_thread(mcp: &mut McpProcess, experimental_raw_events: bool) -> Result<Thread> {
    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
            developer_instructions: Some("Use the test harness tools.".to_string()),
            experimental_raw_events,
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;
    Ok(thread)
}

async fn start_turn(mcp: &mut McpProcess, thread_id: &str) -> Result<TurnStartResponse> {
    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.to_string(),
            input: vec![V2UserInput::Text {
                text: "Hello".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    to_response::<TurnStartResponse>(turn_resp)
}

async fn read_raw_transcript_item(
    mcp: &mut McpProcess,
) -> Result<RawTranscriptItemCompletedNotification> {
    let notification: JSONRPCNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("rawTranscriptItem/completed"),
    )
    .await??;
    let params = notification
        .params
        .context("rawTranscriptItem/completed params")?;
    Ok(serde_json::from_value(params)?)
}

fn item_contains_text(item: &TranscriptItem, expected_text: &str) -> bool {
    let TranscriptItem::Message { content, .. } = item else {
        return false;
    };
    content.iter().any(|item| match item {
        ContentItem::InputText { text, .. } | ContentItem::OutputText { text } => {
            text == expected_text
        }
        _ => false,
    })
}

fn create_config_toml(lha_home: &Path, server_uri: &str) -> std::io::Result<()> {
    crate::test_support::app_server::write_mock_responses_config_toml_with_options(
        lha_home,
        server_uri,
        &BTreeMap::new(),
        20_000,
        Some(false),
        "mock_provider",
        "mock-model",
        "",
        "never",
        "read-only",
    )
}
