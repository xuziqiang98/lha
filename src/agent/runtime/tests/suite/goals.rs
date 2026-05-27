use adam_agent::features::Feature;
use adam_agent::protocol::AskForApproval;
use adam_agent::protocol::EventMsg;
use adam_agent::protocol::Op;
use adam_agent::protocol::SandboxPolicy;
use adam_protocol::config_types::Identity;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::config_types::ReasoningSummary;
use adam_protocol::config_types::Settings;
use adam_protocol::protocol::ThreadGoalSetMode;
use adam_protocol::protocol::ThreadGoalStatus;
use adam_protocol::user_input::UserInput;
use anyhow::Result;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::time::Duration;

async fn submit_user_turn(test: &TestCodex, prompt: &str) -> Result<()> {
    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: prompt.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd.path().to_path_buf(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: Some(Identity {
                kind: IdentityKind::Programmer,
                settings: Settings {
                    model: test.session_configured.model.clone(),
                    reasoning_effort: None,
                    developer_instructions: None,
                },
            }),
            personality: None,
            tui_buddy: None,
        })
        .await?;
    Ok(())
}

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

async fn wait_for_request_count(mock: &ResponseMock, expected: usize) {
    for _ in 0..100 {
        if mock.requests().len() >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {expected} response requests");
}

async fn response_request_count(server: &wiremock::MockServer) -> usize {
    let Some(requests) = server.received_requests().await else {
        panic!("mock server should return received requests");
    };
    requests
        .into_iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .count()
}

async fn wait_for_goal_update(test: &TestCodex, objective: &str, status: ThreadGoalStatus) {
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ThreadGoalUpdated(updated)
                if updated.goal.objective == objective && updated.goal.status == status
        )
    })
    .await;
}

fn create_goal_response(call_id: &str, response_id: &str, objective: &str) -> String {
    sse(vec![
        ev_response_created(response_id),
        ev_function_call(
            call_id,
            "create_goal",
            &json!({ "objective": objective }).to_string(),
        ),
        ev_completed(response_id),
    ])
}

fn update_goal_response(call_id: &str, response_id: &str, status: &str) -> String {
    sse(vec![
        ev_response_created(response_id),
        ev_function_call(
            call_id,
            "update_goal",
            &json!({ "status": status }).to_string(),
        ),
        ev_completed(response_id),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_if_exists_requires_confirmation_for_unfinished_goal() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    let db = test.codex.state_db().expect("state db enabled");
    let original = db
        .replace_thread_goal(
            test.session_configured.session_id,
            "old goal",
            adam_state::ThreadGoalStatus::Active,
            None,
        )
        .await?;

    test.codex
        .submit(Op::ThreadGoalSetObjective {
            objective: "replacement goal".to_string(),
            mode: ThreadGoalSetMode::ConfirmIfExists,
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ThreadGoalReplaceConfirmationRequired(required)
                if required.existing_goal.goal_id == original.goal_id
                    && required.objective == "replacement goal"
        )
    })
    .await;

    assert_eq!(
        Some(original),
        db.get_thread_goal(test.session_configured.session_id)
            .await?
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_goal_confirmation_cannot_replace_new_goal() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    let db = test.codex.state_db().expect("state db enabled");
    let original = db
        .replace_thread_goal(
            test.session_configured.session_id,
            "old goal",
            adam_state::ThreadGoalStatus::Active,
            None,
        )
        .await?;
    let replacement = db
        .replace_thread_goal(
            test.session_configured.session_id,
            "current goal",
            adam_state::ThreadGoalStatus::Active,
            None,
        )
        .await?;

    test.codex
        .submit(Op::ThreadGoalSetObjective {
            objective: "stale replacement".to_string(),
            mode: ThreadGoalSetMode::ReplaceExisting {
                expected_goal_id: original.goal_id,
            },
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::Error(error)
                if error
                    .message
                    .contains("Goal changed before this replacement was confirmed")
        )
    })
    .await;

    assert_eq!(
        Some(replacement),
        db.get_thread_goal(test.session_configured.session_id)
            .await?
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_goal_replace_replaces_matching_goal() -> Result<()> {
    let server = start_mock_server().await;
    let _mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-replaced-goal", "working replacement goal"),
            ev_completed("resp-replaced-goal"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    let db = test.codex.state_db().expect("state db enabled");
    let original = db
        .replace_thread_goal(
            test.session_configured.session_id,
            "old goal",
            adam_state::ThreadGoalStatus::Active,
            Some(100),
        )
        .await?;

    test.codex
        .submit(Op::ThreadGoalSetObjective {
            objective: "replacement goal".to_string(),
            mode: ThreadGoalSetMode::ReplaceExisting {
                expected_goal_id: original.goal_id.clone(),
            },
        })
        .await?;

    wait_for_goal_update(&test, "replacement goal", ThreadGoalStatus::Active).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("goal should exist");
    assert_ne!(original.goal_id, goal.goal_id);
    assert_eq!(goal.objective, "replacement goal");
    assert_eq!(goal.status, adam_state::ThreadGoalStatus::Active);
    assert_eq!(goal.token_budget, None);
    assert_eq!(goal.tokens_used, 0);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_goal_continues_after_incomplete_goal_turn() -> Result<()> {
    let server = start_mock_server().await;
    let responses = vec![
        create_goal_response("create-goal", "resp-1", "finish the migration"),
        sse(vec![
            ev_assistant_message("msg-1", "goal created"),
            ev_completed("resp-2"),
        ]),
        sse(vec![
            ev_assistant_message("msg-2", "still working"),
            ev_completed("resp-3"),
        ]),
        update_goal_response("complete-goal", "resp-4", "complete"),
        sse(vec![
            ev_assistant_message("msg-3", "goal complete"),
            ev_completed("resp-5"),
        ]),
    ];
    let mock = mount_sse_sequence(&server, responses).await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;

    submit_user_turn(&test, "create a goal").await?;
    for _ in 0..3 {
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
    }

    let db = test.codex.state_db().expect("state db enabled");
    let goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("goal should exist");
    assert_eq!(goal.status, adam_state::ThreadGoalStatus::Complete);

    let goal_context_requests = mock
        .requests()
        .iter()
        .filter(|request| {
            request
                .message_input_texts("user")
                .iter()
                .any(|text| text.contains("<goal_context>"))
        })
        .count();
    assert!(
        goal_context_requests >= 2,
        "expected at least two goal continuation requests"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_goal_continues_after_switching_back_to_programmer() -> Result<()> {
    let server = start_mock_server().await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-switched-goal", "working resumed goal"),
            ev_completed("resp-switched-goal"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    let db = test.codex.state_db().expect("state db enabled");
    db.replace_thread_goal(
        test.session_configured.session_id,
        "resume after identity switch",
        adam_state::ThreadGoalStatus::Active,
        None,
    )
    .await?;

    switch_to_programmer(&test).await?;
    wait_for_request_count(&mock, 1).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let request = mock.single_request();
    let user_texts = request.message_input_texts("user");
    assert!(
        user_texts
            .iter()
            .any(|text| text.contains("<goal_context>")),
        "goal continuation should include goal context"
    );
    assert!(
        user_texts
            .iter()
            .any(|text| text.contains("resume after identity switch")),
        "goal continuation should include the active objective"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_goal_accounts_usage_before_continuing() -> Result<()> {
    let server = start_mock_server().await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-budget", "still working"),
            ev_completed_with_tokens("resp-budget", 12),
        ]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    let db = test.codex.state_db().expect("state db enabled");
    db.replace_thread_goal(
        test.session_configured.session_id,
        "stay within budget",
        adam_state::ThreadGoalStatus::Active,
        Some(10),
    )
    .await?;

    test.codex
        .submit(Op::ThreadGoalSetStatus {
            status: ThreadGoalStatus::Active,
        })
        .await?;
    wait_for_request_count(&mock, 1).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("goal should exist");
    assert_eq!(goal.objective, "stay within budget");
    assert_eq!(goal.status, adam_state::ThreadGoalStatus::BudgetLimited);
    assert_eq!(goal.tokens_used, 12);
    assert!(goal.time_used_seconds >= 1);

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(response_request_count(&server).await, 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_goal_accounts_usage_for_user_turn() -> Result<()> {
    let server = start_mock_server().await;
    mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-user-goal", "worked on active goal"),
            ev_completed_with_tokens("resp-user-goal", 18),
        ]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    let db = test.codex.state_db().expect("state db enabled");
    db.replace_thread_goal(
        test.session_configured.session_id,
        "manually continue active goal",
        adam_state::ThreadGoalStatus::Active,
        None,
    )
    .await?;

    submit_user_turn(&test, "continue the current goal").await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("goal should exist");
    assert_eq!(goal.objective, "manually continue active goal");
    assert_eq!(goal.status, adam_state::ThreadGoalStatus::Active);
    assert_eq!(goal.tokens_used, 18);
    assert!(goal.time_used_seconds >= 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_goal_accounts_usage_after_goal_creation_in_same_turn() -> Result<()> {
    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![
            create_goal_response("create-goal", "resp-create-goal", "finish same turn"),
            update_goal_response("complete-goal", "resp-complete-goal", "complete"),
            sse(vec![
                ev_assistant_message("msg-created-goal", "goal complete"),
                ev_completed_with_tokens("resp-created-goal", 25),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;

    submit_user_turn(&test, "create and finish a goal").await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let db = test.codex.state_db().expect("state db enabled");
    let goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("goal should exist");
    assert_eq!(goal.objective, "finish same turn");
    assert_eq!(goal.status, adam_state::ThreadGoalStatus::Complete);
    assert_eq!(goal.tokens_used, 25);
    assert!(goal.time_used_seconds >= 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopped_goal_does_not_account_usage_for_user_turn() -> Result<()> {
    let server = start_mock_server().await;
    mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-stopped-goal", "ordinary response"),
            ev_completed_with_tokens("resp-stopped-goal", 18),
        ]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    let db = test.codex.state_db().expect("state db enabled");
    db.replace_thread_goal(
        test.session_configured.session_id,
        "paused goal",
        adam_state::ThreadGoalStatus::Paused,
        None,
    )
    .await?;

    submit_user_turn(&test, "answer unrelated question").await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("goal should exist");
    assert_eq!(goal.objective, "paused goal");
    assert_eq!(goal.status, adam_state::ThreadGoalStatus::Paused);
    assert_eq!(goal.tokens_used, 0);
    assert_eq!(goal.time_used_seconds, 0);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_goal_continuation_seeds_initial_context() -> Result<()> {
    let server = start_mock_server().await;
    let mut initial_builder = test_codex();
    let initial = initial_builder.build(&server).await?;
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-initial", "initial turn complete"),
            ev_completed("resp-initial"),
        ]),
    )
    .await;
    initial.submit_turn("record initial history").await?;

    let instructions = "resume goal continuation must see these instructions".to_string();
    let expected_instructions = instructions.clone();
    let resumed_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-resumed-goal", "working resumed goal"),
            ev_completed_with_tokens("resp-resumed-goal", 10),
        ]),
    )
    .await;
    let mut resume_builder = test_codex().with_config(move |config| {
        config.features.enable(Feature::Goals);
        config.user_instructions = Some(instructions);
    });
    let resumed = resume_builder.resume(&server, home, rollout_path).await?;
    switch_to_programmer(&resumed).await?;

    let db = resumed.codex.state_db().expect("state db enabled");
    db.replace_thread_goal(
        resumed.session_configured.session_id,
        "resume with context",
        adam_state::ThreadGoalStatus::Active,
        Some(1),
    )
    .await?;

    resumed
        .codex
        .submit(Op::ThreadGoalSetStatus {
            status: ThreadGoalStatus::Active,
        })
        .await?;
    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let request = resumed_mock.single_request();
    let user_texts = request.message_input_texts("user");
    let cwd = resumed.cwd.path().to_string_lossy();
    assert!(
        user_texts
            .iter()
            .any(|text| text.contains(&expected_instructions)),
        "resumed goal continuation should include user instructions"
    );
    assert!(
        user_texts.iter().any(|text| {
            text.contains("<environment_context>") && text.contains(&format!("<cwd>{cwd}</cwd>"))
        }),
        "resumed goal continuation should include current cwd"
    );
    assert!(
        user_texts
            .iter()
            .any(|text| text.contains("<goal_context>")),
        "request should be the resumed goal continuation"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_goal_turn_cannot_complete_replacement_goal() -> Result<()> {
    let server = start_mock_server().await;
    let responses = vec![
        sse_response(create_goal_response("create-goal", "resp-1", "old goal")),
        sse_response(sse(vec![
            ev_assistant_message("msg-1", "goal created"),
            ev_completed("resp-2"),
        ])),
        sse_response(update_goal_response("stale-complete", "resp-3", "complete"))
            .set_delay(Duration::from_millis(750)),
        sse_response(sse(vec![
            ev_assistant_message("msg-2", "stale update rejected"),
            ev_completed("resp-4"),
        ])),
    ];
    let mock = mount_response_sequence(&server, responses).await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;

    submit_user_turn(&test, "create a goal").await?;
    wait_for_goal_update(&test, "old goal", ThreadGoalStatus::Active).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    wait_for_request_count(&mock, 3).await;

    let db = test.codex.state_db().expect("state db enabled");
    let old_goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("old goal should exist");
    test.codex
        .submit(Op::ThreadGoalSetObjective {
            objective: "replacement goal".to_string(),
            mode: ThreadGoalSetMode::ReplaceExisting {
                expected_goal_id: old_goal.goal_id,
            },
        })
        .await?;
    wait_for_goal_update(&test, "replacement goal", ThreadGoalStatus::Active).await;

    test.codex
        .submit(Op::ThreadGoalSetStatus {
            status: ThreadGoalStatus::Paused,
        })
        .await?;
    wait_for_goal_update(&test, "replacement goal", ThreadGoalStatus::Paused).await;

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let failure_request = mock
        .requests()
        .into_iter()
        .find(|request| {
            request
                .function_call_output_text("stale-complete")
                .is_some()
        })
        .expect("stale update output request should be sent");
    let content = failure_request
        .function_call_output_text("stale-complete")
        .expect("stale update output should be present");
    assert!(
        content.contains("goal changed before this update"),
        "stale update output should explain the goal changed"
    );

    let goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("goal should exist");
    assert_eq!(goal.objective, "replacement goal");
    assert_eq!(goal.status, adam_state::ThreadGoalStatus::Paused);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_goal_turn_without_bound_goal_cannot_complete_new_goal() -> Result<()> {
    let server = start_mock_server().await;
    let responses = vec![
        sse_response(update_goal_response(
            "unbound-complete",
            "resp-1",
            "complete",
        ))
        .set_delay(Duration::from_millis(750)),
        sse_response(sse(vec![
            ev_assistant_message("msg-1", "unbound update rejected"),
            ev_completed("resp-2"),
        ])),
    ];
    let mock = mount_response_sequence(&server, responses).await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;

    submit_user_turn(&test, "try to complete the current goal").await?;
    wait_for_request_count(&mock, 1).await;

    test.codex
        .submit(Op::ThreadGoalSetObjective {
            objective: "new goal".to_string(),
            mode: ThreadGoalSetMode::ConfirmIfExists,
        })
        .await?;
    wait_for_goal_update(&test, "new goal", ThreadGoalStatus::Active).await;

    test.codex
        .submit(Op::ThreadGoalSetStatus {
            status: ThreadGoalStatus::Paused,
        })
        .await?;
    wait_for_goal_update(&test, "new goal", ThreadGoalStatus::Paused).await;

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let failure_request = mock
        .requests()
        .into_iter()
        .find(|request| {
            request
                .function_call_output_text("unbound-complete")
                .is_some()
        })
        .expect("unbound update output request should be sent");
    let content = failure_request
        .function_call_output_text("unbound-complete")
        .expect("unbound update output should be present");
    assert!(
        content.contains("call get_goal before updating the current goal"),
        "unbound update output should tell the model to refresh goal state"
    );

    let db = test.codex.state_db().expect("state db enabled");
    let goal = db
        .get_thread_goal(test.session_configured.session_id)
        .await?
        .expect("goal should exist");
    assert_eq!(goal.objective, "new goal");
    assert_eq!(goal.status, adam_state::ThreadGoalStatus::Paused);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_goal_edit_cannot_update_replacement_goal() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    let db = test.codex.state_db().expect("state db enabled");
    let original = db
        .replace_thread_goal(
            test.session_configured.session_id,
            "old goal",
            adam_state::ThreadGoalStatus::Active,
            None,
        )
        .await?;
    let replacement = db
        .replace_thread_goal(
            test.session_configured.session_id,
            "replacement goal",
            adam_state::ThreadGoalStatus::Paused,
            Some(10),
        )
        .await?;

    test.codex
        .submit(Op::ThreadGoalSetObjective {
            objective: "stale edit objective".to_string(),
            mode: ThreadGoalSetMode::UpdateExisting {
                expected_goal_id: original.goal_id,
                status: ThreadGoalStatus::Active,
                token_budget: None,
            },
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::Error(error)
                if error
                    .message
                    .contains("Goal changed before this edit was submitted")
        )
    })
    .await;

    assert_eq!(
        Some(replacement),
        db.get_thread_goal(test.session_configured.session_id)
            .await?
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_goal_edit_cannot_recreate_cleared_goal() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Goals);
    });
    let test = builder.build(&server).await?;
    switch_to_programmer(&test).await?;

    let db = test.codex.state_db().expect("state db enabled");
    let original = db
        .replace_thread_goal(
            test.session_configured.session_id,
            "old goal",
            adam_state::ThreadGoalStatus::Active,
            None,
        )
        .await?;
    assert!(
        db.delete_thread_goal(test.session_configured.session_id)
            .await?
    );

    test.codex
        .submit(Op::ThreadGoalSetObjective {
            objective: "stale edit objective".to_string(),
            mode: ThreadGoalSetMode::UpdateExisting {
                expected_goal_id: original.goal_id,
                status: ThreadGoalStatus::Active,
                token_budget: None,
            },
        })
        .await?;

    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::Error(error)
                if error
                    .message
                    .contains("Goal changed before this edit was submitted")
        )
    })
    .await;

    assert_eq!(
        None,
        db.get_thread_goal(test.session_configured.session_id)
            .await?
    );

    Ok(())
}
