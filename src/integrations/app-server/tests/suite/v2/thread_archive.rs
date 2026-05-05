use adam_agent::ARCHIVED_SESSIONS_SUBDIR;
use adam_agent::find_thread_path_by_id_str;
use adam_app_server_protocol::JSONRPCResponse;
use adam_app_server_protocol::RequestId;
use adam_app_server_protocol::ThreadArchiveParams;
use adam_app_server_protocol::ThreadArchiveResponse;
use adam_app_server_protocol::ThreadStartParams;
use adam_app_server_protocol::ThreadStartResponse;
use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use app_test_support::write_mock_responses_models_json;
use app_test_support::write_state_json;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_archive_moves_rollout_into_archived_directory() -> Result<()> {
    let adam_home = TempDir::new()?;
    create_config_toml(adam_home.path())?;

    let mut mcp = McpProcess::new(adam_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    // Start a thread.
    let start_id = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;
    assert!(!thread.id.is_empty());

    // Locate the rollout path recorded for this thread id.
    let rollout_path = find_thread_path_by_id_str(adam_home.path(), &thread.id)
        .await?
        .expect("expected rollout path for thread id to exist");
    assert!(
        rollout_path.exists(),
        "expected {} to exist",
        rollout_path.display()
    );

    // Archive the thread.
    let archive_id = mcp
        .send_thread_archive_request(ThreadArchiveParams {
            thread_id: thread.id.clone(),
        })
        .await?;
    let archive_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(archive_id)),
    )
    .await??;
    let _: ThreadArchiveResponse = to_response::<ThreadArchiveResponse>(archive_resp)?;

    // Verify file moved.
    let archived_directory = adam_home.path().join(ARCHIVED_SESSIONS_SUBDIR);
    // The archived file keeps the original filename (rollout-...-<id>.jsonl).
    let archived_rollout_path =
        archived_directory.join(rollout_path.file_name().expect("rollout file name"));
    assert!(
        !rollout_path.exists(),
        "expected rollout path {} to be moved",
        rollout_path.display()
    );
    assert!(
        archived_rollout_path.exists(),
        "expected archived rollout path {} to exist",
        archived_rollout_path.display()
    );

    Ok(())
}

fn create_config_toml(adam_home: &Path) -> std::io::Result<()> {
    let config_toml = adam_home.join("config.toml");
    std::fs::write(config_toml, config_contents())?;
    write_mock_responses_models_json(
        adam_home,
        "https://example.com",
        "mock_provider",
        false,
        None,
        "mock-model",
    )?;
    write_state_json(adam_home, "mock_provider.main:mock-model")
}

fn config_contents() -> &'static str {
    r#"approval_policy = "never"
sandbox_mode = "read-only"

[features]
remote_models = false
"#
}
