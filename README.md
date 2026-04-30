## Quickstart

### Building
From the repository root:

```
cargo build -p adam-cli --bin adam --release
cargo build -p adam-exec --bin adam-exec --release

export PATH="<adam binary path>:$PATH" >> ~/.bashrc
```

The resulting binary is written to `target/release/adam`.
The `adam-exec` binary is written to `target/release/adam-exec`.

## Structure

The workspace is organized under [`src/`](./src) with seven top-level domains:

- `llm`: model runtime SDK, provider adapters, and wire-level API clients
- `core`: shared protocol and session state types
- `coding-agent`: Adam-specific runtime, CLI, login, and product logic
- `tui`: terminal user interface application
- `integrations`: app-server, MCP, backend, and cloud-facing adapters
- `platform`: sandboxing, execution, and IPC primitives
- `shared`: reusable support crates and utilities

This repository is licensed under the [Apache-2.0 License](LICENSE).
