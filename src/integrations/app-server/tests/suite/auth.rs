use adam_app_server_protocol::AuthMode;
use adam_app_server_protocol::GetAuthStatusParams;
use adam_app_server_protocol::GetAuthStatusResponse;
use adam_app_server_protocol::JSONRPCError;
use adam_app_server_protocol::JSONRPCResponse;
use adam_app_server_protocol::LoginApiKeyParams;
use adam_app_server_protocol::LoginApiKeyResponse;
use adam_app_server_protocol::RequestId;
use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

fn create_config_toml_custom_provider(
    adam_home: &Path,
    requires_openai_auth: bool,
) -> std::io::Result<()> {
    app_test_support::write_mock_responses_config_toml_with_options(
        adam_home,
        "http://127.0.0.1:0",
        &std::collections::BTreeMap::new(),
        20_000,
        Some(requires_openai_auth),
        "mock_provider",
        "mock-model",
        "",
        "never",
        "danger-full-access",
    )
}

fn create_config_toml(adam_home: &Path) -> std::io::Result<()> {
    let config_toml = adam_home.join("config.toml");
    std::fs::write(
        config_toml,
        r#"
approval_policy = "never"
sandbox_mode = "danger-full-access"
"#,
    )
}

fn create_config_toml_forced_login(adam_home: &Path, forced_method: &str) -> std::io::Result<()> {
    let config_toml = adam_home.join("config.toml");
    let contents = format!(
        r#"
approval_policy = "never"
sandbox_mode = "danger-full-access"
forced_login_method = "{forced_method}"
"#
    );
    std::fs::write(config_toml, contents)
}

async fn login_with_api_key_via_request(mcp: &mut McpProcess, api_key: &str) -> Result<()> {
    let request_id = mcp
        .send_login_api_key_request(LoginApiKeyParams {
            api_key: api_key.to_string(),
        })
        .await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: LoginApiKeyResponse = to_response(resp)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_auth_status_no_auth() -> Result<()> {
    let adam_home = TempDir::new()?;
    create_config_toml(adam_home.path())?;

    let mut mcp = McpProcess::new_with_env(adam_home.path(), &[("OPENAI_API_KEY", None)]).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_get_auth_status_request(GetAuthStatusParams {
            include_token: Some(true),
            refresh_token: Some(false),
        })
        .await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let status: GetAuthStatusResponse = to_response(resp)?;
    assert_eq!(status.auth_method, None, "expected no auth method");
    assert_eq!(status.auth_token, None, "expected no token");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_auth_status_with_api_key() -> Result<()> {
    let adam_home = TempDir::new()?;
    create_config_toml(adam_home.path())?;

    let mut mcp = McpProcess::new(adam_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    login_with_api_key_via_request(&mut mcp, "sk-test-key").await?;

    let request_id = mcp
        .send_get_auth_status_request(GetAuthStatusParams {
            include_token: Some(true),
            refresh_token: Some(false),
        })
        .await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let status: GetAuthStatusResponse = to_response(resp)?;
    assert_eq!(status.auth_method, Some(AuthMode::ApiKey));
    assert_eq!(status.auth_token, Some("sk-test-key".to_string()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_auth_status_with_api_key_when_auth_not_required() -> Result<()> {
    let adam_home = TempDir::new()?;
    create_config_toml_custom_provider(adam_home.path(), false)?;

    let mut mcp = McpProcess::new(adam_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    login_with_api_key_via_request(&mut mcp, "sk-test-key").await?;

    let request_id = mcp
        .send_get_auth_status_request(GetAuthStatusParams {
            include_token: Some(true),
            refresh_token: Some(false),
        })
        .await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let status: GetAuthStatusResponse = to_response(resp)?;
    assert_eq!(status.auth_method, None, "expected no auth method");
    assert_eq!(status.auth_token, None, "expected no token");
    assert_eq!(
        status.requires_openai_auth,
        Some(false),
        "requires_openai_auth should be false",
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_auth_status_with_api_key_no_include_token() -> Result<()> {
    let adam_home = TempDir::new()?;
    create_config_toml(adam_home.path())?;

    let mut mcp = McpProcess::new(adam_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    login_with_api_key_via_request(&mut mcp, "sk-test-key").await?;

    // Build params via struct so None field is omitted in wire JSON.
    let params = GetAuthStatusParams {
        include_token: None,
        refresh_token: Some(false),
    };
    let request_id = mcp.send_get_auth_status_request(params).await?;

    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let status: GetAuthStatusResponse = to_response(resp)?;
    assert_eq!(status.auth_method, Some(AuthMode::ApiKey));
    assert!(status.auth_token.is_none(), "token must be omitted");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_api_key_rejected_when_forced_chatgpt() -> Result<()> {
    let adam_home = TempDir::new()?;
    create_config_toml_forced_login(adam_home.path(), "chatgpt")?;

    let mut mcp = McpProcess::new(adam_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_login_api_key_request(LoginApiKeyParams {
            api_key: "sk-test-key".to_string(),
        })
        .await?;

    let err: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(
        err.error.message,
        "API key login is disabled. Use ChatGPT login instead."
    );
    Ok(())
}
