# Workspace Architecture

The Rust workspace is organized around five top-level domains under `src/`:

- `src/harness`: the agent shell and execution framework. This includes the main entrypoints such as `cli`, `tui`, `exec`, `login`, and the shared `core` crate.
- `src/session`: durable task state. Session data is intended to outlive a particular harness process so work can be resumed from logs or persisted state.
- `src/sandbox`: isolated execution environments and safety enforcement. Platform sandboxes and execution policies live here.
- `src/resources`: the system's gateway to external capabilities, including tools, MCP, provider integrations, patching, search, and related clients.
- `src/orchestration`: the task coordination layer that wires components together and drives multi-step flows across the rest of the system.

## Workspace Root

The repository root is the only Cargo workspace root. Build and test commands should be run from here unless a crate-specific workflow says otherwise.

Examples:

```sh
cargo build -p codex-cli --bin codey
cargo test -p codex-tui
just fmt
```

Build artifacts are emitted to the root `target/` directory, for example:

- debug build: `target/debug/codey`
- release build: `target/release/codey`
