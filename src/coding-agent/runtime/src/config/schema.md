# Config JSON Schema

We generate a JSON Schema for `~/.adam/config.toml` from the `ConfigToml` type
and commit it at `src/coding-agent/runtime/config.schema.json` for editor integration.

When you change any fields included in `ConfigToml` (or nested config types),
regenerate the schema:

```
just write-config-schema
```
