use crate::product::agent::features::Feature;
use crate::product::app_server_protocol::ItemCompletedNotification;
use crate::product::app_server_protocol::ItemStartedNotification;
use crate::product::app_server_protocol::JSONRPCMessage;
use crate::product::app_server_protocol::JSONRPCResponse;
use crate::product::app_server_protocol::PlanDeltaNotification;
use crate::product::app_server_protocol::RequestId;
use crate::product::app_server_protocol::ThreadItem;
use crate::product::app_server_protocol::ThreadStartParams;
use crate::product::app_server_protocol::ThreadStartResponse;
use crate::product::app_server_protocol::TurnCompletedNotification;
use crate::product::app_server_protocol::TurnStartParams;
use crate::product::app_server_protocol::TurnStartResponse;
use crate::product::app_server_protocol::TurnStatus;
use crate::product::app_server_protocol::UserInput as V2UserInput;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::Settings;
use crate::test_support::app_server::McpProcess;
use crate::test_support::app_server::create_mock_responses_server_sequence;
use crate::test_support::app_server::to_response;
use crate::test_support::core::responses;
use crate::test_support::core::skip_if_no_network;
use anyhow::Result;
use anyhow::anyhow;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn plan_mode_uses_proposed_plan_block_for_plan_item() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let plan_block = "<proposed_plan>\n# Final plan\n- first\n- second\n</proposed_plan>\n";
    let full_message = format!("Preface\n{plan_block}Postscript");
    let responses = vec![responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_message_item_added("msg-1", ""),
        responses::ev_output_text_delta(&full_message),
        responses::ev_assistant_message("msg-1", &full_message),
        responses::ev_completed("resp-1"),
    ])];
    let server = create_mock_responses_server_sequence(responses).await;

    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let turn = start_plan_mode_turn(&mut mcp).await?;
    let (_, completed_items, plan_deltas, turn_completed) =
        collect_turn_notifications(&mut mcp).await?;

    assert_eq!(turn_completed.turn.id, turn.id);
    assert_eq!(turn_completed.turn.status, TurnStatus::Completed);

    let expected_plan = ThreadItem::Plan {
        id: format!("{}-plan", turn.id),
        text: "# Final plan\n- first\n- second\n".to_string(),
    };
    let expected_plan_id = format!("{}-plan", turn.id);
    let streamed_plan = plan_deltas
        .iter()
        .map(|delta| delta.delta.as_str())
        .collect::<String>();
    assert_eq!(streamed_plan, "# Final plan\n- first\n- second\n");
    assert!(
        plan_deltas
            .iter()
            .all(|delta| delta.item_id == expected_plan_id)
    );
    let plan_items = completed_items
        .iter()
        .filter_map(|item| match item {
            ThreadItem::Plan { .. } => Some(item.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(plan_items, vec![expected_plan]);
    assert!(
        completed_items
            .iter()
            .any(|item| matches!(item, ThreadItem::AgentMessage { .. })),
        "agent message items should still be emitted alongside the plan item"
    );

    Ok(())
}

#[tokio::test]
async fn plan_mode_without_proposed_plan_does_not_emit_plan_item() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let responses = vec![responses::sse(vec![
        responses::ev_response_created("resp-1"),
        responses::ev_assistant_message("msg-1", "Done"),
        responses::ev_completed("resp-1"),
    ])];
    let server = create_mock_responses_server_sequence(responses).await;

    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let _turn = start_plan_mode_turn(&mut mcp).await?;
    let (_, completed_items, plan_deltas, _) = collect_turn_notifications(&mut mcp).await?;

    let has_plan_item = completed_items
        .iter()
        .any(|item| matches!(item, ThreadItem::Plan { .. }));
    assert!(!has_plan_item);
    assert!(plan_deltas.is_empty());

    Ok(())
}

async fn start_plan_mode_turn(
    mcp: &mut McpProcess,
) -> Result<crate::product::app_server_protocol::Turn> {
    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let thread = to_response::<ThreadStartResponse>(thread_resp)?.thread;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: "mock-model".to_string(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };
    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            input: vec![V2UserInput::Text {
                text: "Plan this".to_string(),
                text_elements: Vec::new(),
            }],
            identity: Some(identity),
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    Ok(to_response::<TurnStartResponse>(turn_resp)?.turn)
}

async fn collect_turn_notifications(
    mcp: &mut McpProcess,
) -> Result<(
    Vec<ThreadItem>,
    Vec<ThreadItem>,
    Vec<PlanDeltaNotification>,
    TurnCompletedNotification,
)> {
    let mut started_items = Vec::new();
    let mut completed_items = Vec::new();
    let mut plan_deltas = Vec::new();

    loop {
        let message = timeout(DEFAULT_READ_TIMEOUT, mcp.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        match notification.method.as_str() {
            "item/started" => {
                let params = notification
                    .params
                    .ok_or_else(|| anyhow!("item/started notifications must include params"))?;
                let payload: ItemStartedNotification = serde_json::from_value(params)?;
                started_items.push(payload.item);
            }
            "item/completed" => {
                let params = notification
                    .params
                    .ok_or_else(|| anyhow!("item/completed notifications must include params"))?;
                let payload: ItemCompletedNotification = serde_json::from_value(params)?;
                completed_items.push(payload.item);
            }
            "item/plan/delta" => {
                let params = notification
                    .params
                    .ok_or_else(|| anyhow!("item/plan/delta notifications must include params"))?;
                let payload: PlanDeltaNotification = serde_json::from_value(params)?;
                plan_deltas.push(payload);
            }
            "turn/completed" => {
                let params = notification
                    .params
                    .ok_or_else(|| anyhow!("turn/completed notifications must include params"))?;
                let payload: TurnCompletedNotification = serde_json::from_value(params)?;
                return Ok((started_items, completed_items, plan_deltas, payload));
            }
            _ => {}
        }
    }
}

fn create_config_toml(lha_home: &Path, server_uri: &str) -> std::io::Result<()> {
    let features = BTreeMap::from([(Feature::RemoteModels, false), (Feature::Identities, true)]);
    crate::test_support::app_server::write_mock_responses_config_toml_with_options(
        lha_home,
        server_uri,
        &features,
        20_000,
        Some(false),
        "mock_provider",
        "mock-model",
        "",
        "never",
        "read-only",
    )
}
