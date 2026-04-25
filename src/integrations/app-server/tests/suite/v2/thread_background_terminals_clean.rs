use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadBackgroundTerminalsCleanParams;
use codex_app_server_protocol::ThreadBackgroundTerminalsCleanResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_background_terminals_clean_returns_success_for_loaded_thread() -> Result<()> {
    let adam_home = TempDir::new()?;
    create_config_toml(adam_home.path())?;

    let mut mcp = McpProcess::new(adam_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

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

    let clean_id = mcp
        .send_thread_background_terminals_clean_request(ThreadBackgroundTerminalsCleanParams {
            thread_id: thread.id,
        })
        .await?;
    let clean_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(clean_id)),
    )
    .await??;
    let _: ThreadBackgroundTerminalsCleanResponse =
        to_response::<ThreadBackgroundTerminalsCleanResponse>(clean_resp)?;

    Ok(())
}

fn create_config_toml(adam_home: &Path) -> std::io::Result<()> {
    let config_toml = adam_home.join("config.toml");
    std::fs::write(config_toml, config_contents())
}

fn config_contents() -> &'static str {
    r#"model = "mock-model"
approval_policy = "never"
sandbox_mode = "read-only"

[features]
remote_models = false
"#
}
