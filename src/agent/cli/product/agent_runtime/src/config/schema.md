# Config JSON Schema

We generate a JSON Schema for `~/.lha/config.toml` from the `ConfigToml` type
and commit it at `src/agent/cli/product/agent_runtime/config.schema.json` for
editor integration.

When you change any fields included in `ConfigToml` (or nested config types),
regenerate the schema:

```
just write-config-schema
```
