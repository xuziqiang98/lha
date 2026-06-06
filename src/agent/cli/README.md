# lha

Long-Horizon Agent product package. This crate installs the `lha` command and
contains the full CLI/TUI product surface.

```sh
cargo install lha --locked
```

## Package contents

`lha` depends on the reusable `lha-llm` and `lha-core` crates, then layers on
the product modules for command-line dispatch, the TUI, local state, sandboxing,
MCP server/client management, memories, skills, telemetry, and bundled tools.

## Binary

The package name and installed binary are both `lha`.

## Documentation

- Root project overview: <https://github.com/xuziqiang98/lha/blob/main/README.md>
- Slash commands: <https://github.com/xuziqiang98/lha/blob/main/docs/slash-commands.md>
- MCP configuration: <https://github.com/xuziqiang98/lha/blob/main/docs/mcp.md>
- Identities: <https://github.com/xuziqiang98/lha/blob/main/docs/identities.md>
