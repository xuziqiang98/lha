use anyhow::Result;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use lha_agent::protocol::EventMsg;
use lha_agent::protocol::IDENTITY_CLOSE_TAG;
use lha_agent::protocol::IDENTITY_OPEN_TAG;
use lha_agent::protocol::Op;
use lha_protocol::config_types::Identity;
use lha_protocol::config_types::IdentityKind;
use lha_protocol::config_types::Settings;
use lha_protocol::user_input::UserInput;
use pretty_assertions::assert_eq;
use serde_json::Value;

fn sse_completed(id: &str) -> String {
    sse(vec![ev_response_created(id), ev_completed(id)])
}

fn identity_with_kind_and_instructions(kind: IdentityKind, instructions: Option<&str>) -> Identity {
    Identity {
        kind,
        settings: Settings {
            model: "gpt-5.1".to_string(),
            reasoning_effort: None,
            developer_instructions: instructions.map(str::to_string),
        },
    }
}

fn identity_with_instructions(instructions: Option<&str>) -> Identity {
    identity_with_kind_and_instructions(IdentityKind::Nobody, instructions)
}

const IDENTITY_CLEARED_MARKER: &str = "The current session has no active preset identity.";

fn developer_texts(input: &[Value]) -> Vec<String> {
    input
        .iter()
        .filter_map(|item| {
            let role = item.get("role")?.as_str()?;
            if role != "developer" {
                return None;
            }
            let text = item
                .get("content")?
                .as_array()?
                .first()?
                .get("text")?
                .as_str()?;
            Some(text.to_string())
        })
        .collect()
}

fn identity_xml(text: &str) -> String {
    format!("{IDENTITY_OPEN_TAG}{text}{IDENTITY_CLOSE_TAG}")
}

fn count_exact(texts: &[String], target: &str) -> usize {
    texts.iter().filter(|text| text.as_str() == target).count()
}

fn position_exact(texts: &[String], target: &str) -> Option<usize> {
    texts.iter().position(|text| text == target)
}

fn position_containing(texts: &[String], needle: &str) -> Option<usize> {
    texts.iter().position(|text| text.contains(needle))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_identity_instructions_by_default() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(&server, sse_completed("resp-1")).await;

    let test = test_codex().build(&server).await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req.single_request().input();
    let dev_texts = developer_texts(&input);
    assert_eq!(dev_texts.len(), 1);
    assert!(dev_texts[0].contains("<permissions instructions>"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_input_includes_identity_instructions_after_override() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(&server, sse_completed("resp-1")).await;

    let test = test_codex().build(&server).await?;

    let identity_text = "identity instructions";
    let identity = identity_with_instructions(Some(identity_text));
    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req.single_request().input();
    let dev_texts = developer_texts(&input);
    let identity_text = identity_xml(identity_text);
    assert_eq!(count_exact(&dev_texts, &identity_text), 1);
    assert!(
        position_containing(&dev_texts, IDENTITY_CLEARED_MARKER).is_none(),
        "did not expect identity clear when resumed Nobody identity has custom instructions"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_instructions_added_on_user_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(&server, sse_completed("resp-1")).await;

    let test = test_codex().build(&server).await?;
    let identity_text = "turn instructions";
    let identity = identity_with_instructions(Some(identity_text));

    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            cwd: test.config.cwd.clone(),
            approval_policy: test.config.approval_policy.value(),
            sandbox_policy: test.config.sandbox_policy.get().clone(),
            model: test.session_configured.model.clone(),
            effort: None,
            summary: test.config.model_reasoning_summary,
            identity: Some(identity),
            final_output_json_schema: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req.single_request().input();
    let dev_texts = developer_texts(&input);
    let identity_text = identity_xml(identity_text);
    assert_eq!(count_exact(&dev_texts, &identity_text), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_then_next_turn_uses_updated_identity_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(&server, sse_completed("resp-1")).await;

    let test = test_codex().build(&server).await?;
    let identity_text = "override instructions";
    let identity = identity_with_instructions(Some(identity_text));

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req.single_request().input();
    let dev_texts = developer_texts(&input);
    let identity_text = identity_xml(identity_text);
    assert_eq!(count_exact(&dev_texts, &identity_text), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_turn_overrides_identity_instructions_after_override() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(&server, sse_completed("resp-1")).await;

    let test = test_codex().build(&server).await?;
    let base_text = "base instructions";
    let base_mode = identity_with_instructions(Some(base_text));
    let turn_text = "turn override";
    let turn_mode = identity_with_instructions(Some(turn_text));

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(base_mode),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            cwd: test.config.cwd.clone(),
            approval_policy: test.config.approval_policy.value(),
            sandbox_policy: test.config.sandbox_policy.get().clone(),
            model: test.session_configured.model.clone(),
            effort: None,
            summary: test.config.model_reasoning_summary,
            identity: Some(turn_mode),
            final_output_json_schema: None,
            personality: None,
            tui_buddy: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req.single_request().input();
    let dev_texts = developer_texts(&input);
    let base_text = identity_xml(base_text);
    let turn_text = identity_xml(turn_text);
    assert_eq!(count_exact(&dev_texts, &base_text), 0);
    assert_eq!(count_exact(&dev_texts, &turn_text), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_update_emits_new_instruction_message() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(&server, sse_completed("resp-1")).await;
    let req2 = mount_sse_once(&server, sse_completed("resp-2")).await;

    let test = test_codex().build(&server).await?;
    let first_text = "first instructions";
    let second_text = "second instructions";

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_instructions(Some(first_text))),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 1".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_instructions(Some(second_text))),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req2.single_request().input();
    let dev_texts = developer_texts(&input);
    let first_text = identity_xml(first_text);
    let second_text = identity_xml(second_text);
    assert_eq!(count_exact(&dev_texts, &first_text), 1);
    assert_eq!(count_exact(&dev_texts, &second_text), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_update_noop_does_not_append() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(&server, sse_completed("resp-1")).await;
    let req2 = mount_sse_once(&server, sse_completed("resp-2")).await;

    let test = test_codex().build(&server).await?;
    let identity_text = "same instructions";

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_instructions(Some(identity_text))),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 1".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_instructions(Some(identity_text))),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req2.single_request().input();
    let dev_texts = developer_texts(&input);
    let identity_text = identity_xml(identity_text);
    assert_eq!(count_exact(&dev_texts, &identity_text), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_update_emits_new_instruction_message_when_mode_changes() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(&server, sse_completed("resp-1")).await;
    let req2 = mount_sse_once(&server, sse_completed("resp-2")).await;

    let test = test_codex().build(&server).await?;
    let code_text = "code mode instructions";
    let plan_text = "plan mode instructions";

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Programmer,
                Some(code_text),
            )),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 1".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Planner,
                Some(plan_text),
            )),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req2.single_request().input();
    let dev_texts = developer_texts(&input);
    let code_text = identity_xml(code_text);
    let plan_text = identity_xml(plan_text);
    assert_eq!(count_exact(&dev_texts, &code_text), 1);
    assert_eq!(count_exact(&dev_texts, &plan_text), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_update_clears_previous_instructions_when_switching_to_nobody() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(&server, sse_completed("resp-1")).await;
    let req2 = mount_sse_once(&server, sse_completed("resp-2")).await;

    let test = test_codex().build(&server).await?;
    let programmer_text = "programmer instructions";

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Programmer,
                Some(programmer_text),
            )),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 1".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Nobody,
                None,
            )),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req2.single_request().input();
    let dev_texts = developer_texts(&input);
    let programmer_text = identity_xml(programmer_text);
    let programmer_pos =
        position_exact(&dev_texts, &programmer_text).expect("programmer identity instructions");
    let clear_pos =
        position_containing(&dev_texts, IDENTITY_CLEARED_MARKER).expect("identity clear message");
    assert_eq!(count_exact(&dev_texts, &programmer_text), 1);
    assert!(
        programmer_pos < clear_pos,
        "expected clear message after stale identity, got {dev_texts:?}"
    );
    assert!(dev_texts[clear_pos].starts_with(IDENTITY_OPEN_TAG));
    assert!(dev_texts[clear_pos].ends_with(IDENTITY_CLOSE_TAG));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_update_noop_does_not_append_when_mode_is_unchanged() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(&server, sse_completed("resp-1")).await;
    let req2 = mount_sse_once(&server, sse_completed("resp-2")).await;

    let test = test_codex().build(&server).await?;
    let identity_text = "mode-stable instructions";

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Programmer,
                Some(identity_text),
            )),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 1".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Programmer,
                Some(identity_text),
            )),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req2.single_request().input();
    let dev_texts = developer_texts(&input);
    let identity_text = identity_xml(identity_text);
    assert_eq!(count_exact(&dev_texts, &identity_text), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_replays_identity_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(&server, sse_completed("resp-1")).await;
    let req2 = mount_sse_once(&server, sse_completed("resp-2")).await;

    let mut builder = test_codex();
    let initial = builder.build(&server).await?;
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");
    let home = initial.home.clone();

    let identity_text = "resume instructions";
    initial
        .codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_instructions(Some(identity_text))),
            personality: None,
        })
        .await?;

    initial
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let resumed = builder.resume(&server, home, rollout_path).await?;
    resumed
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after resume".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&resumed.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req2.single_request().input();
    let dev_texts = developer_texts(&input);
    let identity_text = identity_xml(identity_text);
    assert_eq!(count_exact(&dev_texts, &identity_text), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_with_stale_identity_and_nobody_override_clears_on_first_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(&server, sse_completed("resp-1")).await;
    let req2 = mount_sse_once(&server, sse_completed("resp-2")).await;

    let mut builder = test_codex();
    let initial = builder.build(&server).await?;
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");
    let home = initial.home.clone();

    let programmer_text = "resume programmer instructions";
    initial
        .codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Programmer,
                Some(programmer_text),
            )),
            personality: None,
        })
        .await?;

    initial
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let resumed = builder.resume(&server, home, rollout_path).await?;
    resumed
        .codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Nobody,
                None,
            )),
            personality: None,
        })
        .await?;

    resumed
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after resume".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&resumed.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req2.single_request().input();
    let dev_texts = developer_texts(&input);
    let programmer_text = identity_xml(programmer_text);
    let programmer_pos =
        position_exact(&dev_texts, &programmer_text).expect("programmer identity instructions");
    let clear_pos =
        position_containing(&dev_texts, IDENTITY_CLEARED_MARKER).expect("identity clear message");
    assert!(
        programmer_pos < clear_pos,
        "expected clear message after stale identity, got {dev_texts:?}"
    );
    assert!(dev_texts[clear_pos].starts_with(IDENTITY_OPEN_TAG));
    assert!(dev_texts[clear_pos].ends_with(IDENTITY_CLOSE_TAG));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_with_custom_nobody_identity_does_not_clear_current_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(&server, sse_completed("resp-1")).await;
    let _req2 = mount_sse_once(&server, sse_completed("resp-2")).await;
    let req3 = mount_sse_once(&server, sse_completed("resp-3")).await;

    let mut builder = test_codex();
    let initial = builder.build(&server).await?;
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");
    let home = initial.home.clone();

    let programmer_text = "old programmer instructions";
    initial
        .codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Programmer,
                Some(programmer_text),
            )),
            personality: None,
        })
        .await?;

    initial
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 1".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let custom_text = "custom nobody instructions";
    initial
        .codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_kind_and_instructions(
                IdentityKind::Nobody,
                Some(custom_text),
            )),
            personality: None,
        })
        .await?;

    initial
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello 2".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let resumed = builder.resume(&server, home, rollout_path).await?;
    resumed
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "after resume".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&resumed.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req3.single_request().input();
    let dev_texts = developer_texts(&input);
    let programmer_text = identity_xml(programmer_text);
    let custom_text = identity_xml(custom_text);
    assert_eq!(count_exact(&dev_texts, &programmer_text), 1);
    assert_eq!(count_exact(&dev_texts, &custom_text), 1);
    assert!(
        position_containing(&dev_texts, IDENTITY_CLEARED_MARKER).is_none(),
        "did not expect identity clear when current Nobody identity has custom instructions"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_identity_instructions_are_ignored() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(&server, sse_completed("resp-1")).await;

    let test = test_codex().build(&server).await?;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity_with_instructions(Some(""))),
            personality: None,
        })
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let input = req.single_request().input();
    let dev_texts = developer_texts(&input);
    assert_eq!(dev_texts.len(), 1);
    let identity_text = identity_xml("");
    assert_eq!(count_exact(&dev_texts, &identity_text), 0);

    Ok(())
}
