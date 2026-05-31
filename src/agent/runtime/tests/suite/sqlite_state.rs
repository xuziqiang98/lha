use anyhow::Result;
use anyhow::anyhow;
use core_test_support::load_sse_fixture_with_id;
use core_test_support::responses;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use lha_agent::features::Feature;
use lha_protocol::ThreadId;
use lha_protocol::protocol::EventMsg;
use lha_protocol::protocol::RolloutItem;
use lha_protocol::protocol::RolloutLine;
use lha_protocol::protocol::SessionMeta;
use lha_protocol::protocol::SessionMetaLine;
use lha_protocol::protocol::SessionSource;
use lha_protocol::protocol::UserMessageEvent;
use lha_state::STATE_DB_FILENAME;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::fs;
use std::path::Path;
use tokio::time::Duration;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

fn sse_completed(id: &str) -> String {
    load_sse_fixture_with_id("../fixtures/completed_template.json", id)
}

fn write_rollout_with_schema_version(
    lha_home: &Path,
    uuid: Uuid,
    schema_version: Option<u32>,
) -> Result<std::path::PathBuf> {
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = lha_home.join(format!(
        "sessions/2026/01/27/rollout-2026-01-27T12-00-00-{uuid}.jsonl"
    ));
    let parent = rollout_path
        .parent()
        .ok_or_else(|| anyhow!("rollout path should have parent"))?;
    fs::create_dir_all(parent)?;

    let mut payload = serde_json::to_value(SessionMetaLine {
        meta: SessionMeta {
            id: thread_id,
            forked_from_id: None,
            timestamp: "2026-01-27T12:00:00Z".to_string(),
            cwd: lha_home.to_path_buf(),
            originator: "test".to_string(),
            cli_version: "test".to_string(),
            rollout_schema_version: schema_version
                .unwrap_or(lha_protocol::protocol::ROLLOUT_SCHEMA_VERSION_V3),
            source: SessionSource::default(),
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
        },
        git: None,
    })?;
    if schema_version.is_none()
        && let Some(payload) = payload.as_object_mut()
    {
        payload.remove("rollout_schema_version");
    }

    let lines = [
        json!({
            "timestamp": "2026-01-27T12:00:00Z",
            "type": "session_meta",
            "payload": payload,
        })
        .to_string(),
        json!({
            "timestamp": "2026-01-27T12:00:01Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "hello from backfill",
                "kind": "plain",
            },
        })
        .to_string(),
    ];

    fs::write(&rollout_path, format!("{}\n", lines.join("\n")))?;
    Ok(rollout_path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_thread_is_recorded_in_state_db() -> Result<()> {
    core_test_support::skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Sqlite);
    });
    let test = builder.build(&server).await?;

    let thread_id = test.session_configured.session_id;
    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let db_path = test.config.lha_home.join(STATE_DB_FILENAME);

    for _ in 0..100 {
        if tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let db = test.codex.state_db().expect("state db enabled");

    let mut metadata = None;
    for _ in 0..100 {
        metadata = db.get_thread(thread_id).await?;
        if metadata.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let metadata = metadata.expect("thread should exist in state db");
    assert_eq!(metadata.id, thread_id);
    assert_eq!(metadata.rollout_path, rollout_path);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_scans_existing_rollouts() -> Result<()> {
    core_test_support::skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;

    let uuid = Uuid::now_v7();
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_rel_path = format!("sessions/2026/01/27/rollout-2026-01-27T12-00-00-{uuid}.jsonl");
    let rollout_rel_path_for_hook = rollout_rel_path.clone();

    let mut builder = test_codex()
        .with_pre_build_hook(move |lha_home| {
            let rollout_path = lha_home.join(&rollout_rel_path_for_hook);
            let parent = rollout_path
                .parent()
                .expect("rollout path should have parent");
            fs::create_dir_all(parent).expect("should create rollout directory");

            let session_meta_line = SessionMetaLine {
                meta: SessionMeta {
                    id: thread_id,
                    forked_from_id: None,
                    timestamp: "2026-01-27T12:00:00Z".to_string(),
                    cwd: lha_home.to_path_buf(),
                    originator: "test".to_string(),
                    cli_version: "test".to_string(),
                    rollout_schema_version: lha_protocol::protocol::ROLLOUT_SCHEMA_VERSION_V3,
                    source: SessionSource::default(),
                    model_provider: None,
                    base_instructions: None,
                    dynamic_tools: None,
                },
                git: None,
            };

            let lines = [
                RolloutLine {
                    timestamp: "2026-01-27T12:00:00Z".to_string(),
                    item: RolloutItem::SessionMeta(session_meta_line),
                },
                RolloutLine {
                    timestamp: "2026-01-27T12:00:01Z".to_string(),
                    item: RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                        message: "hello from backfill".to_string(),
                        images: None,
                        local_images: Vec::new(),
                        text_elements: Vec::new(),
                    })),
                },
            ];

            let jsonl = lines
                .iter()
                .map(|line| serde_json::to_string(line).expect("rollout line should serialize"))
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(&rollout_path, format!("{jsonl}\n")).expect("should write rollout file");
        })
        .with_config(|config| {
            config.features.enable(Feature::Sqlite);
        });

    let test = builder.build(&server).await?;

    let db_path = test.config.lha_home.join(STATE_DB_FILENAME);
    let rollout_path = test.config.lha_home.join(&rollout_rel_path);
    let default_provider = test.config.model_provider_id.clone();

    for _ in 0..20 {
        if tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let db = test.codex.state_db().expect("state db enabled");

    let mut metadata = None;
    for _ in 0..40 {
        metadata = db.get_thread(thread_id).await?;
        if metadata.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let metadata = metadata.expect("backfilled thread should exist in state db");
    assert_eq!(metadata.id, thread_id);
    assert_eq!(metadata.rollout_path, rollout_path);
    assert_eq!(metadata.model_provider, default_provider);
    assert!(metadata.has_user_event);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_skips_unsupported_rollouts() -> Result<()> {
    core_test_support::skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let v2_uuid = Uuid::now_v7();
    let missing_uuid = Uuid::now_v7();
    let v2_thread_id = ThreadId::from_string(&v2_uuid.to_string())?;
    let missing_thread_id = ThreadId::from_string(&missing_uuid.to_string())?;

    let mut builder = test_codex()
        .with_pre_build_hook(move |lha_home| {
            write_rollout_with_schema_version(lha_home, v2_uuid, Some(2))
                .expect("should write v2 rollout");
            write_rollout_with_schema_version(lha_home, missing_uuid, None)
                .expect("should write missing-schema rollout");
        })
        .with_config(|config| {
            config.features.enable(Feature::Sqlite);
        });

    let test = builder.build(&server).await?;
    let db_path = test.config.lha_home.join(STATE_DB_FILENAME);
    for _ in 0..20 {
        if tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let db = test.codex.state_db().expect("state db enabled");
    tokio::time::sleep(Duration::from_millis(250)).await;

    assert_eq!(db.get_thread(v2_thread_id).await?, None);
    assert_eq!(db.get_thread(missing_thread_id).await?, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_skips_unsupported_rollout() -> Result<()> {
    core_test_support::skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let uuid = Uuid::now_v7();
    let thread_id = ThreadId::from_string(&uuid.to_string())?;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Sqlite);
    });
    let test = builder.build(&server).await?;
    let db = test.codex.state_db().expect("state db enabled");
    let rollout_path = write_rollout_with_schema_version(&test.config.lha_home, uuid, Some(2))?;

    lha_agent::state_db::reconcile_rollout(
        Some(db.as_ref()),
        &rollout_path,
        test.config.model_provider_id.as_str(),
        None,
        &[],
    )
    .await;

    assert_eq!(db.get_thread(thread_id).await?, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_messages_persist_in_state_db() -> Result<()> {
    core_test_support::skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Sqlite);
    });
    let test = builder.build(&server).await?;

    let db_path = test.config.lha_home.join(STATE_DB_FILENAME);
    for _ in 0..100 {
        if tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    test.submit_turn("hello from sqlite").await?;
    test.submit_turn("another message").await?;

    let db = test.codex.state_db().expect("state db enabled");
    let thread_id = test.session_configured.session_id;

    let mut metadata = None;
    for _ in 0..100 {
        metadata = db.get_thread(thread_id).await?;
        if metadata
            .as_ref()
            .map(|entry| entry.has_user_event)
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let metadata = metadata.expect("thread should exist in state db");
    assert!(metadata.has_user_event);

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_logs_include_thread_id() -> Result<()> {
    core_test_support::skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let call_id = "call-1";
    let args = json!({
        "command": "echo hello",
        "timeout_ms": 1_000,
        "login": false,
    });
    let args_json = serde_json::to_string(&args)?;
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(call_id, "shell_command", &args_json),
                ev_completed("resp-1"),
            ]),
            responses::sse(vec![ev_completed("resp-2")]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::Sqlite);
    });
    let test = builder.build(&server).await?;
    let db = test.codex.state_db().expect("state db enabled");
    let expected_thread_id = test.session_configured.session_id.to_string();

    test.submit_turn("run a shell command").await?;
    let subscriber = tracing_subscriber::registry().with(lha_state::log_db::start(db.clone()));
    let dispatch = tracing::Dispatch::new(subscriber);
    tracing::dispatcher::with_default(&dispatch, || {
        let span = tracing::info_span!("test_log_span", thread_id = %expected_thread_id);
        let _entered = span.enter();
        tracing::info!("ToolCall: shell_command {{\"command\":\"echo hello\"}}");
    });

    let mut found = None;
    for _ in 0..80 {
        let query = lha_state::LogQuery {
            descending: true,
            limit: Some(20),
            ..Default::default()
        };
        let rows = db.query_logs(&query).await?;
        if let Some(row) = rows.into_iter().find(|row| {
            row.message
                .as_deref()
                .is_some_and(|m| m.starts_with("ToolCall:"))
        }) {
            let thread_id = row.thread_id;
            let message = row.message;
            found = Some((thread_id, message));
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let (thread_id, message) = found.expect("expected ToolCall log row");
    assert_eq!(thread_id, Some(expected_thread_id));
    assert!(
        message
            .as_deref()
            .is_some_and(|text| text.starts_with("ToolCall:")),
        "expected ToolCall message, got {message:?}"
    );

    Ok(())
}
