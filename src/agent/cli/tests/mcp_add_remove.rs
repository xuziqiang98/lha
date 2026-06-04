use std::path::Path;

use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

mod common;

fn codex_command(lha_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(common::cargo_bin::cargo_bin("lha")?);
    cmd.env("LHA_HOME", lha_home);
    Ok(cmd)
}

fn read_config(lha_home: &Path) -> Result<toml::Value> {
    let config = std::fs::read_to_string(lha_home.join("config.toml"))?;
    Ok(toml::from_str(&config)?)
}

fn read_config_text(lha_home: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(lha_home.join("config.toml")).unwrap_or_default())
}

#[tokio::test]
async fn add_and_remove_server_updates_global_config() -> Result<()> {
    let lha_home = TempDir::new()?;

    let mut add_cmd = codex_command(lha_home.path())?;
    add_cmd
        .args(["mcp", "add", "docs", "--", "echo", "hello"])
        .assert()
        .success()
        .stdout(contains("Added global MCP server 'docs'."));

    let config = read_config(lha_home.path())?;
    let expected_docs: toml::Value = toml::from_str(
        r#"
command = "echo"
args = ["hello"]
"#,
    )?;
    assert_eq!(config["mcp_servers"]["docs"], expected_docs);

    let mut remove_cmd = codex_command(lha_home.path())?;
    remove_cmd
        .args(["mcp", "remove", "docs"])
        .assert()
        .success()
        .stdout(contains("Removed global MCP server 'docs'."));

    assert!(!read_config_text(lha_home.path())?.contains("mcp_servers"));

    let mut remove_again_cmd = codex_command(lha_home.path())?;
    remove_again_cmd
        .args(["mcp", "remove", "docs"])
        .assert()
        .success()
        .stdout(contains("No MCP server named 'docs' found."));

    assert!(!read_config_text(lha_home.path())?.contains("mcp_servers"));

    Ok(())
}

#[tokio::test]
async fn add_with_env_preserves_key_order_and_values() -> Result<()> {
    let lha_home = TempDir::new()?;

    let mut add_cmd = codex_command(lha_home.path())?;
    add_cmd
        .args([
            "mcp",
            "add",
            "envy",
            "--env",
            "FOO=bar",
            "--env",
            "ALPHA=beta",
            "--",
            "python",
            "server.py",
        ])
        .assert()
        .success();

    let config = read_config(lha_home.path())?;
    let expected_envy: toml::Value = toml::from_str(
        r#"
command = "python"
args = ["server.py"]

[env]
ALPHA = "beta"
FOO = "bar"
"#,
    )?;
    assert_eq!(config["mcp_servers"]["envy"], expected_envy);

    Ok(())
}

#[tokio::test]
async fn add_streamable_http_without_manual_token() -> Result<()> {
    let lha_home = TempDir::new()?;

    let mut add_cmd = codex_command(lha_home.path())?;
    add_cmd
        .args(["mcp", "add", "github", "--url", "https://example.com/mcp"])
        .assert()
        .success();

    let config = read_config(lha_home.path())?;
    let expected_github: toml::Value = toml::from_str(
        r#"
url = "https://example.com/mcp"
"#,
    )?;
    assert_eq!(config["mcp_servers"]["github"], expected_github);

    assert!(!lha_home.path().join(".credentials.json").exists());
    assert!(!lha_home.path().join(".env").exists());

    Ok(())
}

#[tokio::test]
async fn add_streamable_http_with_custom_env_var() -> Result<()> {
    let lha_home = TempDir::new()?;

    let mut add_cmd = codex_command(lha_home.path())?;
    add_cmd
        .args([
            "mcp",
            "add",
            "issues",
            "--url",
            "https://example.com/issues",
            "--bearer-token-env-var",
            "GITHUB_TOKEN",
        ])
        .assert()
        .success();

    let config = read_config(lha_home.path())?;
    let expected_issues: toml::Value = toml::from_str(
        r#"
url = "https://example.com/issues"
bearer_token_env_var = "GITHUB_TOKEN"
"#,
    )?;
    assert_eq!(config["mcp_servers"]["issues"], expected_issues);
    Ok(())
}

#[tokio::test]
async fn add_streamable_http_rejects_removed_flag() -> Result<()> {
    let lha_home = TempDir::new()?;

    let mut add_cmd = codex_command(lha_home.path())?;
    add_cmd
        .args([
            "mcp",
            "add",
            "github",
            "--url",
            "https://example.com/mcp",
            "--with-bearer-token",
        ])
        .assert()
        .failure()
        .stderr(contains("--with-bearer-token"));

    assert!(!read_config_text(lha_home.path())?.contains("mcp_servers"));

    Ok(())
}

#[tokio::test]
async fn add_cant_add_command_and_url() -> Result<()> {
    let lha_home = TempDir::new()?;

    let mut add_cmd = codex_command(lha_home.path())?;
    add_cmd
        .args([
            "mcp",
            "add",
            "github",
            "--url",
            "https://example.com/mcp",
            "--command",
            "--",
            "echo",
            "hello",
        ])
        .assert()
        .failure()
        .stderr(contains("unexpected argument '--command' found"));

    assert!(!read_config_text(lha_home.path())?.contains("mcp_servers"));

    Ok(())
}
