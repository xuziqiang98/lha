#![cfg(not(target_os = "windows"))]

use std::fs;

use crate::product::agent::features::Feature;
use crate::product::agent::protocol::AskForApproval;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::ItemCompletedEvent;
use crate::product::agent::protocol::Op;
use crate::product::agent::protocol::SandboxPolicy;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::ReasoningSummary;
use crate::product::protocol::config_types::Settings;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::plan_tool::StepStatus;
use crate::product::protocol::user_input::UserInput;
use crate::test_support::core::assert_regex_match;
use crate::test_support::core::responses;
use crate::test_support::core::responses::ResponsesRequest;
use crate::test_support::core::responses::ev_apply_patch_function_call;
use crate::test_support::core::responses::ev_assistant_message;
use crate::test_support::core::responses::ev_completed;
use crate::test_support::core::responses::ev_function_call;
use crate::test_support::core::responses::ev_local_shell_call;
use crate::test_support::core::responses::ev_response_created;
use crate::test_support::core::responses::sse;
use crate::test_support::core::responses::start_mock_server;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::TestCodex;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event;
use assert_matches::assert_matches;
use serde_json::Value;
use serde_json::json;

#[derive(Debug, PartialEq, Eq)]
enum RelevantTurnEvent {
    AgentMessage(String),
    PlanItem(String),
    PlanUpdate,
    Error,
    TurnFinalizing,
    TurnComplete,
}

fn call_output(req: &ResponsesRequest, call_id: &str) -> (String, Option<bool>) {
    let raw = req.function_call_output(call_id);
    assert_eq!(
        raw.get("call_id").and_then(Value::as_str),
        Some(call_id),
        "mismatched call_id in function_call_output"
    );
    let (content_opt, success) = match req.function_call_output_content_and_success(call_id) {
        Some(values) => values,
        None => panic!("function_call_output present"),
    };
    let content = match content_opt {
        Some(c) => c,
        None => panic!("function_call_output content present"),
    };
    (content, success)
}

async fn submit_test_turn(
    test: &TestCodex,
    text: &str,
    identity: Option<Identity>,
) -> anyhow::Result<()> {
    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity,
            personality: None,
            tui_buddy: None,
        })
        .await?;
    Ok(())
}

async fn collect_relevant_events_until_complete(
    codex: &crate::product::agent::CodexThread,
) -> Vec<RelevantTurnEvent> {
    let mut events = Vec::new();
    loop {
        match wait_for_event(codex, |_| true).await {
            EventMsg::AgentMessage(event) => {
                events.push(RelevantTurnEvent::AgentMessage(event.message));
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::Plan(item),
                ..
            }) => events.push(RelevantTurnEvent::PlanItem(item.text)),
            EventMsg::PlanUpdate(_) => events.push(RelevantTurnEvent::PlanUpdate),
            EventMsg::Error(_) => events.push(RelevantTurnEvent::Error),
            EventMsg::TurnFinalizing => events.push(RelevantTurnEvent::TurnFinalizing),
            EventMsg::TurnComplete(_) => {
                events.push(RelevantTurnEvent::TurnComplete);
                break;
            }
            _ => {}
        }
    }
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_tool_executes_command_and_streams_output() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex().with_model("gpt-5");
    let TestCodex {
        codex,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;

    let call_id = "shell-tool-call";
    let command = vec!["/bin/echo", "tool harness"];
    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_local_shell_call(call_id, "completed", command),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "all done"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please run the shell command".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;

    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let req = second_mock.single_request();
    let (output_text, _) = call_output(&req, call_id);
    let exec_output: Value = serde_json::from_str(&output_text)?;
    assert_eq!(exec_output["metadata"]["exit_code"], 0);
    let stdout = exec_output["output"].as_str().expect("stdout field");
    assert_regex_match(r"(?s)^tool harness\n?$", stdout);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_plan_tool_emits_plan_update_event() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex();
    let TestCodex {
        codex,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;

    let call_id = "plan-tool-call";
    let plan_args = json!({
        "explanation": "Tool harness check",
        "plan": [
            {"step": "Inspect workspace", "status": "in_progress"},
            {"step": "Report results", "status": "pending"},
        ],
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, "update_plan", &plan_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "plan acknowledged"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please update the plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut saw_plan_update = false;
    wait_for_event(&codex, |event| match event {
        EventMsg::PlanUpdate(update) => {
            saw_plan_update = true;
            assert_eq!(update.explanation.as_deref(), Some("Tool harness check"));
            assert_eq!(update.plan.len(), 2);
            assert_eq!(update.plan[0].step, "Inspect workspace");
            assert_matches!(update.plan[0].status, StepStatus::InProgress);
            assert_eq!(update.plan[1].step, "Report results");
            assert_matches!(update.plan[1].status, StepStatus::Pending);
            false
        }
        EventMsg::TurnComplete(_) => true,
        _ => false,
    })
    .await;

    assert!(saw_plan_update, "expected PlanUpdate event");

    let req = second_mock.single_request();
    let (output_text, _success_flag) = call_output(&req, call_id);
    assert_eq!(output_text, "Plan updated");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_finalizing_follows_final_message_after_intermediate_message_and_plan_update()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;

    let call_id = "plan-after-progress";
    let plan_args = json!({
        "explanation": "Continue after progress",
        "plan": [
            {"step": "Inspect", "status": "completed"},
            {"step": "Report", "status": "in_progress"},
        ],
    })
    .to_string();
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-progress"),
            ev_assistant_message("msg-progress", "Progress update"),
            ev_function_call(call_id, "update_plan", &plan_args),
            ev_completed("resp-progress"),
        ]),
    )
    .await;
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-final", "Final answer"),
            ev_completed("resp-final"),
        ]),
    )
    .await;

    submit_test_turn(&test, "work through the plan", None).await?;

    assert_eq!(
        collect_relevant_events_until_complete(&test.codex).await,
        vec![
            RelevantTurnEvent::AgentMessage("Progress update".to_string()),
            RelevantTurnEvent::PlanUpdate,
            RelevantTurnEvent::AgentMessage("Final answer".to_string()),
            RelevantTurnEvent::TurnFinalizing,
            RelevantTurnEvent::TurnComplete,
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pure_final_answer_emits_one_turn_finalizing_before_completion() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-final"),
            ev_assistant_message("msg-final", "Only answer"),
            ev_completed("resp-final"),
        ]),
    )
    .await;

    submit_test_turn(&test, "answer directly", None).await?;

    assert_eq!(
        collect_relevant_events_until_complete(&test.codex).await,
        vec![
            RelevantTurnEvent::AgentMessage("Only answer".to_string()),
            RelevantTurnEvent::TurnFinalizing,
            RelevantTurnEvent::TurnComplete,
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_turn_does_not_emit_turn_finalizing() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    responses::mount_sse_once(
        &server,
        responses::sse_failed(
            "resp-failed",
            "insufficient_quota",
            "quota unavailable for test",
        ),
    )
    .await;

    submit_test_turn(&test, "trigger an error", None).await?;

    assert_eq!(
        collect_relevant_events_until_complete(&test.codex).await,
        vec![RelevantTurnEvent::Error, RelevantTurnEvent::TurnComplete]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_only_turn_does_not_emit_turn_finalizing() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    let plan = "<proposed_plan>\n- Inspect\n- Report\n</proposed_plan>\n";
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-plan"),
            ev_assistant_message("msg-plan", plan),
            ev_completed("resp-plan"),
        ]),
    )
    .await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: test.session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };
    submit_test_turn(&test, "produce only a plan", Some(identity)).await?;

    assert_eq!(
        collect_relevant_events_until_complete(&test.codex).await,
        vec![
            RelevantTurnEvent::PlanItem("- Inspect\n- Report\n".to_string()),
            RelevantTurnEvent::TurnComplete,
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_turn_does_not_emit_turn_finalizing() -> anyhow::Result<()> {
    crate::test_support::core::skip_if_sandbox!(Ok(()));
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("gpt-5.1");
    let test = builder.build(&server).await?;
    let args = json!({
        "command": "sleep 60",
        "timeout_ms": 60_000,
    })
    .to_string();
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-interrupt"),
            ev_function_call("call-sleep", "shell_command", &args),
            ev_completed("resp-interrupt"),
        ]),
    )
    .await;

    submit_test_turn(&test, "start a long command", None).await?;

    let mut saw_turn_finalizing = false;
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::TurnFinalizing => saw_turn_finalizing = true,
            EventMsg::ExecCommandBegin(_) => break,
            _ => {}
        }
    }

    test.codex.submit(Op::Interrupt).await?;
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::TurnFinalizing => saw_turn_finalizing = true,
            EventMsg::TurnAborted(_) => break,
            _ => {}
        }
    }

    assert!(
        !saw_turn_finalizing,
        "interrupted turns must not enter the final-answer lifecycle"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_plan_tool_rejects_malformed_payload() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex();
    let TestCodex {
        codex,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;

    let call_id = "plan-tool-invalid";
    let invalid_args = json!({
        "explanation": "Missing plan data"
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, "update_plan", &invalid_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "malformed plan payload"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please update the plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut saw_plan_update = false;
    wait_for_event(&codex, |event| match event {
        EventMsg::PlanUpdate(_) => {
            saw_plan_update = true;
            false
        }
        EventMsg::TurnComplete(_) => true,
        _ => false,
    })
    .await;

    assert!(
        !saw_plan_update,
        "did not expect PlanUpdate event for malformed payload"
    );

    let req = second_mock.single_request();
    let (output_text, success_flag) = call_output(&req, call_id);
    assert!(
        output_text.contains("failed to parse function arguments"),
        "expected parse error message in output text, got {output_text:?}"
    );
    if let Some(success_flag) = success_flag {
        assert!(
            !success_flag,
            "expected tool output to mark success=false for malformed payload"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_patch_tool_executes_and_emits_patch_events() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::ApplyPatchFreeform);
    });
    let TestCodex {
        codex,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;

    let file_name = "notes.txt";
    let file_path = cwd.path().join(file_name);
    let call_id = "apply-patch-call";
    let patch_content = format!(
        r#"*** Begin Patch
*** Add File: {file_name}
+Tool harness apply patch
*** End Patch"#
    );

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_apply_patch_function_call(call_id, &patch_content),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "patch complete"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please apply a patch".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;

    let mut saw_patch_begin = false;
    let mut patch_end_success = None;
    wait_for_event(&codex, |event| match event {
        EventMsg::PatchApplyBegin(begin) => {
            saw_patch_begin = true;
            assert_eq!(begin.call_id, call_id);
            false
        }
        EventMsg::PatchApplyEnd(end) => {
            assert_eq!(end.call_id, call_id);
            patch_end_success = Some(end.success);
            false
        }
        EventMsg::TurnComplete(_) => true,
        _ => false,
    })
    .await;

    assert!(saw_patch_begin, "expected PatchApplyBegin event");
    let patch_end_success =
        patch_end_success.expect("expected PatchApplyEnd event to capture success flag");
    assert!(patch_end_success);

    let req = second_mock.single_request();
    let (output_text, _success_flag) = call_output(&req, call_id);

    let expected_pattern = format!(
        r"(?s)^Exit code: 0
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
Success. Updated the following files:
A {file_name}
?$"
    );
    assert_regex_match(&expected_pattern, &output_text);

    let updated_contents = fs::read_to_string(file_path)?;
    assert_eq!(
        updated_contents, "Tool harness apply patch\n",
        "expected updated file content"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_patch_reports_parse_diagnostics() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::ApplyPatchFreeform);
    });
    let TestCodex {
        codex,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;

    let call_id = "apply-patch-parse-error";
    let patch_content = r"*** Begin Patch
*** Update File: broken.txt
*** End Patch";

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_apply_patch_function_call(call_id, patch_content),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "failed"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please apply a patch".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: session_model,
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;

    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let req = second_mock.single_request();
    let (output_text, success_flag) = call_output(&req, call_id);

    assert!(
        output_text.contains("apply_patch verification failed"),
        "expected apply_patch verification failure message, got {output_text:?}"
    );
    assert!(
        output_text.contains("invalid hunk"),
        "expected parse diagnostics in output text, got {output_text:?}"
    );

    if let Some(success_flag) = success_flag {
        assert!(
            !success_flag,
            "expected tool output to mark success=false for parse failures"
        );
    }

    Ok(())
}
