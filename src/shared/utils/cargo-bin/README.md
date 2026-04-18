# codex-utils-cargo-bin

Helpers for Cargo-based test runs in this workspace.

Function behavior:
- `cargo_bin`: reads `CARGO_BIN_EXE_*` environment variables and returns the
  resolved absolute path to a first-party binary. If those variables are not
  present, it falls back to `assert_cmd::Command::cargo_bin(...)`.
- `find_resource!`: resolves test fixtures relative to the calling crate's
  `CARGO_MANIFEST_DIR`, which keeps fixture lookup working even after tests
  change the process working directory.
- `repo_root`: walks upward from the local `repo_root.marker` file until it
  finds the repository root.
