#![allow(clippy::expect_used)]
use std::path::Path;
use tempfile::TempDir;
use wiremock::MockServer;

pub struct TestCodexExecBuilder {
    home: TempDir,
    cwd: TempDir,
}

impl TestCodexExecBuilder {
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::new(
            adam_utils_cargo_bin::cargo_bin("adam-exec").expect("should find binary for adam-exec"),
        );
        cmd.current_dir(self.cwd.path())
            .env("ADAM_HOME", self.home.path())
            .env("OPENAI_API_KEY", "dummy");
        cmd
    }
    pub fn cmd_with_server(&self, server: &MockServer) -> assert_cmd::Command {
        let mut cmd = self.cmd();
        let base = format!("{}/v1", server.uri());
        cmd.env("OPENAI_BASE_URL", base);
        cmd
    }

    pub fn cwd_path(&self) -> &Path {
        self.cwd.path()
    }
    pub fn home_path(&self) -> &Path {
        self.home.path()
    }
}

pub fn test_codex_exec() -> TestCodexExecBuilder {
    let home = TempDir::new().expect("create temp home");
    std::fs::write(home.path().join("models.json"), r#"{"providers":{}}"#)
        .expect("write test models.json");
    TestCodexExecBuilder {
        home,
        cwd: TempDir::new().expect("create temp cwd"),
    }
}
