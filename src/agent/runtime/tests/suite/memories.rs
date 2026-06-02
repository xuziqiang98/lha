use anyhow::Result;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use lha_agent::features::Feature;
use lha_protocol::ThreadId;
use lha_protocol::items::TurnItem;
use lha_protocol::memory_citation::MemoryCitation;
use lha_protocol::memory_citation::MemoryCitationEntry;
use lha_protocol::models::ContentItem;
use lha_protocol::models::TranscriptItem;
use lha_protocol::protocol::EventMsg;
use lha_protocol::protocol::ItemCompletedEvent;
use lha_protocol::protocol::Op;
use lha_protocol::protocol::RolloutItem;
use lha_protocol::protocol::RolloutLine;
use lha_protocol::protocol::SessionMeta;
use lha_protocol::protocol::SessionMetaLine;
use lha_protocol::protocol::SessionSource;
use lha_protocol::protocol::USER_MESSAGE_BEGIN;
use lha_protocol::user_input::UserInput;
use lha_state::Phase2JobClaimOutcome;
use lha_state::Stage1StartupClaimParams;
use lha_state::StateRuntime;
use lha_state::ThreadMetadata;
use pretty_assertions::assert_eq;
use serde_json::json;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tempfile::TempDir;
use tokio::time::Duration;
use uuid::Uuid;

const TEST_PROVIDER: &str = "test-provider";
const TEST_SECRET: &str = "sk-abcdefghijklmnopqrstuvwx";
const JOB_KIND_STAGE1: &str = "memory_stage1";
const JOB_KIND_PHASE2: &str = "memory_consolidate_global";
const PHASE2_JOB_KEY: &str = "global";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_thread_persists_disabled_memory_mode_when_generation_disabled() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.features.disable(Feature::MemoryTool);
        config.memories.generate_memories = false;
    });
    let test = builder.build(&server).await?;

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;

    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let items = read_rollout_items(rollout_path.as_path()).await?;
    let Some(RolloutItem::SessionMeta(SessionMetaLine { meta, .. })) = items.first() else {
        anyhow::bail!("first rollout item should be SessionMeta");
    };

    assert_eq!(
        meta.memory_mode.as_deref(),
        Some(lha_protocol::protocol::MEMORY_MODE_DISABLED)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn assistant_memory_citation_is_persisted_and_replayed() -> Result<()> {
    let server = start_mock_server().await;
    let expected_citation = MemoryCitation {
        entries: vec![MemoryCitationEntry {
            path: "MEMORY.md".into(),
            line_start: 1,
            line_end: 2,
            note: "used preference".into(),
        }],
        rollout_ids: vec!["00000000-0000-0000-0000-000000000001".into()],
    };
    let assistant_message = "answer\n<oai-mem-citation>\n<citation_entries>\nMEMORY.md:1-2|note=[used preference]\n</citation_entries>\n<rollout_ids>\n00000000-0000-0000-0000-000000000001\n</rollout_ids>\n</oai-mem-citation>";
    mount_sse_once(
        &server,
        responses::sse(vec![
            ev_response_created("resp-memory-citation"),
            ev_assistant_message("msg-memory-citation", assistant_message),
            ev_completed("resp-memory-citation"),
        ]),
    )
    .await;
    let mut builder = test_codex().with_config(|config| {
        config.features.enable(Feature::MemoryTool);
    });
    let test = builder.build(&server).await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "answer with memory".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await?;

    let completed = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::AgentMessage(item),
            ..
        }) => Some(item.clone()),
        _ => None,
    })
    .await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    assert_eq!(completed.memory_citation, Some(expected_citation.clone()));
    let text = completed
        .content
        .iter()
        .map(|entry| match entry {
            lha_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect::<String>();
    assert_eq!(text, "answer");

    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;

    let rollout_path = test.codex.rollout_path().expect("rollout path");
    let items = read_rollout_items(rollout_path.as_path()).await?;
    let persisted = items.iter().find_map(|item| match item {
        RolloutItem::EventMsg(EventMsg::AgentMessage(payload)) => Some(payload.clone()),
        RolloutItem::SessionMeta(_)
        | RolloutItem::TranscriptItem(_)
        | RolloutItem::GhostSnapshot(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::Workflow(_)
        | RolloutItem::EventMsg(_) => None,
    });
    let persisted = persisted.expect("persisted agent message event");
    assert_eq!(persisted.message, "answer");
    assert_eq!(persisted.memory_citation, Some(expected_citation.clone()));

    let event_msgs = items
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::EventMsg(event) => Some(event),
            RolloutItem::SessionMeta(_)
            | RolloutItem::TranscriptItem(_)
            | RolloutItem::GhostSnapshot(_)
            | RolloutItem::Compacted(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::Workflow(_) => None,
        })
        .collect::<Vec<_>>();
    let turns = lha_app_server_protocol::build_turns_from_event_msgs(&event_msgs);
    let replayed_citation = turns.iter().find_map(|turn| {
        turn.items.iter().find_map(|item| match item {
            lha_app_server_protocol::ThreadItem::AgentMessage {
                memory_citation, ..
            } => memory_citation.clone(),
            lha_app_server_protocol::ThreadItem::UserMessage { .. }
            | lha_app_server_protocol::ThreadItem::Reasoning { .. }
            | lha_app_server_protocol::ThreadItem::Plan { .. }
            | lha_app_server_protocol::ThreadItem::WebSearch { .. }
            | lha_app_server_protocol::ThreadItem::McpToolCall { .. }
            | lha_app_server_protocol::ThreadItem::CommandExecution { .. }
            | lha_app_server_protocol::ThreadItem::FileChange { .. }
            | lha_app_server_protocol::ThreadItem::ImageView { .. }
            | lha_app_server_protocol::ThreadItem::EnteredReviewMode { .. }
            | lha_app_server_protocol::ThreadItem::ExitedReviewMode { .. }
            | lha_app_server_protocol::ThreadItem::ContextCompaction { .. } => None,
        })
    });
    assert_eq!(replayed_citation, Some(expected_citation));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_phase1_mock_response_creates_redacted_stage1_output() -> Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let state_db =
        StateRuntime::init(home.path().to_path_buf(), TEST_PROVIDER.to_string(), None).await?;
    let source_thread = write_memory_thread(
        home.path(),
        &state_db,
        vec![
            developer_message("developer instructions should be excluded"),
            user_message(
                "# AGENTS.md instructions for /tmp\n\n<INSTRUCTIONS>\nexclude me\n</INSTRUCTIONS>",
            ),
            user_message("<skill>\n<name>demo</name>\nexclude me\n</skill>"),
            user_message("<environment_context>\n<cwd>/tmp/project</cwd>\n</environment_context>"),
            user_message(&format!(
                "session wrapper {USER_MESSAGE_BEGIN} remember this {TEST_SECRET}"
            )),
        ],
    )
    .await?;
    put_phase2_in_cooldown(&state_db, source_thread).await?;

    let stage1_output = json!({
        "raw_memory": format!("The user's token is {TEST_SECRET}"),
        "rollout_summary": "User asked LHA to remember a token.",
        "rollout_slug": "remember-token"
    });
    let stage1_mock = mount_sse_once(
        &server,
        responses::sse(vec![
            ev_assistant_message("msg-stage1", &stage1_output.to_string()),
            ev_completed("resp-stage1"),
        ]),
    )
    .await;

    let mut builder = test_codex()
        .with_home(Arc::clone(&home))
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
            config.memories.max_rollouts_per_startup = 1;
            config.memories.min_rollout_idle_hours = 1;
        });
    let _test = builder.build(&server).await?;

    let outputs = wait_for_stage1_outputs(&state_db, 1).await?;
    assert_eq!(outputs.len(), 1);
    let output = &outputs[0];
    assert_eq!(output.thread_id, source_thread);
    assert_eq!(output.raw_memory, "The user's token is [REDACTED_SECRET]");
    assert_eq!(output.rollout_slug.as_deref(), Some("remember-token"));

    let request = stage1_mock.single_request();
    let prompt = request.message_input_texts("user").join("\n");
    assert!(prompt.contains("remember this [REDACTED_SECRET]"));
    assert!(prompt.contains("<environment_context>"));
    assert!(!prompt.contains(TEST_SECRET));
    assert!(!prompt.contains("developer instructions should be excluded"));
    assert!(!prompt.contains("AGENTS.md instructions"));
    assert!(!prompt.contains("<skill>"));
    assert!(!prompt.contains(USER_MESSAGE_BEGIN));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_phase1_empty_output_removes_stale_stage1_output() -> Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let state_db =
        StateRuntime::init(home.path().to_path_buf(), TEST_PROVIDER.to_string(), None).await?;
    let source_thread =
        write_memory_thread(home.path(), &state_db, vec![user_message("old memory")]).await?;
    seed_stage1_output(&state_db, source_thread, "stale raw", "stale summary").await?;
    make_thread_updated_again(&state_db, source_thread).await?;
    put_phase2_in_cooldown(&state_db, source_thread).await?;

    let stage1_output = json!({
        "raw_memory": "",
        "rollout_summary": "No durable memory.",
        "rollout_slug": null
    });
    mount_sse_once(
        &server,
        responses::sse(vec![
            ev_assistant_message("msg-stage1-empty", &stage1_output.to_string()),
            ev_completed("resp-stage1-empty"),
        ]),
    )
    .await;

    let mut builder = test_codex()
        .with_home(Arc::clone(&home))
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
            config.memories.max_rollouts_per_startup = 1;
            config.memories.min_rollout_idle_hours = 1;
        });
    let _test = builder.build(&server).await?;

    wait_for_no_stage1_outputs(&state_db).await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_phase1_unsupported_output_schema_marks_job_failed() -> Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let state_db =
        StateRuntime::init(home.path().to_path_buf(), TEST_PROVIDER.to_string(), None).await?;
    let source_thread =
        write_memory_thread(home.path(), &state_db, vec![user_message("remember chat")]).await?;
    put_phase2_in_cooldown(&state_db, source_thread).await?;

    let mut builder = test_codex()
        .with_home(Arc::clone(&home))
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
            config.model_provider.set_chat_turns();
            config.memories.max_rollouts_per_startup = 1;
            config.memories.min_rollout_idle_hours = 1;
        });
    let _test = builder.build(&server).await?;

    let job = wait_for_memory_job(
        home.path(),
        JOB_KIND_STAGE1,
        &source_thread.to_string(),
        "error",
    )
    .await?;
    assert_eq!(job.last_error.as_deref(), Some("unsupported_output_schema"));
    assert_eq!(job.retry_remaining, 2);
    assert!(job.retry_at.is_some());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_phase2_mock_consolidation_agent_updates_workspace() -> Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let state_db =
        StateRuntime::init(home.path().to_path_buf(), TEST_PROVIDER.to_string(), None).await?;
    let source_thread =
        write_memory_thread(home.path(), &state_db, vec![user_message("remember rust")]).await?;
    seed_stage1_output(
        &state_db,
        source_thread,
        "The user works mostly in Rust.",
        "User asked to remember Rust preferences.",
    )
    .await?;

    let call_id = "write-memory-files";
    let command = "printf '%s\\n' '# Memory' 'User works mostly in Rust.' > MEMORY.md && printf '%s\\n' '# Summary' 'Rust preferences are available.' > memory_summary.md";
    let args = json!({
        "command": command,
        "timeout_ms": 1_000,
        "login": false,
    });
    mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-phase2-tool"),
                ev_function_call(call_id, "shell_command", &args.to_string()),
                ev_completed("resp-phase2-tool"),
            ]),
            responses::sse(vec![
                ev_assistant_message("msg-phase2-done", "memory workspace updated"),
                ev_completed("resp-phase2-done"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex()
        .with_home(Arc::clone(&home))
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
            config.memories.max_rollouts_per_startup = 1;
            config.memories.min_rollout_idle_hours = 1;
        });
    let _test = builder.build(&server).await?;

    let memory_root = home.path().join("memories");
    let summary = wait_for_file_contains(&memory_root.join("memory_summary.md"), "Rust").await?;
    assert!(summary.contains("Rust preferences"));
    wait_for_clean_memory_workspace(&memory_root).await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_phase2_no_workspace_diff_succeeds_without_consolidation_agent() -> Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let state_db =
        StateRuntime::init(home.path().to_path_buf(), TEST_PROVIDER.to_string(), None).await?;
    let source_thread = write_memory_thread(
        home.path(),
        &state_db,
        vec![user_message("remember stable")],
    )
    .await?;
    seed_stage1_output(
        &state_db,
        source_thread,
        "The user likes deterministic memory files.",
        "User wants deterministic memory files.",
    )
    .await?;
    prepare_clean_phase2_workspace(home.path(), &state_db).await?;

    let mut builder = test_codex()
        .with_home(Arc::clone(&home))
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
            config.memories.max_rollouts_per_startup = 1;
            config.memories.min_rollout_idle_hours = 1;
        });
    let _test = builder.build(&server).await?;

    let job = wait_for_memory_job(home.path(), JOB_KIND_PHASE2, PHASE2_JOB_KEY, "done").await?;
    assert_eq!(job.last_success_watermark, job.input_watermark);
    let output_state = memory_output_state(home.path(), source_thread).await?;
    assert!(output_state.selected_for_phase2);
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| anyhow::anyhow!("mock server request log unavailable"))?;
    assert!(
        !requests
            .iter()
            .any(|request| request.url.path().ends_with("/responses")),
        "phase2 should not spawn a consolidation agent when the workspace has no changes"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_phase2_failed_consolidation_agent_marks_retryable_error() -> Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let state_db =
        StateRuntime::init(home.path().to_path_buf(), TEST_PROVIDER.to_string(), None).await?;
    let source_thread = write_memory_thread(
        home.path(),
        &state_db,
        vec![user_message("remember failure")],
    )
    .await?;
    seed_stage1_output(
        &state_db,
        source_thread,
        "The user wants failure paths covered.",
        "User asked for failure-path coverage.",
    )
    .await?;
    mount_sse_once(
        &server,
        responses::sse_failed("resp-phase2-failed", "server_error", "phase2 failed"),
    )
    .await;

    let mut builder = test_codex()
        .with_home(Arc::clone(&home))
        .with_config(|config| {
            config.features.enable(Feature::MemoryTool);
            config.memories.max_rollouts_per_startup = 1;
            config.memories.min_rollout_idle_hours = 1;
        });
    let _test = builder.build(&server).await?;

    let job = wait_for_memory_job(home.path(), JOB_KIND_PHASE2, PHASE2_JOB_KEY, "error").await?;
    assert_eq!(job.last_error.as_deref(), Some("failed_agent"));
    assert_eq!(job.retry_remaining, 2);
    assert!(job.retry_at.is_some());

    Ok(())
}

async fn write_memory_thread(
    lha_home: &Path,
    state_db: &StateRuntime,
    transcript: Vec<TranscriptItem>,
) -> Result<ThreadId> {
    let uuid = Uuid::now_v7();
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let created_at = chrono::Utc::now() - chrono::Duration::hours(8);
    let updated_at = chrono::Utc::now() - chrono::Duration::hours(7);
    let rollout_path = lha_home.join(format!(
        "sessions/2026/06/01/rollout-2026-06-01T00-00-00-{uuid}.jsonl"
    ));
    if let Some(parent) = rollout_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut lines = vec![RolloutLine {
        timestamp: created_at.to_rfc3339(),
        item: RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                id: thread_id,
                forked_from_id: None,
                timestamp: created_at.to_rfc3339(),
                cwd: lha_home.to_path_buf(),
                originator: "test".to_string(),
                cli_version: "test".to_string(),
                rollout_schema_version: lha_protocol::protocol::ROLLOUT_SCHEMA_VERSION_V3,
                source: SessionSource::VSCode,
                model_provider: Some(TEST_PROVIDER.to_string()),
                base_instructions: None,
                dynamic_tools: None,
                memory_mode: Some(lha_protocol::protocol::MEMORY_MODE_ENABLED.to_string()),
            },
            git: None,
        }),
    }];
    lines.extend(transcript.into_iter().map(|item| RolloutLine {
        timestamp: updated_at.to_rfc3339(),
        item: RolloutItem::TranscriptItem(item),
    }));
    let jsonl = lines
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    tokio::fs::write(&rollout_path, format!("{jsonl}\n")).await?;

    state_db
        .upsert_thread(&ThreadMetadata {
            id: thread_id,
            rollout_path,
            created_at,
            updated_at,
            source: SessionSource::VSCode.to_string(),
            model_provider: TEST_PROVIDER.to_string(),
            cwd: lha_home.to_path_buf(),
            title: "memory source".to_string(),
            sandbox_policy: "danger-full-access".to_string(),
            approval_mode: "never".to_string(),
            tokens_used: 0,
            has_user_event: true,
            archived_at: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
            memory_mode: lha_protocol::protocol::MEMORY_MODE_ENABLED.to_string(),
        })
        .await?;
    Ok(thread_id)
}

async fn make_thread_updated_again(state_db: &StateRuntime, thread_id: ThreadId) -> Result<()> {
    let Some(mut metadata) = state_db.get_thread(thread_id).await? else {
        anyhow::bail!("thread should exist");
    };
    metadata.updated_at = chrono::Utc::now() - chrono::Duration::hours(6);
    state_db.upsert_thread(&metadata).await
}

async fn seed_stage1_output(
    state_db: &StateRuntime,
    thread_id: ThreadId,
    raw_memory: &str,
    rollout_summary: &str,
) -> Result<()> {
    let claim = claim_one_stage1_job(state_db, thread_id).await?;
    let source_updated_at = claim.thread.updated_at.timestamp();
    assert!(
        memory_store(state_db)?
            .mark_stage1_job_succeeded(
                thread_id,
                &claim.ownership_token,
                source_updated_at,
                raw_memory,
                rollout_summary,
                Some("mocked-memory"),
            )
            .await?
    );
    Ok(())
}

async fn claim_one_stage1_job(
    state_db: &StateRuntime,
    thread_id: ThreadId,
) -> Result<lha_state::Stage1JobClaim> {
    let allowed_sources = vec![SessionSource::VSCode.to_string()];
    let claims = memory_store(state_db)?
        .claim_stage1_jobs_for_startup(
            test_thread_id(9_999)?,
            Stage1StartupClaimParams {
                scan_limit: 10,
                max_claimed: 10,
                max_age_days: 10,
                min_rollout_idle_hours: 1,
                allowed_sources: &allowed_sources,
                lease_seconds: lha_memories_write::STAGE_ONE_JOB_LEASE_SECONDS,
            },
        )
        .await?;
    claims
        .into_iter()
        .find(|claim| claim.thread.id == thread_id)
        .ok_or_else(|| anyhow::anyhow!("stage1 job was not claimed"))
}

async fn put_phase2_in_cooldown(state_db: &StateRuntime, thread_id: ThreadId) -> Result<()> {
    match memory_store(state_db)?
        .try_claim_global_phase2_job(thread_id, lha_memories_write::STAGE_TWO_JOB_LEASE_SECONDS)
        .await?
    {
        Phase2JobClaimOutcome::Claimed {
            ownership_token, ..
        } => {
            assert!(
                memory_store(state_db)?
                    .mark_global_phase2_job_succeeded(&ownership_token, 0, &[])
                    .await?
            );
            Ok(())
        }
        Phase2JobClaimOutcome::SkippedRetryUnavailable
        | Phase2JobClaimOutcome::SkippedCooldown
        | Phase2JobClaimOutcome::SkippedRunning => Ok(()),
    }
}

async fn wait_for_stage1_outputs(
    state_db: &StateRuntime,
    expected: usize,
) -> Result<Vec<lha_state::Stage1Output>> {
    wait_for_condition(|| async {
        let outputs = memory_store(state_db)?
            .list_stage1_outputs_for_global(10)
            .await?;
        Ok((outputs.len() >= expected).then_some(outputs))
    })
    .await
}

async fn wait_for_no_stage1_outputs(state_db: &StateRuntime) -> Result<()> {
    wait_for_condition(|| async {
        let outputs = memory_store(state_db)?
            .list_stage1_outputs_for_global(10)
            .await?;
        Ok(outputs.is_empty().then_some(()))
    })
    .await
}

async fn wait_for_file_contains(path: &Path, needle: &str) -> Result<String> {
    wait_for_condition(|| async {
        match tokio::fs::read_to_string(path).await {
            Ok(contents) if contents.contains(needle) => Ok(Some(contents)),
            Ok(_) => Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    })
    .await
}

async fn wait_for_clean_memory_workspace(memory_root: &Path) -> Result<()> {
    wait_for_condition(|| async {
        if tokio::fs::try_exists(memory_root.join("phase2_workspace_diff.md")).await? {
            return Ok(None);
        }
        match lha_memories_write::memory_workspace_diff(memory_root).await {
            Ok(diff) if !diff.has_changes() => Ok(Some(())),
            Ok(_) | Err(_) => Ok(None),
        }
    })
    .await
}

async fn prepare_clean_phase2_workspace(lha_home: &Path, state_db: &StateRuntime) -> Result<()> {
    let memory_root = lha_home.join("memories");
    let selected = memory_store(state_db)?
        .get_phase2_input_selection(10, 30)
        .await?;
    lha_memories_write::prepare_memory_workspace(&memory_root).await?;
    lha_memories_write::sync_rollout_summaries_from_memories(&memory_root, &selected, 10).await?;
    lha_memories_write::rebuild_raw_memories_file_from_memories(&memory_root, &selected, 10)
        .await?;
    lha_memories_write::reset_memory_workspace_baseline(&memory_root).await?;
    Ok(())
}

fn memory_store(state_db: &StateRuntime) -> Result<&lha_state::MemoryStore> {
    state_db
        .memories()
        .ok_or_else(|| anyhow::anyhow!("memory store unavailable"))
}

#[derive(Debug)]
struct MemoryJobRow {
    last_error: Option<String>,
    retry_at: Option<i64>,
    retry_remaining: i64,
    input_watermark: Option<i64>,
    last_success_watermark: Option<i64>,
}

async fn wait_for_memory_job(
    lha_home: &Path,
    kind: &str,
    job_key: &str,
    expected_status: &str,
) -> Result<MemoryJobRow> {
    let pool = open_memories_pool(lha_home).await?;
    wait_for_condition(|| async {
        let Some(row) = sqlx::query(
            "SELECT status, last_error, retry_at, retry_remaining, input_watermark, last_success_watermark FROM jobs WHERE kind = ? AND job_key = ?",
        )
        .bind(kind)
        .bind(job_key)
        .fetch_optional(&pool)
        .await? else {
            return Ok(None);
        };
        let status: String = row.try_get("status")?;
        if status != expected_status {
            return Ok(None);
        }
        Ok(Some(MemoryJobRow {
            last_error: row.try_get("last_error")?,
            retry_at: row.try_get("retry_at")?,
            retry_remaining: row.try_get("retry_remaining")?,
            input_watermark: row.try_get("input_watermark")?,
            last_success_watermark: row.try_get("last_success_watermark")?,
        }))
    })
    .await
}

#[derive(Debug)]
struct MemoryOutputState {
    selected_for_phase2: bool,
}

async fn memory_output_state(lha_home: &Path, thread_id: ThreadId) -> Result<MemoryOutputState> {
    let pool = open_memories_pool(lha_home).await?;
    let row = sqlx::query("SELECT selected_for_phase2 FROM stage1_outputs WHERE thread_id = ?")
        .bind(thread_id.to_string())
        .fetch_one(&pool)
        .await?;
    Ok(MemoryOutputState {
        selected_for_phase2: row.try_get::<i64, _>("selected_for_phase2")? != 0,
    })
}

async fn open_memories_pool(lha_home: &Path) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(lha_home.join(lha_state::MEMORIES_DB_FILENAME))
        .create_if_missing(false)
        .busy_timeout(StdDuration::from_secs(5));
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?)
}

async fn wait_for_condition<T, Fut, F>(mut check: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>>>,
{
    for _ in 0..200 {
        if let Some(value) = check().await? {
            return Ok(value);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("timed out waiting for memory condition")
}

fn test_thread_id(seed: u128) -> Result<ThreadId> {
    Ok(ThreadId::from_string(&Uuid::from_u128(seed).to_string())?)
}

fn user_message(text: &str) -> TranscriptItem {
    TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
    }
}

fn developer_message(text: &str) -> TranscriptItem {
    TranscriptItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
    }
}

async fn read_rollout_items(path: &Path) -> Result<Vec<RolloutItem>> {
    let contents = tokio::fs::read_to_string(path).await?;
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<RolloutLine>(line)
                .map(|rollout_line| rollout_line.item)
                .map_err(Into::into)
        })
        .collect()
}
