use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::select;
use tokio::time::timeout;

/// Regression test for https://github.com/openai/codex/issues/8803.
#[tokio::test]
async fn malformed_rules_should_not_panic() -> anyhow::Result<()> {
    // run_codex_cli() does not work on Windows due to PTY limitations.
    if cfg!(windows) {
        return Ok(());
    }

    let tmp = tempfile::tempdir()?;
    let codex_home = tmp.path();
    std::fs::write(
        codex_home.join("rules"),
        "rules should be a directory not a file",
    )?;

    // TODO(mbolin): Figure out why using a temp dir as the cwd causes this test
    // to hang.
    let cwd = std::env::current_dir()?;
    let config_contents = format!(
        r#"
# Pick a local provider so the CLI doesn't prompt for OpenAI auth in this test.
model_provider = "ollama"

[projects]
"{cwd}" = {{ trust_level = "trusted" }}
"#,
        cwd = cwd.display()
    );
    std::fs::write(codex_home.join("config.toml"), config_contents)?;

    let CodexCliOutput { exit_code, output } = run_codex_cli(codex_home, cwd).await?;
    assert_ne!(0, exit_code, "Codex CLI should exit nonzero.");
    assert!(
        output.contains("ERROR: Failed to initialize codex:"),
        "expected startup error in output, got: {output}"
    );
    assert!(
        output.contains("failed to read rules files"),
        "expected rules read error in output, got: {output}"
    );
    Ok(())
}

struct CodexCliOutput {
    exit_code: i32,
    output: String,
}

async fn run_codex_cli(
    codex_home: impl AsRef<Path>,
    cwd: impl AsRef<Path>,
) -> anyhow::Result<CodexCliOutput> {
    let (program, args, timeout_secs) = match codex_utils_cargo_bin::cargo_bin("codey") {
        Ok(path) => (
            path.to_string_lossy().into_owned(),
            vec!["-c".to_string(), "analytics.enabled=false".to_string()],
            10,
        ),
        Err(codex_utils_cargo_bin::CargoBinError::NotFound { .. })
        | Err(codex_utils_cargo_bin::CargoBinError::ResolvedPathDoesNotExist { .. }) => {
            let built_binary = build_codey_binary().await?;
            (
                built_binary.to_string_lossy().into_owned(),
                vec!["-c".to_string(), "analytics.enabled=false".to_string()],
                10,
            )
        }
        Err(err) => return Err(err.into()),
    };

    let mut env = HashMap::new();
    env.insert(
        "CODEY_HOME".to_string(),
        codex_home.as_ref().display().to_string(),
    );

    let spawned =
        codex_utils_pty::spawn_pty_process(&program, &args, cwd.as_ref(), &env, &None).await?;
    let mut output = Vec::new();
    let mut output_rx = spawned.output_rx;
    let mut exit_rx = spawned.exit_rx;
    let writer_tx = spawned.session.writer_sender();
    let exit_code_result = timeout(Duration::from_secs(timeout_secs), async {
        // Read PTY output until the process exits while replying to cursor
        // position queries so the TUI can initialize without a real terminal.
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        // The TUI asks for the cursor position via ESC[6n.
                        // Respond with a valid position to unblock startup.
                        if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                        }
                        output.extend_from_slice(&chunk);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break exit_rx.await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                result = &mut exit_rx => break result,
            }
        }
    })
    .await;
    let exit_code = match exit_code_result {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            spawned.session.terminate();
            anyhow::bail!("timed out waiting for codex CLI to exit");
        }
    };
    // Drain any output that raced with the exit notification.
    while let Ok(chunk) = output_rx.try_recv() {
        output.extend_from_slice(&chunk);
    }

    let output = String::from_utf8_lossy(&output);
    Ok(CodexCliOutput {
        exit_code,
        output: output.to_string(),
    })
}

fn cli_manifest_path() -> anyhow::Result<PathBuf> {
    let tui_manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = tui_manifest_dir.parent().ok_or_else(|| {
        anyhow::anyhow!("expected tui crate to live under the codex-rs workspace root")
    })?;
    Ok(workspace_root.join("cli/Cargo.toml"))
}

async fn build_codey_binary() -> anyhow::Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let cli_manifest_path = cli_manifest_path()?;
    let workspace_root = cli_manifest_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            anyhow::anyhow!("expected cli manifest to live under the codex-rs workspace root")
        })?;

    let status = Command::new(&cargo)
        .args([
            "build",
            "-q",
            "--manifest-path",
            cli_manifest_path.to_string_lossy().as_ref(),
            "--bin",
            "codey",
        ])
        .current_dir(workspace_root)
        .status()
        .await?;

    if !status.success() {
        anyhow::bail!("failed to build codey binary via cargo before PTY test");
    }

    let target_dir = cargo_target_directory(&cargo, &cli_manifest_path, workspace_root).await?;
    let binary_name = if cfg!(windows) { "codey.exe" } else { "codey" };
    let binary_path = target_dir.join("debug").join(binary_name);
    if !binary_path.exists() {
        anyhow::bail!(
            "built codey binary was not found at {} (cargo target dir: {})",
            binary_path.display(),
            target_dir.display()
        );
    }

    Ok(binary_path)
}

async fn cargo_target_directory(
    cargo: &str,
    cli_manifest_path: &Path,
    workspace_root: &Path,
) -> anyhow::Result<PathBuf> {
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
            cli_manifest_path.to_string_lossy().as_ref(),
        ])
        .current_dir(workspace_root)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("failed to query cargo metadata for PTY test: {stderr}");
    }

    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let target_dir = metadata
        .get("target_directory")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("cargo metadata did not include target_directory"))?;

    Ok(PathBuf::from(target_dir))
}
