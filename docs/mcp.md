# MCP Configuration

Adam can connect to Model Context Protocol (MCP) servers and expose their tools
to the agent. The preferred way to manage MCP servers is with the `adam mcp`
commands, but advanced users can edit `config.toml` directly.

## Configuration locations

Adam reads MCP server definitions from the same configuration layers as the rest
of the agent runtime. The global user config is `${ADAM_HOME}/config.toml`,
usually `~/.adam/config.toml`.

Project-specific configuration can also live in `.adam/config.toml` inside a
project. Project config is loaded only in trusted project contexts. Use global
configuration for personal MCP servers and project configuration for servers
that should apply only to one repository.

`adam mcp add` writes MCP server entries to the global config. If you edit TOML
by hand while a long-running client is active, restart the client or reload MCP
configuration before expecting new servers or tools to appear.

## Inspect configured servers

Use the CLI to inspect the MCP servers Adam can see:

```sh
adam mcp list
adam mcp list --json
adam mcp get <name>
adam mcp get <name> --json
```

In the TUI, use `/mcp` to view available MCP tools.

## Add a local stdio server

Stdio MCP servers are launched locally and communicate with Adam over
stdin/stdout. This is the common transport for local filesystem, browser,
database, and custom development tools.

Add a local server with a command after `--`:

```sh
adam mcp add filesystem -- npx -y @modelcontextprotocol/server-filesystem /path/to/allowed/dir
```

Pass environment variables to the launched process when needed:

```sh
adam mcp add my-server --env API_KEY=secret -- my-mcp-command --flag value
```

The equivalent TOML shape is:

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
startup_timeout_sec = 20
tool_timeout_sec = 60
```

For servers that need a working directory or inherited secrets, use `cwd`,
`env_vars`, and `env`:

```toml
[mcp_servers.my_server]
command = "node"
args = ["dist/server.js"]
cwd = "/path/to/server"
env_vars = ["API_KEY"]

[mcp_servers.my_server.env]
NODE_ENV = "production"
```

`env_vars` copies values from Adam's current environment. The `env` table sets
explicit environment variables for the MCP process.

## Add a streamable HTTP server

Streamable HTTP MCP servers are reached by URL instead of being launched as a
local process.

```sh
adam mcp add issues --url https://example.com/mcp
```

If the server expects an HTTP bearer token, provide the name of an environment
variable that contains the token:

```sh
adam mcp add issues --url https://example.com/mcp --bearer-token-env-var GITHUB_TOKEN
```

The equivalent TOML shape is:

```toml
[mcp_servers.issues]
url = "https://example.com/mcp"
bearer_token_env_var = "GITHUB_TOKEN"
```

For other headers, use static headers or headers sourced from environment
variables:

```toml
[mcp_servers.issues]
url = "https://example.com/mcp"

[mcp_servers.issues.http_headers]
"X-Client" = "adam"

[mcp_servers.issues.env_http_headers]
"X-API-Key" = "ISSUES_API_KEY"
```

## Authenticate with OAuth

OAuth is supported for streamable HTTP MCP servers that expose OAuth login
metadata.

Start with a streamable HTTP server entry:

```toml
mcp_oauth_credentials_store = "auto"

[mcp_servers.remote]
url = "https://example.com/mcp"
```

Then run:

```sh
adam mcp login remote
```

Request scopes on the command line when the server requires them:

```sh
adam mcp login remote --scopes scope1,scope2
```

You can also store default scopes in TOML:

```toml
[mcp_servers.remote]
url = "https://example.com/mcp"
scopes = ["scope1", "scope2"]
```

Remove stored OAuth credentials with:

```sh
adam mcp logout remote
```

OAuth credentials are stored according to `mcp_oauth_credentials_store`:

- `auto`: use the OS keyring when available and fall back to file storage.
- `keyring`: require the OS keyring.
- `file`: store credentials in the Adam home directory.

By default, Adam binds an ephemeral local callback port during OAuth login. Set
a fixed callback port only if your environment requires one:

```toml
mcp_oauth_callback_port = 4321
```

## Enable, disable, and filter tools

Each MCP server can be enabled, disabled, or filtered:

```toml
[mcp_servers.docs]
command = "docs-mcp"
enabled = true
enabled_tools = ["search", "open"]
disabled_tools = ["delete"]
```

Set `enabled = false` to keep a server in config without initializing it.
`enabled_tools` is an allow-list: when set, only those tools are exposed.
`disabled_tools` is then applied as a deny-list on top of the enabled set.

## Timeouts

Use startup and tool-call timeouts for slow servers:

```toml
[mcp_servers.slow_server]
command = "slow-mcp"
startup_timeout_sec = 30
tool_timeout_sec = 120
```

`startup_timeout_sec` controls how long Adam waits for the server to initialize
and list its tools. `tool_timeout_sec` controls the default timeout for MCP tool
calls made through that server.

## Security notes

- Do not commit secrets in `config.toml`.
- Do not use inline `bearer_token`; use `bearer_token_env_var` or
  `env_http_headers` instead.
- Prefer `env_vars` for inherited local-process secrets.
- Keep project `.adam/config.toml` secret-free if the project config is
  committed.
- For OAuth, prefer `auto` or `keyring` unless file storage is required.

## Troubleshooting

If a server does not appear:

- Run `adam mcp list`.
- Check that the server name contains only letters, numbers, `-`, or `_`.
- If the server is in `.adam/config.toml`, check that the project context is
  trusted.

If tools do not appear:

- Run `adam mcp get <name> --json`.
- Open `/mcp` in the TUI.
- Increase `startup_timeout_sec` for slow servers.
- Check `enabled_tools` and `disabled_tools` filters.

If HTTP authentication fails:

- Ensure the token environment variable is exported before launching Adam.
- Check `bearer_token_env_var` and `env_http_headers`.
- For OAuth servers, rerun `adam mcp login <name>`.

If a local command fails:

- Ensure the executable is on `PATH`.
- Use absolute paths when command resolution is ambiguous.
- Set `cwd` for servers that expect a specific working directory.

## Example complete configuration

This example combines a local stdio server, an HTTP server using a bearer token,
and an OAuth-capable HTTP server:

```toml
mcp_oauth_credentials_store = "auto"

[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/project"]
startup_timeout_sec = 20
tool_timeout_sec = 60

[mcp_servers.issues]
url = "https://example.com/issues/mcp"
bearer_token_env_var = "GITHUB_TOKEN"
enabled_tools = ["search_issues", "open_issue"]

[mcp_servers.remote]
url = "https://example.com/mcp"
scopes = ["read", "write"]
tool_timeout_sec = 120
```
