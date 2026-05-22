use anyhow::Result;
use adam_agent::config::Constrained;
use adam_agent::protocol::AskForApproval;
use adam_agent::protocol::IDENTITY_CLOSE_TAG;
use adam_agent::protocol::IDENTITY_OPEN_TAG;
use adam_agent::protocol::EventMsg;
use adam_agent::protocol::Op;
use adam_agent::protocol::RolloutItem;
use adam_agent::protocol::RolloutLine;
use adam_agent::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use adam_protocol::config_types::Identity;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::config_types::Settings;
use adam_protocol::models::ContentItem;
use adam_protocol::models::TranscriptItem;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

fn identity_with_instructions(instructions: Option<&str>) -> Identity {
    Identity {
        kind: IdentityKind::Nobody,
        settings: Settings {
            model: "gpt-5.1".to_string(),
            reasoning_effort: None,
            developer_instructions: instructions.map(str::to_string),
        },
    }
}

fn identity_xml(text: &str) -> String {
    format!("{IDENTITY_OPEN_TAG}{text}{IDENTITY_CLOSE_TAG}")
}

async fn read_rollout_text(path: &Path) -> anyhow::Result<String> {
    for _ in 0..50 {
        if path.exists()
            && let Ok(text) = std::fs::read_to_string(path)
            && !text.trim().is_empty()
        {
            return Ok(text);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(std::fs::read_to_string(path)?)
}

fn rollout_developer_texts(text: &str) -> Vec<String> {
    let mut texts = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rollout: RolloutLine = match serde_json::from_str(trimmed) {
            Ok(rollout) => rollout,
            Err(_) => continue,
        };
        if let RolloutItem::TranscriptItem(TranscriptItem::Message(message)) = rollout.item
            && message.role == "developer"
        {
            for item in message.content {
                if let ContentItem::InputText { text } = item {
                    texts.push(text);
                }
            }
        }
    }
    texts
}

fn rollout_environment_texts(text: &str) -> Vec<String> {
    let mut texts = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rollout: RolloutLine = match serde_json::from_str(trimmed) {
            Ok(rollout) => rollout,
            Err(_) => continue,
        };
        if let RolloutItem::TranscriptItem(TranscriptItem::Message(message)) = rollout.item
            && message.role == "user"
        {
            for item in message.content {
                if let ContentItem::InputText { text } = item
                    && text.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG)
                {
                    texts.push(text);
                }
            }
        }
    }
    texts
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_turn_context_without_user_turn_does_not_record_permissions_update() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
    });
    let test = builder.build(&server).await?;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: Some(AskForApproval::Never),
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: None,
            personality: None,
        })
        .await?;

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let rollout_text = read_rollout_text(&rollout_path).await?;
    let developer_texts = rollout_developer_texts(&rollout_text);
    let approval_texts: Vec<&String> = developer_texts
        .iter()
        .filter(|text| text.contains("`approval_policy`"))
        .collect();
    assert!(
        approval_texts.is_empty(),
        "did not expect permissions updates before a new user turn: {approval_texts:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_turn_context_without_user_turn_does_not_record_environment_update() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;
    let new_cwd = TempDir::new()?;

    test.codex
        .submit(Op::OverrideTurnContext {
            cwd: Some(new_cwd.path().to_path_buf()),
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: None,
            personality: None,
        })
        .await?;

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let rollout_text = read_rollout_text(&rollout_path).await?;
    let env_texts = rollout_environment_texts(&rollout_text);
    assert!(
        env_texts.is_empty(),
        "did not expect environment updates before a new user turn: {env_texts:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_turn_context_without_user_turn_does_not_record_identity_update() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = test_codex().build(&server).await?;
    let identity_text = "override identity instructions";
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

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;

    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let rollout_text = read_rollout_text(&rollout_path).await?;
    let developer_texts = rollout_developer_texts(&rollout_text);
    let identity_text = identity_xml(identity_text);
    let identity_count = developer_texts
        .iter()
        .filter(|text| text.as_str() == identity_text.as_str())
        .count();
    assert_eq!(identity_count, 0);

    Ok(())
}
