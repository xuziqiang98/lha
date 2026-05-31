use anyhow::Result;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use lha_agent::features::Feature;
use lha_agent::protocol::EventMsg;
use lha_agent::protocol::IDENTITY_CLOSE_TAG;
use lha_agent::protocol::IDENTITY_OPEN_TAG;
use lha_agent::protocol::Op;
use lha_protocol::config_types::Identity;
use lha_protocol::config_types::IdentityKind;
use lha_protocol::config_types::Settings;
use lha_protocol::protocol::ThreadPlanRunStatus;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::time::Duration;

async fn switch_to_programmer(test: &TestCodex) -> Result<()> {
    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(Identity {
                kind: IdentityKind::Programmer,
                settings: Settings {
                    model: test.session_configured.model.clone(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            personality: None,
        })
        .await?;
    Ok(())
}

fn identity_with_instructions(
    kind: IdentityKind,
    model: impl Into<String>,
    instructions: &str,
) -> Identity {
    Identity {
        kind,
        settings: Settings {
            model: model.into(),
            reasoning_effort: None,
            developer_instructions: Some(instructions.to_string()),
        },
    }
}

fn identity_xml(text: &str) -> String {
    format!("{IDENTITY_OPEN_TAG}{text}{IDENTITY_CLOSE_TAG}")
}

async fn wait_for_request_count(mock: &ResponseMock, expected: usize) {
    for _ in 0..100 {
        if mock.requests().len() >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {expected} response requests");
}

fn update_plan_run_response(call_id: &str, response_id: &str, status: &str) -> String {
    sse(vec![
        ev_response_created(response_id),
        ev_function_call(
            call_id,
            "update_plan_run",
            &json!({ "status": status }).to_string(),
        ),
        ev_completed(response_id),
    ])
}

async fn wait_for_plan_run_update(test: &TestCodex, status: ThreadPlanRunStatus) {
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ThreadPlanRunUpdated(updated) if updated.plan_run.status == status
        )
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_completion_injects_programmer_identity_after_planner_switch() -> Result<()> {
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("msg-planner", "plan captured"),
                ev_completed("resp-planner"),
            ]),
            update_plan_run_response("complete-plan", "resp-complete", "complete"),
            sse(vec![
                ev_assistant_message("msg-complete-plan", "plan complete"),
                ev_completed("resp-after-complete"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::PlanCompletion);
    });
    let test = builder.build(&server).await?;
    let planner_text = "planner instructions";
    let programmer_text = "programmer instructions";
    let model = test.session_configured.model.clone();

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_instructions(
                IdentityKind::Planner,
                model.clone(),
                planner_text,
            )),
            personality: None,
        })
        .await?;
    test.codex
        .submit(Op::UserInput {
            items: vec![lha_protocol::user_input::UserInput::Text {
                text: "draft a plan".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_instructions(
                IdentityKind::Programmer,
                model,
                programmer_text,
            )),
            personality: None,
        })
        .await?;
    test.codex
        .submit(Op::ThreadPlanRunStart {
            plan_text: "# Test plan\n- implement it".to_string(),
        })
        .await?;

    wait_for_request_count(&mock, 2).await;
    let requests = mock.requests();
    let request = requests
        .iter()
        .find(|request| {
            request
                .message_input_texts("user")
                .iter()
                .any(|text| text.contains("<plan_completion_context>"))
        })
        .expect("continuation request should include plan context");
    let developer_texts = request.message_input_texts("developer");
    let planner_identity = identity_xml(planner_text);
    let programmer_identity = identity_xml(programmer_text);

    assert!(
        developer_texts
            .iter()
            .any(|text| text.as_str() == planner_identity),
        "expected prior planner identity in history, got: {developer_texts:?}"
    );
    assert!(
        developer_texts
            .iter()
            .any(|text| text.as_str() == programmer_identity),
        "expected programmer identity update before plan continuation, got: {developer_texts:?}"
    );
    let last_identity = developer_texts
        .iter()
        .rev()
        .find(|text| text.starts_with(IDENTITY_OPEN_TAG))
        .expect("expected at least one identity developer message");
    assert_eq!(last_identity, &programmer_identity);

    wait_for_plan_run_update(&test, ThreadPlanRunStatus::Complete).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_plan_run_continues_until_complete() -> Result<()> {
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            update_plan_run_response("complete-plan", "resp-1", "complete"),
            sse(vec![
                ev_assistant_message("msg-complete-plan", "plan complete"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::PlanCompletion);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    test.codex
        .submit(Op::ThreadPlanRunStart {
            plan_text: "# Test plan\n- implement it".to_string(),
        })
        .await?;

    wait_for_plan_run_update(&test, ThreadPlanRunStatus::Active).await;
    wait_for_request_count(&mock, 2).await;
    wait_for_plan_run_update(&test, ThreadPlanRunStatus::Complete).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let db = test.codex.state_db().expect("state db enabled");
    let plan_run = db
        .get_thread_plan_run(test.session_configured.session_id)
        .await?
        .expect("plan run should exist");
    assert_eq!(lha_state::ThreadPlanRunStatus::Complete, plan_run.status);

    let requests = mock.requests();
    let request = requests
        .iter()
        .find(|request| {
            request
                .message_input_texts("user")
                .iter()
                .any(|text| text.contains("<plan_completion_context>"))
        })
        .expect("continuation request should include plan context");
    let user_texts = request.message_input_texts("user");
    assert!(
        user_texts
            .iter()
            .any(|text| text.contains("# Test plan") && text.contains("update_plan_run")),
        "plan completion prompt should include the plan and completion tool guidance"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_run_start_rejects_unfinished_goal() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::PlanCompletion);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    let db = test.codex.state_db().expect("state db enabled");
    db.replace_thread_goal(
        test.session_configured.session_id,
        "finish goal first",
        lha_state::ThreadGoalStatus::Active,
        None,
    )
    .await?;

    test.codex
        .submit(Op::ThreadPlanRunStart {
            plan_text: "# blocked by goal".to_string(),
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::Error(error)
                if error
                    .message
                    .contains("Cannot start YOLO plan completion while a programmer goal is unfinished")
        )
    })
    .await;

    assert_eq!(
        None,
        db.get_thread_plan_run(test.session_configured.session_id)
            .await?
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_run_start_rejects_unfinished_plan_run() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::PlanCompletion);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    let db = test.codex.state_db().expect("state db enabled");
    let original = db
        .replace_thread_plan_run(
            test.session_configured.session_id,
            "# original plan",
            lha_state::ThreadPlanRunStatus::Active,
            None,
        )
        .await?;

    test.codex
        .submit(Op::ThreadPlanRunStart {
            plan_text: "# replacement plan".to_string(),
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::Error(error)
                if error
                    .message
                    .contains("A YOLO plan completion is already unfinished")
        )
    })
    .await;

    assert_eq!(
        Some(original),
        db.get_thread_plan_run(test.session_configured.session_id)
            .await?
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_completion_feature_disabled_rejects_start() -> Result<()> {
    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;
    switch_to_programmer(&test).await?;

    test.codex
        .submit(Op::ThreadPlanRunStart {
            plan_text: "# disabled plan".to_string(),
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::Error(error)
                if error.message.contains("YOLO plan completion is disabled")
        )
    })
    .await;

    Ok(())
}
