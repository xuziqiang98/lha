use std::process::Command;

use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;
use tempfile::TempDir;

mod common;

#[test]
fn single_binary_compat_responses_proxy_arg0_help() -> Result<()> {
    let lha_home = TempDir::new()?;
    // This smoke test exercises the argv0 alias path. The security guarantee is
    // that arg0 dispatch handles this alias before dotenv/PATH helper setup;
    // `lha responses-api-proxy` is not the documented safe-start path.
    let output = Command::new(common::cargo_bin::cargo_bin("codex-responses-api-proxy")?)
        .env("LHA_HOME", lha_home.path())
        .arg("--help")
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        output.status.success(),
        "responses proxy help failed: {combined}"
    );
    assert!(
        combined.contains("Minimal OpenAI responses proxy") || combined.contains("--upstream-url"),
        "responses proxy help did not render proxy options: {combined}"
    );

    Ok(())
}

#[test]
fn single_binary_compat_lha_responses_proxy_subcommand_is_rejected() -> Result<()> {
    let lha_home = TempDir::new()?;
    let output = Command::new(common::cargo_bin::cargo_bin("lha")?)
        .env("LHA_HOME", lha_home.path())
        .arg("responses-api-proxy")
        .arg("--help")
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        !output.status.success(),
        "responses proxy subcommand unexpectedly succeeded: {combined}"
    );
    assert!(
        combined.contains("unrecognized subcommand") && combined.contains("responses-api-proxy"),
        "responses proxy subcommand error did not identify the removed subcommand: {combined}"
    );

    Ok(())
}

#[test]
fn single_binary_compat_lha_responses_proxy_after_global_flags_is_rejected() -> Result<()> {
    let lha_home = TempDir::new()?;
    let output = Command::new(common::cargo_bin::cargo_bin("lha")?)
        .env("LHA_HOME", lha_home.path())
        .args(["-c", "model=gpt-5.1", "responses-api-proxy", "--help"])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        !output.status.success(),
        "responses proxy subcommand unexpectedly succeeded: {combined}"
    );
    assert!(
        combined.contains("unrecognized subcommand") && combined.contains("responses-api-proxy"),
        "responses proxy subcommand error did not identify the removed subcommand: {combined}"
    );

    Ok(())
}

#[test]
fn single_binary_compat_npm_wrapper_keeps_proxy_binary_name() -> Result<()> {
    let wrapper_path = common::cargo_bin::repo_root()?
        .join("src/agent/cli/product/responses_api_proxy/npm/bin/codex-responses-api-proxy.js");
    let wrapper = std::fs::read_to_string(wrapper_path)?;

    assert!(wrapper.contains("const binaryBaseName = \"codex-responses-api-proxy\";"));

    Ok(())
}

#[test]
fn single_binary_compat_npm_package_keeps_openai_proxy_contract() -> Result<()> {
    let package_path = common::cargo_bin::repo_root()?
        .join("src/agent/cli/product/responses_api_proxy/npm/package.json");
    let package: JsonValue = serde_json::from_str(&std::fs::read_to_string(package_path)?)?;

    assert_eq!(package["name"], "@openai/codex-responses-api-proxy");
    assert_eq!(
        package["bin"],
        json!({
            "codex-responses-api-proxy": "bin/codex-responses-api-proxy.js",
        })
    );

    Ok(())
}
