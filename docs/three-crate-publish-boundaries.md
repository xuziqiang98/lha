# Three-Crate Publish Boundary Refactor

This document describes the target publishing shape for reducing the current
multi-crate workspace into three crates.io packages:

- `lha-llm`
- `lha-core`
- `lha-cli`

The goal is not to immediately move code. The goal is to make the intended
publish boundaries explicit so future refactors can move code without
re-deciding what belongs where.

## Goals

- Publish only three public crates.io packages for the main product path.
- Keep `lha-llm` reusable as a model API and semantic runtime SDK.
- Keep `lha-core` reusable as a minimal agent SDK with tools, lightweight
  skills, and optional MCP-to-tool support.
- Keep `lha-cli` as the full LHA product package that installs the `lha`
  command.
- Avoid publishing internal helper crates solely because they are current
  workspace package boundaries.
- Ensure each public package can eventually pass `cargo package` without
  unpublished internal path dependencies.

## Non-goals

- Do not change Rust code as part of this planning document.
- Do not preserve every current workspace crate as a crates.io package.
- Do not make `lha-core` a full product core with TUI, CLI, persistence,
  sandbox UX, or LHA-specific protocol events.
- Do not move LHA product telemetry, OAuth, config loading, or bundled skill
  management into the minimal SDK.
- Do not change the installed command name; `lha-cli` still installs `lha`.

## Target crates

### `lha-llm`

`lha-llm` is the reusable model-facing SDK. It owns provider configuration,
wire API clients, streaming, model catalog support, semantic runtime sessions,
and semantic LLM types.

It should not depend on LHA product-layer crates such as telemetry, protocol,
state, TUI, sandboxing, or MCP server configuration.

### `lha-core`

`lha-core` is the minimal agent SDK. It owns the in-memory agent loop, session
lifecycle, input queues, event stream, turn execution, tool registry, tool
execution, lightweight skills abstractions, and optional MCP-to-tool adapters.

It can depend on `lha-llm` and third-party crates from crates.io. Its default
feature set should stay small and product-neutral.

### `lha-cli`

`lha-cli` is the full LHA product package. It depends on `lha-llm` and
`lha-core`, contains the remaining product code, and keeps the binary target
named `lha`.

Users install it with:

```sh
cargo install lha-cli --locked
```

## Repository and dependency style

The three publishable crates do not require three Git repositories. They should
remain in one repository and one Cargo workspace unless there is a separate
product reason to split source control.

The publishable crates should depend on each other with path plus version
requirements. Local development uses the `path`; crates.io packaging strips the
local path and keeps the registry `version`.

Target workspace dependency shape:

```toml
[workspace.dependencies]
lha-llm = { path = "src/llm", version = "1.0.0" }
lha-core = { path = "src/core", version = "1.0.0" }
```

Target `lha-core` dependency shape:

```toml
[dependencies]
lha-llm = { workspace = true }
```

Target `lha-cli` dependency shape:

```toml
[dependencies]
lha-llm = { workspace = true }
lha-core = { workspace = true }
```

In the source checkout, Cargo resolves `lha-core` to the local `src/core`
package and `lha-llm` to the local `src/llm` package. In the packaged crate,
the same dependency becomes a registry dependency, for example:

```toml
lha-llm = "1.0.0"
```

This avoids maintaining separate local and publishing manifests.

## Current crate mapping

The current workspace has many package boundaries that should become modules
inside one of the three public publish boundaries.

### Maps to `lha-llm`

- `src/llm/runtime` -> `lha-llm`
- `src/llm/api/codex-api` -> `lha-llm::api`
- `src/llm/api/codex-client` -> `lha-llm::client`
- `src/llm/types` -> `lha-llm::types`

### Maps to `lha-core`

- `src/core/agent-core` -> `lha-core::kernel`
- `src/core/agent-runtime` -> `lha-core`
- selected helper code from `src/shared/async-utils` -> private `lha-core`
  module
- selected MCP protocol types and adapters from `src/integrations/mcp-types`
  -> optional `lha-core::mcp`

### Maps to `lha-cli`

Everything product-facing remains in the `lha-cli` publish boundary, including:

- `src/agent/runtime`
- `src/agent/cli`
- `src/agent/feedback`
- `src/agent/identity`
- `src/agent/memories/read`
- `src/agent/memories/write`
- `src/core/protocol`
- `src/core/state`
- `src/integrations/app-server`
- `src/integrations/app-server-protocol`
- `src/integrations/mcp-server`
- `src/integrations/rmcp-client`
- `src/integrations/responses-api-proxy`
- `src/platform/exec`
- `src/platform/ipc/stdio-to-uds`
- `src/platform/sandbox/*`
- `src/resources/apply-patch`
- `src/resources/file-search`
- `src/resources/keyring-store`
- `src/shared/*`
- `src/tui/app`

Dev/test-only helper crates should remain unpublished and may stay as test
helpers or private modules:

- `core_test_support`
- `app_test_support`
- `mcp_test_support`
- `lha-utils-cargo-bin`

## lha-llm boundary

`lha-llm` owns:

- model API requests
- provider and wire API selection
- streaming response handling
- model catalog refresh and metadata
- runtime sessions
- semantic transcript items
- tool descriptors
- turn requests and turn events
- model/runtime metadata

The current `lha-llm`, `lha-api`, `lha-client`, and `lha-llm-types` crates
should be folded into one package. The public surface should continue to expose
semantic concepts:

```rust
use lha_llm::SemanticRuntime;
use lha_llm::SemanticRuntimeSession;
use lha_llm::ToolDescriptor;
use lha_llm::TranscriptItem;
use lha_llm::TurnEvent;
use lha_llm::TurnRequest;
```

Product coupling to remove:

- direct dependency on `lha-otel`
- unused `lha-git` or `mcp-types` dependencies in LLM types
- any LHA protocol or MCP product types that leak into model-only APIs

Telemetry should be represented by generic hooks or traits inside `lha-llm`.
The `lha-cli` product layer is responsible for adapting those hooks to the
current `lha-otel` implementation.

## lha-core boundary

`lha-core` is the minimal agent SDK. It owns:

- low-level turn-stream kernel
- stateful in-memory sessions
- agent manager and session lifecycle
- primary, steering, and follow-up input queues
- generic agent events
- session snapshots
- tool registration and execution
- lightweight skills abstractions
- optional MCP-to-tool adapter support

The current `lha-agent-core` and `lha-agent-runtime` crates should be folded
into a single `lha-core` package. Imports should eventually move from
`lha_agent_runtime::*` to `lha_core::*`.

Future public API direction:

```rust
use lha_core::AgentBuilder;
use lha_core::AgentEvent;
use lha_core::AgentManager;
use lha_core::AgentSession;
use lha_core::SessionInput;
use lha_core::skills::Skill;
use lha_core::skills::SkillProvider;
use lha_core::tools::ToolHandler;
use lha_core::tools::ToolInvocation;
use lha_core::tools::ToolOutput;
```

`lha-core` must not contain:

- TUI
- CLI
- sandbox or exec approval product UX
- rollout persistence
- SQLite-backed state
- LHA `Op` or `EventMsg` product protocol
- MCP server lifecycle config
- MCP OAuth/login UX
- skill installer, bundled skill assets, or dependency install prompts
- telemetry backend

## lha-cli boundary

`lha-cli` is the complete LHA product. It owns:

- CLI parsing and binary entrypoint
- TUI
- LHA product runtime and compatibility wrappers
- config loading and profiles
- sandboxing and exec approval UX
- persistence, rollouts, state, memories, and goals
- MCP server management and LHA-as-MCP-server
- app-server integration
- telemetry backend
- built-in tools and product-specific tool orchestration
- local skills directory, skill installer, bundled skills, and skill dependency
  prompts

`lha-cli` depends on the public `lha-llm` and `lha-core` crates, plus
third-party crates from crates.io. It should not depend on unpublished sibling
workspace crates in the final publishable shape.

Package and binary names:

- package: `lha-cli`
- binary: `lha`
- install command: `cargo install lha-cli --locked`

## Tools, Skills, and MCP

### Tools

Tools are core agent-loop functionality and belong in `lha-core`.

`lha-core` should keep the current generic concepts:

- `ToolRegistry`
- `ToolRegistryBuilder`
- `ToolHandler`
- `ToolInvocation`
- `ToolPayload`
- `ToolOutput`
- `ToolError`
- parallel tool call support

Target trait shape:

```rust
pub trait ToolHandler: Send + Sync {
    fn spec(&self) -> lha_llm::ToolDescriptor;
    fn supports_parallel_tool_calls(&self) -> bool;

    async fn handle(
        &self,
        invocation: ToolInvocation,
        cancellation_token: CancellationToken,
    ) -> Result<ToolOutput, ToolError>;
}
```

Product-specific tools such as shell execution, apply_patch, memories, image
generation, delegated jobs, and request-user-input remain in `lha-cli`.

### Skills

Skills should have a lightweight SDK abstraction in `lha-core`, but the LHA
product skill system remains in `lha-cli`.

`lha-core` owns:

- skill metadata
- skill instructions
- skill selection hooks
- turn-context instruction injection points
- declarations of required tool capabilities

Target API direction:

```rust
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub instructions: String,
}

pub trait SkillProvider: Send + Sync {
    async fn skills_for_turn(&self, context: &SkillContext) -> Result<Vec<Skill>, SkillError>;
}
```

`lha-cli` owns:

- `$LHA_HOME/skills`
- skill installer
- bundled sample skills
- skill dependency prompts
- skill MCP dependency install
- skill assets
- product-specific enable/disable policy

### MCP

MCP support should enter `lha-core` as an optional MCP-to-tool adapter, not as a
default product-level subsystem.

The current LHA MCP client path already behaves as an adapter:

```text
MCP server tool
  -> converted to lha_llm::ToolDescriptor
  -> model calls a normal function tool
  -> core/tool handler dispatches call
  -> adapter calls MCP tools/call
  -> MCP result becomes LLM tool result
```

The target feature gate is:

```toml
lha-core = { version = "...", features = ["mcp"] }
```

The `mcp` feature should provide:

- MCP protocol types needed by the SDK, folded under `lha_core::mcp::types`
  instead of publishing `mcp-types` separately
- MCP tool-name qualification helpers, such as `mcp__server__tool`
- MCP tool-to-`ToolDescriptor` conversion
- `McpClient` trait
- `McpToolProvider` adapter over that trait

Target API direction:

```rust
pub mod mcp {
    pub trait McpClient: Send + Sync {
        async fn list_tools(&self) -> Result<Vec<McpTool>, McpError>;

        async fn call_tool(
            &self,
            tool_name: &str,
            arguments: Option<serde_json::Value>,
        ) -> Result<McpCallToolResult, McpError>;
    }

    pub struct McpToolProvider<C> {
        pub server_name: String,
        pub client: C,
    }
}
```

`lha-cli` keeps:

- `config.toml` `mcp_servers`
- stdio and streamable HTTP server startup
- OAuth login
- GitHub MCP PAT hint
- startup timeout UI
- resource tools UX
- MCP approval prompt
- MCP telemetry
- memory pollution marking
- LHA-as-MCP-server (`lha mcp-server`)

## Refactor phases

### Phase 1: Consolidate `lha-llm`

- Create a single publishable `lha-llm` package.
- Move current API, client, types, and runtime modules under one package.
- Preserve semantic public re-exports where possible.
- Remove the direct `lha-otel` dependency.
- Replace product telemetry with generic hooks.
- Ensure `cargo package -p lha-llm --no-verify` does not require any internal
  unpublished crate.

### Phase 2: Consolidate `lha-core`

- Create a publishable `lha-core` package.
- Move the `lha-agent-core` kernel into `lha_core::kernel`.
- Move the `lha-agent-runtime` session runtime into `lha_core`.
- Rename public imports from `lha_agent_runtime::*` to `lha_core::*`.
- Add a `skills` module with lightweight abstractions.
- Add an optional `mcp` feature with adapter traits and conversion helpers.
- Ensure default `lha-core` depends on `lha-llm` and third-party crates only.
- Ensure `cargo package -p lha-core --no-verify` does not require any internal
  unpublished crate.

### Phase 3: Collapse product crates into `lha-cli`

- Keep package name `lha-cli`.
- Keep binary name `lha`.
- Move or inline remaining product crates so final `lha-cli` depends only on:
  - `lha-llm`
  - `lha-core`
  - third-party crates from crates.io
- Keep product modules private unless intentionally public.
- Keep current CLI behavior stable.

### Phase 4: Publishing readiness

- Add required package metadata to all three public crates.
- Remove Git dependencies and `[patch]` entries that block crates.io publishing.
- Run package dry-runs for all three crates.
- Validate local install from the CLI package path.

### Phase 5: Publish in dependency order

- Publish `lha-llm` first.
- Wait until crates.io index resolution can see the published `lha-llm`
  version.
- Publish `lha-core` second; its packaged dependency on `lha-llm` resolves from
  crates.io.
- Wait until crates.io index resolution can see the published `lha-core`
  version.
- Publish `lha-cli` last; its packaged dependencies on `lha-core` and
  `lha-llm` resolve from crates.io.

## Publishing readiness

Before publishing, each public package must pass:

```sh
cargo package -p lha-llm --no-verify
cargo package -p lha-core --no-verify
cargo package -p lha-cli --no-verify
```

Validate the CLI install path:

```sh
cargo install --path src/agent/cli --locked --force
lha --version
```

Publish in dependency order:

```sh
cargo publish -p lha-llm

# Wait until crates.io can resolve lha-llm 1.0.0.
cargo publish -p lha-core

# Wait until crates.io can resolve lha-core 1.0.0.
cargo publish -p lha-cli
```

Use `cargo publish --dry-run` for each package before the real publish. Do not
publish `lha-core` until the `lha-llm` version it depends on is visible to
Cargo, and do not publish `lha-cli` until both `lha-core` and `lha-llm` are
visible.

The final published dependency graph should be:

```text
lha-cli
  -> lha-core
      -> lha-llm
  -> lha-llm
  -> third-party crates.io crates
```

No final public package should depend on unpublished internal path crates.

## Validation

Docs-only changes to this plan do not require Rust formatting or tests.

For future code movement phases, validate with:

```sh
cargo test -p lha-llm
cargo test -p lha-core
cargo test -p lha-cli
```

When shared, core, or protocol code has moved, ask before running the full
suite, then run:

```sh
cargo test --all-features
```

Before publishing, also validate package and install commands:

```sh
cargo package -p lha-llm --no-verify
cargo package -p lha-core --no-verify
cargo package -p lha-cli --no-verify
cargo install --path src/agent/cli --locked --force
lha --version
```

## Open risks

- `lha-llm` currently has product telemetry coupling through `lha-otel`; this
  must be replaced with generic hooks before it can be a clean reusable SDK.
- Git dependencies and workspace `[patch]` entries may still block crates.io
  publishing until replaced by registry dependencies or upstreamed changes.
- `lha-core` needs a careful public API review because it will become the
  stable minimal agent SDK boundary.
- Moving `mcp-types` under `lha-core::mcp::types` may require compatibility
  adapters for existing product code and tests.
- Collapsing product crates into `lha-cli` will require a staged module move to
  preserve current CLI, TUI, app-server, and sandbox behavior.
- Package names on crates.io are immutable once published; name availability
  must be checked before any real publish.
- Published versions on crates.io are immutable. If a bad version is published,
  yank it and release a new patch version rather than trying to overwrite it.
