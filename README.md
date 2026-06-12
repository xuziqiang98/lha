## Quickstart

### Install

Install the published CLI from crates.io:

```sh
cargo install lha --locked
```

Run the installed command:

```sh
lha
```

To confirm the installed version:

```sh
lha --version
```

### Build From Source

From the repository root:

```sh
cargo build -p lha --bin lha --release

export PATH="<lha binary path>:$PATH" >> ~/.bashrc
```

The resulting binary is written to `target/release/lha`.

## Documentation

- [Identities](./docs/identities.md)
- [Slash commands](./docs/slash-commands.md)
- [MCP configuration](./docs/mcp.md)
- [SDK: Building agents with lha-llm and lha-core](./docs/sdk-building-agents.md)

## Structure

The workspace is organized under [`src/`](./src) around three crates.io publish boundaries:

- `src/llm`: `lha-llm`, the reusable model runtime SDK, provider adapters, and wire-level API clients
- `src/core`: `lha-core`, the reusable agent core APIs
- `src/agent/cli`: `lha`, the LHA product package that installs the `lha` command; its private `product/` modules include the TUI, runtime, state, app-server, MCP, sandboxing, and related tooling

To build your own agent on the reusable SDK crates, start with
[`docs/sdk-building-agents.md`](./docs/sdk-building-agents.md).

## TUI Copying

In the TUI, plain `Ctrl+C` interrupts the current turn or exits after confirmation. To copy transcript selection, drag-select text in the transcript or press `Ctrl+Shift+C`/`Cmd+C` when your terminal forwards that key. If your terminal intercepts `Ctrl+Shift+C`, hold `Shift` while dragging for terminal-native selection, or run with `--no-mouse-capture` / set `[tui] mouse_capture = false`.

This repository is licensed under the [Apache-2.0 License](LICENSE).
