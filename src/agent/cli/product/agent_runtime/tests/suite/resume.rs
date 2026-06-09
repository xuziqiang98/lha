use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::Op;
use crate::product::protocol::config_types::Identity;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::config_types::Settings;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::user_input::ByteRange;
use crate::product::protocol::user_input::TextElement;
use crate::product::protocol::user_input::UserInput;
use crate::test_support::core::responses::ev_assistant_message;
use crate::test_support::core::responses::ev_completed;
use crate::test_support::core::responses::ev_message_item_added;
use crate::test_support::core::responses::ev_output_text_delta;
use crate::test_support::core::responses::ev_reasoning_item;
use crate::test_support::core::responses::ev_response_created;
use crate::test_support::core::responses::mount_sse_once;
use crate::test_support::core::responses::sse;
use crate::test_support::core::responses::start_mock_server;
use crate::test_support::core::skip_if_no_network;
use crate::test_support::core::test_codex::test_codex;
use crate::test_support::core::wait_for_event;
use anyhow::Result;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_includes_initial_messages_from_rollout_events() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let initial = builder.build(&server).await?;
    let codex = Arc::clone(&initial.codex);
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    let initial_sse = sse(vec![
        ev_response_created("resp-initial"),
        ev_assistant_message("msg-1", "Completed first turn"),
        ev_completed("resp-initial"),
    ]);
    mount_sse_once(&server, initial_sse).await;

    let text_elements = vec![TextElement::new(
        ByteRange { start: 0, end: 6 },
        Some("<note>".into()),
    )];

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Record some messages".into(),
                text_elements: text_elements.clone(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let resumed = builder.resume(&server, home, rollout_path).await?;
    let initial_messages = resumed
        .session_configured
        .initial_messages
        .expect("expected initial messages to be present for resumed session");
    match initial_messages.as_slice() {
        [
            EventMsg::UserMessage(first_user),
            EventMsg::TokenCount(_),
            EventMsg::AgentMessage(assistant_message),
            EventMsg::TokenCount(_),
        ] => {
            assert_eq!(first_user.message, "Record some messages");
            assert_eq!(first_user.text_elements, text_elements);
            assert_eq!(assistant_message.message, "Completed first turn");
        }
        other => panic!("unexpected initial messages after resume: {other:#?}"),
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_includes_streamed_plan_mode_agent_message_from_rollout() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let initial = builder.build(&server).await?;
    let codex = Arc::clone(&initial.codex);
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    let plan_block = "<proposed_plan>\n- Step 1\n</proposed_plan>\n";
    let full_message = format!("Intro\n{plan_block}Outro");
    let initial_sse = sse(vec![
        ev_response_created("resp-plan"),
        ev_message_item_added("msg-plan", ""),
        ev_output_text_delta(&full_message),
        ev_assistant_message("msg-plan", &full_message),
        ev_completed("resp-plan"),
    ]);
    mount_sse_once(&server, initial_sse).await;

    let identity = Identity {
        kind: IdentityKind::Planner,
        settings: Settings {
            model: initial.session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };
    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "Plan with visible intro".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: std::env::current_dir()?,
            approval_policy: crate::product::agent::protocol::AskForApproval::Never,
            sandbox_policy: crate::product::agent::protocol::SandboxPolicy::DangerFullAccess,
            model: initial.session_configured.model.clone(),
            effort: None,
            summary: crate::product::protocol::config_types::ReasoningSummary::Auto,
            identity: Some(identity),
            personality: None,
            tui_buddy: None,
        })
        .await?;

    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let mut resume_builder = test_codex();
    let resumed = resume_builder.resume(&server, home, rollout_path).await?;
    let initial_messages = resumed
        .session_configured
        .initial_messages
        .expect("expected initial messages to be present for resumed session");
    let persisted_agent_messages = initial_messages
        .iter()
        .filter_map(|event| match event {
            EventMsg::AgentMessage(message) => Some(message.message.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let persisted_plan_items = initial_messages
        .iter()
        .filter_map(|event| match event {
            EventMsg::ItemCompleted(completed) => match &completed.item {
                TurnItem::Plan(plan) => Some(plan.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(persisted_agent_messages, vec!["Intro\nOutro"]);
    assert_eq!(persisted_plan_items, vec!["- Step 1\n"]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_includes_initial_messages_from_reasoning_events() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.show_raw_agent_reasoning = true;
    });
    let initial = builder.build(&server).await?;
    let codex = Arc::clone(&initial.codex);
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    let initial_sse = sse(vec![
        ev_response_created("resp-initial"),
        ev_reasoning_item("reason-1", &["Summarized step"], &["raw detail"]),
        ev_assistant_message("msg-1", "Completed reasoning turn"),
        ev_completed("resp-initial"),
    ]);
    mount_sse_once(&server, initial_sse).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Record reasoning messages".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let resumed = builder.resume(&server, home, rollout_path).await?;
    let initial_messages = resumed
        .session_configured
        .initial_messages
        .expect("expected initial messages to be present for resumed session");
    match initial_messages.as_slice() {
        [
            EventMsg::UserMessage(first_user),
            EventMsg::TokenCount(_),
            EventMsg::AgentReasoning(reasoning),
            EventMsg::AgentReasoningRawContent(raw),
            EventMsg::AgentMessage(assistant_message),
            EventMsg::TokenCount(_),
        ] => {
            assert_eq!(first_user.message, "Record reasoning messages");
            assert_eq!(reasoning.text, "Summarized step");
            assert_eq!(raw.text, "raw detail");
            assert_eq!(assistant_message.message, "Completed reasoning turn");
        }
        other => panic!("unexpected initial messages after resume: {other:#?}"),
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_switches_models_preserves_base_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.model = Some("gpt-5.2".to_string());
    });
    let initial = builder.build(&server).await?;
    let codex = Arc::clone(&initial.codex);
    let home = initial.home.clone();
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    let initial_sse = sse(vec![
        ev_response_created("resp-initial"),
        ev_assistant_message("msg-1", "Completed first turn"),
        ev_completed("resp-initial"),
    ]);
    let initial_mock = mount_sse_once(&server, initial_sse).await;

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Record initial instructions".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let initial_body = initial_mock.single_request().body_json();
    let initial_instructions = initial_body
        .get("instructions")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let resumed_sse = sse(vec![
        ev_response_created("resp-resume"),
        ev_assistant_message("msg-2", "Resumed turn"),
        ev_completed("resp-resume"),
    ]);
    let resumed_mock = mount_sse_once(&server, resumed_sse).await;

    let mut resume_builder = test_codex().with_config(|config| {
        config.model = Some("gpt-5.2-codex".to_string());
    });
    let resumed = resume_builder.resume(&server, home, rollout_path).await?;
    resumed
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Resume with different model".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let resumed_body = resumed_mock.single_request().body_json();
    let resumed_instructions = resumed_body
        .get("instructions")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert_eq!(resumed_instructions, initial_instructions);

    Ok(())
}
