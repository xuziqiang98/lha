#![allow(clippy::unwrap_used)]

use crate::product::agent::features::Feature;
use crate::product::protocol::workflow::WorkflowDefinition;
use crate::product::protocol::workflow::WorkflowMode;
use crate::product::protocol::workflow::WorkflowStepDefinition;
use crate::test_support::core::responses;
use crate::test_support::core::responses::ResponsesRequest;
use crate::test_support::core::responses::ev_assistant_message;
use crate::test_support::core::responses::ev_completed;
use crate::test_support::core::responses::ev_function_call;
use crate::test_support::core::responses::ev_response_created;
use crate::test_support::core::responses::sse;
use crate::test_support::core::responses::start_mock_server;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn workflow_definition() -> WorkflowDefinition {
    WorkflowDefinition {
        id: "architect_v1".to_string(),
        identity_id: "architect".to_string(),
        mode: WorkflowMode::Sequential,
        steps: vec![
            WorkflowStepDefinition {
                id: "requirements".to_string(),
                label: "Requirements".to_string(),
                prompt: "Collect requirements.".to_string(),
                depends_on: Vec::new(),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "requirements": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": { "id": { "type": "string" } },
                                "required": ["id"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["requirements"],
                    "additionalProperties": false
                }),
                allowed_tools: Some(vec!["read_file".to_string()]),
                validators: Vec::new(),
            },
            WorkflowStepDefinition {
                id: "architecture".to_string(),
                label: "Architecture".to_string(),
                prompt: "Design architecture.".to_string(),
                depends_on: vec!["requirements".to_string()],
                output_schema: json!({
                    "type": "object",
                    "properties": { "components": { "type": "array" } },
                    "required": ["components"],
                    "additionalProperties": false
                }),
                allowed_tools: None,
                validators: Vec::new(),
            },
        ],
    }
}

fn call_output(req: &ResponsesRequest, call_id: &str) -> (Value, Option<bool>) {
    let (content, success) = match req.function_call_output_content_and_success(call_id) {
        Some(values) => values,
        None => panic!("function call output present"),
    };
    let content = match content {
        Some(content) => content,
        None => panic!("function call output content present"),
    };
    (serde_json::from_str(&content).unwrap(), success)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_tool_accepts_current_step_artifact_and_filters_tools() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Identities);
    });
    let test = builder.build(&server).await?;
    test.codex
        .set_workflow_for_testing(workflow_definition())
        .await
        .expect("valid workflow");

    let call_id = "workflow-call";
    let args = json!({
        "step_id": "requirements",
        "artifact": { "requirements": [{ "id": "r1" }] }
    })
    .to_string();
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "workflow_submit_artifact", &args),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let completion = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "accepted"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn("run workflow").await?;

    let requests = server
        .received_requests()
        .await
        .expect("mock server should capture requests");
    let first_body: Value = serde_json::from_slice(&requests[0].body)?;
    let tools = first_body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(tools.contains(&"workflow_submit_artifact"));
    assert!(!tools.contains(&"update_plan"));

    let (output, success) = call_output(&completion.single_request(), call_id);
    assert_eq!(success, Some(true));
    assert_eq!(output["status"], "accepted");
    assert_eq!(output["completed_step"], "requirements");
    assert_eq!(output["next_step"], "architecture");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_tool_rejects_skipped_step() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    test.codex
        .set_workflow_for_testing(workflow_definition())
        .await
        .expect("valid workflow");

    let call_id = "workflow-call";
    let args = json!({
        "step_id": "architecture",
        "artifact": { "components": [] }
    })
    .to_string();
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "workflow_submit_artifact", &args),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let completion = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "rejected"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn("skip ahead").await?;

    let (output, success) = call_output(&completion.single_request(), call_id);
    assert_eq!(success, Some(false));
    assert_eq!(output["status"], "rejected");
    assert_eq!(output["errors"][0]["code"], "step_not_current");
    Ok(())
}
