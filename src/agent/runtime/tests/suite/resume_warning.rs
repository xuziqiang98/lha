#![allow(clippy::unwrap_used, clippy::expect_used)]

use core::time::Duration;
use core_test_support::load_default_config_for_test;
use core_test_support::wait_for_event;
use lha_agent::AuthManager;
use lha_agent::CodexAuth;
use lha_agent::NewThread;
use lha_agent::ThreadManager;
use lha_agent::protocol::EventMsg;
use lha_agent::protocol::InitialHistory;
use lha_agent::protocol::Op;
use lha_agent::protocol::ResumedHistory;
use lha_agent::protocol::RolloutItem;
use lha_agent::protocol::RolloutLine;
use lha_agent::protocol::TurnContextItem;
use lha_agent::protocol::WarningEvent;
use lha_protocol::ThreadId;
use lha_protocol::config_types::Identity;
use lha_protocol::config_types::IdentityKind;
use lha_protocol::config_types::Settings;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;

fn identity(kind: IdentityKind, model: &str, developer_instructions: Option<&str>) -> Identity {
    Identity {
        kind,
        settings: Settings {
            model: model.to_string(),
            reasoning_effort: None,
            developer_instructions: developer_instructions.map(str::to_string),
        },
    }
}

fn turn_context(
    config: &lha_agent::config::Config,
    model: &str,
    identity: Option<Identity>,
) -> TurnContextItem {
    TurnContextItem {
        cwd: config.cwd.clone(),
        approval_policy: config.approval_policy.value(),
        sandbox_policy: config.sandbox_policy.get().clone(),
        model: model.to_string(),
        personality: None,
        identity,
        effort: config.model_reasoning_effort,
        summary: config.model_reasoning_summary,
        user_instructions: None,
        developer_instructions: None,
        final_output_json_schema: None,
        truncation_policy: None,
    }
}

fn resumed_history(rollout_path: &Path, history: Vec<RolloutItem>) -> InitialHistory {
    InitialHistory::Resumed(ResumedHistory {
        conversation_id: ThreadId::default(),
        history,
        rollout_path: rollout_path.to_path_buf(),
    })
}

fn resume_history(
    config: &lha_agent::config::Config,
    previous_model: &str,
    rollout_path: &Path,
) -> InitialHistory {
    resumed_history(
        rollout_path,
        vec![RolloutItem::TurnContext(turn_context(
            config,
            previous_model,
            None,
        ))],
    )
}

async fn read_rollout_items(path: &Path) -> Vec<RolloutItem> {
    let text = tokio::fs::read_to_string(path).await.expect("read rollout");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<RolloutLine>(line)
                .expect("parse rollout line")
                .item
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emits_warning_when_resumed_model_differs() {
    // Arrange a config with a current model and a prior rollout recorded under a different model.
    let home = TempDir::new().expect("tempdir");
    let mut config = load_default_config_for_test(&home).await;
    config.model = Some("current-model".to_string());
    // Ensure cwd is absolute (the helper sets it to the temp dir already).
    assert!(config.cwd.is_absolute());

    let rollout_path = home.path().join("rollout.jsonl");
    std::fs::write(&rollout_path, "").expect("create rollout placeholder");

    let initial_history = resume_history(&config, "previous-model", &rollout_path);

    let thread_manager = ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        config.model_provider.clone(),
    );
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));

    // Act: resume the conversation.
    let NewThread {
        thread: conversation,
        ..
    } = thread_manager
        .resume_thread_with_history(config, initial_history, auth_manager)
        .await
        .expect("resume conversation");

    // Assert: a Warning event is emitted describing the model mismatch.
    let warning = wait_for_event(&conversation, |ev| matches!(ev, EventMsg::Warning(_))).await;
    let EventMsg::Warning(WarningEvent { message }) = warning else {
        panic!("expected warning event");
    };
    assert!(message.contains("previous-model"));
    assert!(message.contains("current-model"));

    // Drain the TurnComplete/Shutdown window to avoid leaking tasks between tests.
    // The warning is emitted during initialization, so a short sleep is sufficient.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_restores_latest_identity_and_preserves_current_model() {
    let home = TempDir::new().expect("tempdir");
    let mut config = load_default_config_for_test(&home).await;
    config.model = Some("current-model".to_string());

    let rollout_path = home.path().join("rollout.jsonl");
    std::fs::write(&rollout_path, "").expect("create rollout placeholder");
    let initial_history = resumed_history(
        &rollout_path,
        vec![
            RolloutItem::TurnContext(turn_context(
                &config,
                "previous-model",
                Some(identity(
                    IdentityKind::Planner,
                    "previous-model",
                    Some("planner instructions"),
                )),
            )),
            RolloutItem::TurnContext(turn_context(
                &config,
                "previous-model",
                Some(identity(
                    IdentityKind::Programmer,
                    "previous-model",
                    Some("programmer instructions"),
                )),
            )),
        ],
    );

    let thread_manager = ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        config.model_provider.clone(),
    );
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));

    let NewThread {
        thread,
        session_configured,
        ..
    } = thread_manager
        .resume_thread_with_history(config, initial_history, auth_manager)
        .await
        .expect("resume conversation");

    assert_eq!(session_configured.identity_kind, IdentityKind::Programmer);
    assert_eq!(session_configured.model, "current-model");

    thread.submit(Op::Shutdown).await.expect("submit shutdown");
    thread
        .wait_for_shutdown_complete()
        .await
        .expect("wait for shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_restores_latest_identity() {
    let home = TempDir::new().expect("tempdir");
    let config = load_default_config_for_test(&home).await;
    let initial_history = InitialHistory::Forked(vec![RolloutItem::TurnContext(turn_context(
        &config,
        "previous-model",
        Some(identity(
            IdentityKind::Planner,
            "previous-model",
            Some("planner instructions"),
        )),
    ))]);

    let thread_manager = ThreadManager::with_models_provider(
        CodexAuth::from_api_key("test"),
        config.model_provider.clone(),
    );
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("test"));

    let NewThread {
        thread,
        session_configured,
        ..
    } = thread_manager
        .resume_thread_with_history(config, initial_history, auth_manager)
        .await
        .expect("fork conversation");

    assert_eq!(session_configured.identity_kind, IdentityKind::Planner);

    thread.submit(Op::Shutdown).await.expect("submit shutdown");
    thread
        .wait_for_shutdown_complete()
        .await
        .expect("wait for shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_identity_without_turn_is_persisted_and_restored() {
    let home = TempDir::new().expect("tempdir");
    let mut config = load_default_config_for_test(&home).await;
    config.model = Some("current-model".to_string());
    let auth = CodexAuth::from_api_key("test");
    let thread_manager = ThreadManager::with_models_provider_and_home(
        auth.clone(),
        config.model_provider_id.as_str(),
        config.model_provider.clone(),
        config.lha_home.clone(),
    );
    let auth_manager = AuthManager::from_auth_for_testing(auth.clone());

    let NewThread {
        thread,
        session_configured,
        ..
    } = thread_manager
        .start_thread(config.clone())
        .await
        .expect("start thread");
    let rollout_path = session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    thread
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            identity: Some(identity(
                IdentityKind::Programmer,
                "current-model",
                Some("programmer instructions"),
            )),
            personality: None,
        })
        .await
        .expect("submit identity override");
    thread.submit(Op::Shutdown).await.expect("submit shutdown");
    thread
        .wait_for_shutdown_complete()
        .await
        .expect("wait for shutdown");

    let items = read_rollout_items(&rollout_path).await;
    let latest_identity = items.iter().rev().find_map(|item| match item {
        RolloutItem::TurnContext(turn_context) => turn_context.identity.as_ref(),
        RolloutItem::SessionMeta(_)
        | RolloutItem::TranscriptItem(_)
        | RolloutItem::GhostSnapshot(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::Workflow(_)
        | RolloutItem::EventMsg(_) => None,
    });
    assert_eq!(
        latest_identity.map(|identity| identity.kind),
        Some(IdentityKind::Programmer)
    );

    let NewThread {
        thread: resumed_thread,
        session_configured,
        ..
    } = thread_manager
        .resume_thread_from_rollout(config, rollout_path, auth_manager)
        .await
        .expect("resume thread");
    assert_eq!(session_configured.identity_kind, IdentityKind::Programmer);

    resumed_thread
        .submit(Op::Shutdown)
        .await
        .expect("submit resumed shutdown");
    resumed_thread
        .wait_for_shutdown_complete()
        .await
        .expect("wait for resumed shutdown");
}
