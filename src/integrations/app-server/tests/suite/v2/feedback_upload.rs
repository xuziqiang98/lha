use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_fake_rollout;
use app_test_support::rollout_path;
use app_test_support::to_response;
use lha_app_server_protocol::FeedbackUploadParams;
use lha_app_server_protocol::FeedbackUploadResponse;
use lha_app_server_protocol::JSONRPCError;
use lha_app_server_protocol::RequestId;
use lha_app_server_protocol::ThreadResumeParams;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_upload_persists_local_bundle() -> Result<()> {
    let lha_home = TempDir::new()?;
    let lha_home_path = lha_home.path().canonicalize()?;
    let thread_id = create_fake_rollout(
        lha_home.path(),
        "2025-02-01T10-00-00",
        "2025-02-01T10:00:00Z",
        "hello",
        Some("openai"),
        None,
    )?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;
    let resume_id = mcp
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(),
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(resume_id)),
    )
    .await??;

    let request_id = mcp
        .send_feedback_upload_request(FeedbackUploadParams {
            classification: "bug".to_string(),
            reason: Some("details".to_string()),
            thread_id: Some(thread_id.clone()),
            include_logs: true,
        })
        .await?;
    let response: FeedbackUploadResponse = to_response(
        timeout(
            DEFAULT_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
        )
        .await??,
    )?;

    assert_eq!(response.thread_id, thread_id);
    assert!(
        response
            .saved_path
            .starts_with(lha_home_path.join("feedback"))
    );
    assert!(response.saved_path.exists());
    assert!(response.saved_path.join("metadata.json").exists());
    assert!(response.saved_path.join("lha-logs.log").exists());

    let rollout_file = rollout_path(lha_home.path(), "2025-02-01T10-00-00", &response.thread_id);
    let rollout_name = rollout_file
        .file_name()
        .expect("rollout file name should exist");
    assert!(response.saved_path.join(rollout_name).exists());

    let metadata: Value =
        serde_json::from_slice(&std::fs::read(response.saved_path.join("metadata.json"))?)?;
    assert_eq!(metadata["threadId"], response.thread_id);
    assert_eq!(metadata["classification"], "bug");
    assert_eq!(metadata["reason"], "details");
    assert_eq!(metadata["includeLogs"], true);
    assert_eq!(metadata["files"]["logs"], "lha-logs.log");
    assert_eq!(
        metadata["files"]["rollout"],
        rollout_name.to_string_lossy().to_string()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_upload_respects_disabled_config() -> Result<()> {
    let lha_home = TempDir::new()?;
    std::fs::write(
        lha_home.path().join("config.toml"),
        "[feedback]\nenabled = false\n",
    )?;

    let mut mcp = McpProcess::new(lha_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_feedback_upload_request(FeedbackUploadParams {
            classification: "other".to_string(),
            reason: None,
            thread_id: None,
            include_logs: false,
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "feedback is disabled by configuration");

    Ok(())
}
