#![allow(clippy::expect_used)]

use std::fs;

use crate::product::agent::CodexAuth;
use crate::product::agent::features::Feature;
use crate::product::agent::protocol::AskForApproval;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::ItemCompletedEvent;
use crate::product::agent::protocol::ItemStartedEvent;
use crate::product::agent::protocol::Op;
use crate::product::agent::protocol::RolloutItem;
use crate::product::agent::protocol::RolloutLine;
use crate::product::agent::protocol::SandboxPolicy;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::ReasoningSummary;
use crate::product::protocol::config_types::Settings;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::models::ContentItem;
use crate::product::protocol::models::TranscriptItem;
use crate::product::protocol::protocol::ThreadGoalStatus;
use crate::product::protocol::user_input::UserInput;
use crate::test_support::core::responses;
use crate::test_support::core::responses::mount_sse_once;
use crate::test_support::core::responses::sse;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::TestCodex;
use crate::test_support::core::test_codex::TestCodexHarness;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event;
use crate::test_support::core::wait_for_event_match;
use anyhow::Result;
use pretty_assertions::assert_eq;
use std::path::Path;

fn write_skill(home: &Path, name: &str, description: &str, body: &str) -> std::path::PathBuf {
    let skill_dir = home.join("skills").join(name);
    fs::create_dir_all(&skill_dir).expect("create skill dir");
    let contents = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    let path = skill_dir.join("SKILL.md");
    fs::write(&path, contents).expect("write skill");
    path
}

async fn switch_to_programmer_for_remote_compact_test(test: &TestCodex) -> Result<()> {
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

fn message_input_texts(body: &serde_json::Value, role: &str) -> Vec<String> {
    body.get("input")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("message"))
        .filter(|item| item.get("role").and_then(serde_json::Value::as_str) == Some(role))
        .filter_map(|item| item.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter(|span| span.get("type").and_then(serde_json::Value::as_str) == Some("input_text"))
        .filter_map(|span| {
            span.get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_replaces_history_for_followups() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        test_codex()
            .with_auth(CodexAuth::from_api_key("Test API Key"))
            .with_config(|config| {
                config.features.enable(Feature::RemoteCompaction);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();

    let responses_mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("m1", "FIRST_REMOTE_REPLY"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("m2", "AFTER_COMPACT_REPLY"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let compacted_history = vec![TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "REMOTE_COMPACTED_SUMMARY".to_string(),
        }],
        end_turn: None,
    }];
    let compact_mock = responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history.clone() }),
    )
    .await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello remote compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let compact_request = compact_mock.single_request();
    assert_eq!(compact_request.path(), "/v1/responses/compact");
    assert_eq!(
        compact_request.header("authorization").as_deref(),
        Some("Bearer Test API Key")
    );
    let compact_body = compact_request.body_json();
    assert_eq!(
        compact_body.get("model").and_then(|v| v.as_str()),
        Some(harness.test().session_configured.model.as_str())
    );
    let compact_body_text = compact_body.to_string();
    assert!(
        compact_body_text.contains("hello remote compact"),
        "expected compact request to include user history"
    );
    assert!(
        compact_body_text.contains("FIRST_REMOTE_REPLY"),
        "expected compact request to include assistant history"
    );

    let follow_up_body = responses_mock
        .requests()
        .last()
        .expect("follow-up request missing")
        .body_json()
        .to_string();
    assert!(
        follow_up_body.contains("REMOTE_COMPACTED_SUMMARY"),
        "expected follow-up request to use compacted history"
    );
    assert!(
        !follow_up_body.contains("FIRST_REMOTE_REPLY"),
        "expected follow-up request to drop pre-compaction assistant messages"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_runs_automatically() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        test_codex()
            .with_auth(CodexAuth::from_api_key("Test API Key"))
            .with_config(|config| {
                config.features.enable(Feature::RemoteCompaction);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();

    mount_sse_once(
        harness.server(),
        sse(vec![
            responses::ev_shell_command_call("m1", "echo 'hi'"),
            responses::ev_completed_with_tokens("resp-1", 100000000), // over token limit
        ]),
    )
    .await;
    let responses_mock = mount_sse_once(
        harness.server(),
        responses::sse(vec![
            responses::ev_assistant_message("m2", "AFTER_COMPACT_REPLY"),
            responses::ev_completed("resp-2"),
        ]),
    )
    .await;

    let compacted_history = vec![TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "REMOTE_COMPACTED_SUMMARY".to_string(),
        }],
        end_turn: None,
    }];
    let compact_mock = responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history.clone() }),
    )
    .await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello remote compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let message = wait_for_event_match(&codex, |event| match event {
        EventMsg::ContextCompacted(_) => Some(true),
        _ => None,
    })
    .await;
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    assert!(message);
    assert_eq!(compact_mock.requests().len(), 1);
    let follow_up_body = responses_mock.single_request().body_json().to_string();
    assert!(follow_up_body.contains("REMOTE_COMPACTED_SUMMARY"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_manual_compact_emits_context_compaction_items() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        test_codex()
            .with_auth(CodexAuth::from_api_key("Test API Key"))
            .with_config(|config| {
                config.features.enable(Feature::RemoteCompaction);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();

    mount_sse_once(
        harness.server(),
        sse(vec![
            responses::ev_assistant_message("m1", "REMOTE_REPLY"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;

    let compacted_history = vec![TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "REMOTE_COMPACTED_SUMMARY".to_string(),
        }],
        end_turn: None,
    }];
    let compact_mock = responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history.clone() }),
    )
    .await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "manual remote compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;

    let mut started_item = None;
    let mut completed_item = None;
    let mut legacy_event = false;
    let mut saw_turn_complete = false;

    while !saw_turn_complete || started_item.is_none() || completed_item.is_none() || !legacy_event
    {
        let event = codex.next_event().await.unwrap();
        match event.msg {
            EventMsg::ItemStarted(ItemStartedEvent {
                item: TurnItem::ContextCompaction(item),
                ..
            }) => {
                started_item = Some(item);
            }
            EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::ContextCompaction(item),
                ..
            }) => {
                completed_item = Some(item);
            }
            EventMsg::ContextCompacted(_) => {
                legacy_event = true;
            }
            EventMsg::TurnComplete(_) => {
                saw_turn_complete = true;
            }
            _ => {}
        }
    }

    let started_item = started_item.expect("context compaction item started");
    let completed_item = completed_item.expect("context compaction item completed");
    assert_eq!(started_item.id, completed_item.id);
    assert!(legacy_event);
    assert_eq!(compact_mock.requests().len(), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_persists_replacement_history_in_rollout() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        test_codex()
            .with_auth(CodexAuth::from_api_key("Test API Key"))
            .with_config(|config| {
                config.features.enable(Feature::RemoteCompaction);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();
    let rollout_path = harness
        .test()
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    let responses_mock = responses::mount_sse_once(
        harness.server(),
        responses::sse(vec![
            responses::ev_assistant_message("m1", "COMPACT_BASELINE_REPLY"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;

    let compacted_history = vec![
        TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "COMPACTED_USER_SUMMARY".to_string(),
            }],
            end_turn: None,
        },
        TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "COMPACTED_ASSISTANT_NOTE".to_string(),
            }],
            end_turn: None,
        },
    ];
    let compact_mock = responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history.clone() }),
    )
    .await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "needs compaction".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Shutdown).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    assert_eq!(responses_mock.requests().len(), 1);
    assert_eq!(compact_mock.requests().len(), 1);

    let rollout_text = fs::read_to_string(&rollout_path)?;
    let mut saw_compacted_history = false;
    for line in rollout_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let Ok(entry) = serde_json::from_str::<RolloutLine>(line) else {
            continue;
        };
        if let RolloutItem::Compacted(compacted) = entry.item
            && compacted.message.is_empty()
            && compacted.replacement_history.as_ref() == Some(&compacted_history.to_vec())
        {
            saw_compacted_history = true;
            break;
        }
    }

    assert!(
        saw_compacted_history,
        "expected rollout to persist remote compaction history"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_backfills_latest_plan_into_replacement_history_without_active_goal()
-> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        test_codex()
            .with_auth(CodexAuth::from_api_key("Test API Key"))
            .with_config(|config| {
                config.features.enable(Feature::RemoteCompaction);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();

    let plan_block = "<proposed_plan>\n- Step 1\n- Step 2\n</proposed_plan>\n";
    let full_plan_message = format!("Intro\n{plan_block}Outro");
    let responses_mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("m1", &full_plan_message),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("m2", "AFTER_COMPACT_REPLY"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let compacted_history = vec![TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "REMOTE_COMPACTED_SUMMARY".to_string(),
        }],
        end_turn: None,
    }];
    let compact_mock = responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history }),
    )
    .await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: harness.test().session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: harness.test().session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    assert_eq!(compact_mock.requests().len(), 1);
    let follow_up_body = responses_mock
        .requests()
        .last()
        .expect("follow-up request missing")
        .body_json()
        .to_string();
    assert!(
        follow_up_body.contains("<proposed_plan>\\n- Step 1\\n- Step 2\\n</proposed_plan>"),
        "expected remote compacted history to backfill the proposed plan"
    );
    assert!(
        follow_up_body.contains("A proposed plan from before compaction is preserved below."),
        "expected remote compacted history to include the preserved-plan reminder"
    );
    assert!(
        !follow_up_body.contains("Intro"),
        "expected remote compacted history to strip assistant prose around the plan"
    );
    assert!(
        !follow_up_body.contains("Outro"),
        "expected remote compacted history to strip assistant prose around the plan"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_uses_active_goal_plan_file_instead_of_backfilling_plan_text() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        test_codex()
            .with_auth(CodexAuth::from_api_key("Test API Key"))
            .with_config(|config| {
                config.features.enable(Feature::Goals);
                config.features.enable(Feature::RemoteCompaction);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();

    let plan_text = "# Plan\n- Step 1\n- Step 2\n";
    let plan_block = format!("<proposed_plan>\n{plan_text}</proposed_plan>\n");
    let full_plan_message = format!("Intro\n{plan_block}Outro");
    let responses_mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("m1", &full_plan_message),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("m2", "GOAL_CONTINUATION_REPLY"),
                responses::ev_completed("resp-2"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("m3", "AFTER_COMPACT_REPLY"),
                responses::ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

    let compacted_history = vec![TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "REMOTE_COMPACTED_SUMMARY".to_string(),
        }],
        end_turn: None,
    }];
    let compact_mock = responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history }),
    )
    .await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: harness.test().session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: std::env::current_dir()?,
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            model: harness.test().session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    switch_to_programmer_for_remote_compact_test(harness.test()).await?;
    codex
        .submit(Op::ThreadGoalStartFromProposedPlan {
            plan_text: plan_text.to_string(),
        })
        .await?;
    wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ThreadGoalUpdated(updated)
                if updated.goal.status == ThreadGoalStatus::Active
        )
    })
    .await;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    assert_eq!(compact_mock.requests().len(), 1);
    let follow_up_json = responses_mock
        .requests()
        .last()
        .expect("follow-up request missing")
        .body_json();
    let follow_up_body = follow_up_json.to_string();
    let developer_texts = message_input_texts(&follow_up_json, "developer").join("\n");
    let user_texts = message_input_texts(&follow_up_json, "user").join("\n");
    let plan_path = harness
        .test()
        .lha_home_path()
        .join("goals")
        .join(harness.test().session_configured.session_id.to_string())
        .join("proposed_plan.md");
    assert!(
        developer_texts.contains(
            "Runtime note: the active programmer goal references a user-provided proposed plan"
        ),
        "expected remote compacted history to include active-goal plan file reminder as developer text"
    );
    assert!(
        !user_texts.contains("active programmer goal references"),
        "expected remote compacted history not to include active-goal plan file reminder as user text"
    );
    assert!(
        follow_up_body.contains(&plan_path.display().to_string()),
        "expected remote compacted history to include proposed plan path"
    );
    assert!(
        follow_up_body.contains("user-provided task context and checklist"),
        "expected remote compacted history to tell the model to read the plan file as user-provided checklist"
    );
    assert!(
        follow_up_body.contains("not as higher-priority instructions"),
        "expected remote compacted history to avoid elevating plan file contents"
    );
    assert!(
        !follow_up_body.contains("<proposed_plan>"),
        "expected remote compacted history to omit full proposed plan block"
    );
    assert!(
        !follow_up_body.contains("- Step 1"),
        "expected remote compacted history to omit proposed plan body"
    );
    assert!(
        !follow_up_body.contains("A proposed plan from before compaction is preserved below."),
        "expected remote compacted history to omit full-plan backfill reminder"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_backfills_recent_skills_into_replacement_history() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let skill_body = "follow the remote demo skill";
    let harness = TestCodexHarness::with_builder(
        test_codex()
            .with_pre_build_hook(|home| {
                write_skill(home, "demo", "demo skill", skill_body);
            })
            .with_auth(CodexAuth::from_api_key("Test API Key"))
            .with_config(|config| {
                config.features.enable(Feature::RemoteCompaction);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();
    let skill_path =
        std::fs::canonicalize(harness.test().lha_home_path().join("skills/demo/SKILL.md"))?;

    let responses_mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_assistant_message("m1", "FIRST_REMOTE_REPLY"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("m2", "AFTER_SECOND_COMPACT_REPLY"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let compacted_history = vec![TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "REMOTE_COMPACTED_SUMMARY".to_string(),
        }],
        end_turn: None,
    }];
    responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history.clone() }),
    )
    .await;
    responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history.clone() }),
    )
    .await;

    codex
        .submit(Op::UserTurn {
            items: vec![
                UserInput::Text {
                    text: "please use $demo".to_string(),
                    text_elements: Vec::new(),
                },
                UserInput::Skill {
                    name: "demo".to_string(),
                    path: skill_path.clone(),
                },
            ],
            final_output_json_schema: None,
            cwd: harness.test().cwd_path().to_path_buf(),
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: harness.test().session_configured.model.clone(),
            effort: None,
            summary: ReasoningSummary::Auto,
            identity: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let follow_up_body = responses_mock
        .requests()
        .last()
        .expect("follow-up request missing")
        .body_json()
        .to_string();
    let compact_requests = harness
        .server()
        .received_requests()
        .await
        .expect("received requests");
    let compact_bodies = compact_requests
        .iter()
        .filter(|request| request.url.path() == "/v1/responses/compact")
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect::<Vec<_>>();
    let second_compact_body = compact_bodies
        .last()
        .expect("second compact request missing");
    let synthetic_skill_prefix = "<skill source=\\\"compact_backfill\\\">\\n<name>demo</name>";

    assert!(!second_compact_body.contains(synthetic_skill_prefix));
    assert!(!second_compact_body.contains(skill_path.to_string_lossy().as_ref()));
    assert!(!second_compact_body.contains(skill_body));

    assert!(follow_up_body.contains(synthetic_skill_prefix));
    assert!(follow_up_body.contains(skill_path.to_string_lossy().as_ref()));
    assert!(follow_up_body.contains(skill_body));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_compact_backfills_latest_unfinished_update_plan() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::with_builder(
        test_codex()
            .with_auth(CodexAuth::from_api_key("Test API Key"))
            .with_config(|config| {
                config.features.enable(Feature::RemoteCompaction);
            }),
    )
    .await?;
    let codex = harness.test().codex.clone();

    let update_plan_args = serde_json::json!({
        "explanation": "Keep going",
        "plan": [
            { "step": "Inspect compact flow", "status": "completed" },
            { "step": "Backfill checklist", "status": "in_progress" }
        ]
    })
    .to_string();
    let responses_mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_function_call("plan-call-1", "update_plan", &update_plan_args),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("m2", "AFTER_COMPACT_REPLY"),
                responses::ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let compacted_history = vec![TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "REMOTE_COMPACTED_SUMMARY".to_string(),
        }],
        end_turn: None,
    }];
    responses::mount_compact_json_once(
        harness.server(),
        serde_json::json!({ "output": compacted_history }),
    )
    .await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "please track progress".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex.submit(Op::Compact).await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after compact".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let follow_up_body = responses_mock
        .requests()
        .last()
        .expect("follow-up request missing")
        .body_json()
        .to_string();
    assert!(
        follow_up_body.contains("\"name\":\"update_plan\""),
        "expected remote compacted history to backfill update_plan call"
    );
    assert!(
        follow_up_body.contains(&serde_json::to_string(&update_plan_args)?),
        "expected remote compacted history to preserve update_plan arguments"
    );
    assert!(
        follow_up_body.contains("\"call_id\":\"compact_backfill_update_plan\""),
        "expected remote compacted history to include the synthetic update_plan output"
    );

    Ok(())
}
