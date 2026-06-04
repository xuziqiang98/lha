use crate::product::app_server_protocol::ClientInfo;
use crate::product::app_server_protocol::InitializeResponse;
use crate::product::app_server_protocol::JSONRPCMessage;
use crate::test_support::app_server::McpProcess;
use crate::test_support::app_server::create_mock_responses_server_sequence_unchecked;
use crate::test_support::app_server::to_response;
use anyhow::Result;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn initialize_uses_client_info_name_as_originator() -> Result<()> {
    let responses = Vec::new();
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri(), "never")?;
    let mut mcp = McpProcess::new(lha_home.path()).await?;

    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_client_info(ClientInfo {
            name: "lha_vscode".to_string(),
            title: Some("LHA VS Code Extension".to_string()),
            version: "0.1.0".to_string(),
        }),
    )
    .await??;

    let JSONRPCMessage::Response(response) = message else {
        anyhow::bail!("expected initialize response, got {message:?}");
    };
    let InitializeResponse { user_agent } = to_response::<InitializeResponse>(response)?;

    assert!(user_agent.starts_with("lha_vscode/"));
    Ok(())
}

#[tokio::test]
async fn initialize_respects_originator_override_env_var() -> Result<()> {
    let responses = Vec::new();
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri(), "never")?;
    let mut mcp = McpProcess::new_with_env(
        lha_home.path(),
        &[(
            "LHA_INTERNAL_ORIGINATOR_OVERRIDE",
            Some("lha_originator_via_env_var"),
        )],
    )
    .await?;

    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_client_info(ClientInfo {
            name: "lha_vscode".to_string(),
            title: Some("LHA VS Code Extension".to_string()),
            version: "0.1.0".to_string(),
        }),
    )
    .await??;

    let JSONRPCMessage::Response(response) = message else {
        anyhow::bail!("expected initialize response, got {message:?}");
    };
    let InitializeResponse { user_agent } = to_response::<InitializeResponse>(response)?;

    assert!(user_agent.starts_with("lha_originator_via_env_var/"));
    Ok(())
}

#[tokio::test]
async fn initialize_rejects_invalid_client_name() -> Result<()> {
    let responses = Vec::new();
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    let lha_home = TempDir::new()?;
    create_config_toml(lha_home.path(), &server.uri(), "never")?;
    let mut mcp = McpProcess::new_with_env(
        lha_home.path(),
        &[("LHA_INTERNAL_ORIGINATOR_OVERRIDE", None)],
    )
    .await?;

    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_client_info(ClientInfo {
            name: "bad\rname".to_string(),
            title: Some("Bad Client".to_string()),
            version: "0.1.0".to_string(),
        }),
    )
    .await??;

    let JSONRPCMessage::Error(error) = message else {
        anyhow::bail!("expected initialize error, got {message:?}");
    };

    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        "Invalid clientInfo.name: 'bad\rname'. Must be a valid HTTP header value."
    );
    assert_eq!(error.error.data, None);
    Ok(())
}

// Helper to create a config.toml pointing at the mock model server.
fn create_config_toml(
    lha_home: &Path,
    server_uri: &str,
    approval_policy: &str,
) -> std::io::Result<()> {
    crate::test_support::app_server::write_mock_responses_config_toml_with_options(
        lha_home,
        server_uri,
        &std::collections::BTreeMap::new(),
        20_000,
        Some(false),
        "mock_provider",
        "mock-model",
        "",
        approval_policy,
        "read-only",
    )
}
