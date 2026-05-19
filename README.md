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

## Documentation

- [MCP configuration](./docs/mcp.md)

## Structure

The workspace is organized under [`src/`](./src) with seven top-level domains:

- `llm`: model runtime SDK, provider adapters, and wire-level API clients
- `core`: shared protocol and session state types
- `coding-agent`: Adam-specific runtime, CLI, login, and product logic
- `tui`: terminal user interface application
- `integrations`: app-server, MCP, backend, and cloud-facing adapters
- `platform`: sandboxing, execution, and IPC primitives
- `shared`: reusable support crates and utilities

## TUI Copying

In the TUI, plain `Ctrl+C` interrupts the current turn or exits after confirmation. To copy transcript selection, drag-select text in the transcript or press `Ctrl+Shift+C`/`Cmd+C` when your terminal forwards that key. If your terminal intercepts `Ctrl+Shift+C`, hold `Shift` while dragging for terminal-native selection, or run with `--no-mouse-capture` / set `[tui] mouse_capture = false`.

This repository is licensed under the [Apache-2.0 License](LICENSE).
