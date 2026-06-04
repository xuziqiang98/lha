#![allow(clippy::expect_used)]
use crate::product::agent::config::model_ref::ModelRef;
use crate::product::agent::config::state_json::LHAStateStore;
use std::path::Path;
use tempfile::TempDir;
use wiremock::MockServer;

const DEFAULT_BASE_URL: &str = "http://unused.local/v1";
const DEFAULT_MODEL: &str = "gpt-5.1";

pub struct TestCodexExecBuilder {
    home: TempDir,
    cwd: TempDir,
}

impl TestCodexExecBuilder {
    pub fn cmd(&self) -> assert_cmd::Command {
        write_test_model_config(self.home.path(), DEFAULT_BASE_URL);
        let mut cmd = assert_cmd::Command::new(
            crate::test_support::cargo_bin::cargo_bin("lha-exec")
                .expect("should find binary for lha-exec"),
        );
        cmd.current_dir(self.cwd.path())
            .env("LHA_HOME", self.home.path())
            .env("OPENAI_API_KEY", "dummy");
        cmd
    }
    pub fn cmd_with_server(&self, server: &MockServer) -> assert_cmd::Command {
        let base = format!("{}/v1", server.uri());
        let mut cmd = self.cmd();
        write_test_model_config(self.home.path(), &base);
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
    write_test_model_config(home.path(), DEFAULT_BASE_URL);
    TestCodexExecBuilder {
        home,
        cwd: TempDir::new().expect("create temp cwd"),
    }
}

fn write_test_model_config(home: &Path, base_url: &str) {
    let models_json = serde_json::json!({
        "providers": {
            "openai": {
                "name": "OpenAI",
                "endpoints": {
                    "main": {
                        "base_url": base_url,
                        "env_key": "OPENAI_API_KEY",
                        "dialect": "responses",
                        "supports_realtime_streaming": true,
                        "models": {
                            "gpt-5.1": {},
                            "gpt-5.1-high": {},
                            "gpt-5.2-codex": {}
                        }
                    }
                }
            }
        }
    });
    std::fs::write(
        home.join("models.json"),
        serde_json::to_string_pretty(&models_json).expect("serialize test models.json"),
    )
    .expect("write test models.json");

    LHAStateStore::new(home)
        .set_last_selected_model(&ModelRef::new("openai", "main", DEFAULT_MODEL), None, None)
        .expect("write test state.json");
}
