# lha-cli

Long-Horizon Agent product package. This crate installs the `lha` command and
contains the full CLI/TUI product surface.

```sh
cargo install lha-cli --locked
```

## Package contents

`lha-cli` depends on the reusable `lha-llm` and `lha-core` crates, then layers on
the product modules for command-line dispatch, the TUI, local state, sandboxing,
MCP server/client management, memories, skills, telemetry, and bundled tools.

## Binary

The package name is `lha-cli`, but the installed binary is `lha`.

## Documentation

- Root project overview: <https://github.com/xuziqiang98/lha/blob/main/README.md>
- Slash commands: <https://github.com/xuziqiang98/lha/blob/main/docs/slash-commands.md>
- MCP configuration: <https://github.com/xuziqiang98/lha/blob/main/docs/mcp.md>
- Identities: <https://github.com/xuziqiang98/lha/blob/main/docs/identities.md>
