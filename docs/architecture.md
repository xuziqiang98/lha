# Workspace Architecture

The Rust workspace is organized around three main package boundaries under
`src/`:

- `src/llm`: the LLM-facing SDK boundary.
  - `src/llm/src/api` owns wire-level API clients and provider protocol details.
  - `src/llm/src/client` owns generic HTTP transport, retry, SSE, and multipart primitives.
  - `src/llm/src/types` owns model catalog, transcript, usage, and config semantics.
  - `src/llm/src` exposes the semantic runtime interface consumed by the agent layer.
    Its public surface should remain semantic and provider-neutral rather than
    re-exporting protocol compatibility helpers or wire-specific request types.
  - `src/llm/providers` is reserved for provider-specific adapters when needed.
- `src/core`: reusable agent SDK primitives that should not depend on a
  specific product UI.
  - `src/core` owns the `lha-core` publish boundary: the reusable agent-loop kernel, stateful in-memory agent SDK, lightweight skills API, and optional MCP SDK skeleton.
  - `src/core/agent-core` and `src/core/agent-runtime` are temporary compatibility shims over `lha-core`.
- `src/agent/cli`: the full LHA product package.
  - `src/agent/cli/src` owns the public `lha` binary entrypoint, CLI parsing,
    and intentionally public sandbox debug command types.
  - `src/agent/cli/product` contains private product modules for the LHA
    runtime, TUI, protocol, app-server, state, MCP, sandboxing, helper
    adapters, and low-level product utilities.
  - `src/agent/cli/product/agent_runtime` contains LHA-specific agent policy,
    tool orchestration, model management, config, persistence integration, and
    prompt assembly.
  - `src/agent/cli/product/tui_app`, `exec_cli`, `app_server`,
    `app_server_protocol`, `mcp_server`, `rmcp_client`, `responses_api_proxy`,
    `state`, `protocol`, and `windows_sandbox` are private modules inside the
    `lha` package, not separate publishable crates.

## Primary Product Stack

The main product path is:

`src/llm` -> `src/core` -> `src/agent/cli/product` -> `src/agent/cli/src`

The important boundary in this stack is that the LHA product runtime should
talk to `lha-llm` as an SDK, not by reaching into provider-specific internals.
The reusable turn-stream kernel and higher-level reusable session runtime now live in `lha-core`, so new agent products should prefer building on that layer instead of reimplementing the loop inside a product crate.
In particular, `lha-llm` should expose semantic tool descriptors, turn requests,
and turn events, while conversion to provider-specific payloads stays inside
`src/llm/src/api` and the runtime modules under `src/llm/src`.
Tool names such as `local_shell` should remain generic function-tool names at
the `lha-llm` boundary; product defaults such as sandbox policy belong above
that SDK boundary.

## Target crates.io publish boundaries

The long-term crates.io publishing target is three public packages:

- `lha-llm`: reusable model API and semantic runtime SDK.
- `lha-core`: minimal agent SDK rooted at `src/core`, with tools, lightweight
  skills, and optional MCP-to-tool support. `src/core/agent-core` and
  `src/core/agent-runtime` are compatibility shims.
- `lha`: the complete LHA product package that depends on `lha-llm` and
  `lha-core` and installs the `lha` command.

See [Three-Crate Publish Boundary Refactor](./three-crate-publish-boundaries.md)
for the detailed mapping from current workspace crates to these publish
boundaries.

## Product Philosophy

LHA's product model is a single main agent. Isolated model work such as
exploration or review should run as bounded CLI-backed one-shot jobs, not as
long-lived sub-agent sessions or model-visible collaboration. See
[Design Philosophy](./design-philosophy.md) for the design principles and naming
rules.

## Intended Dependency Direction

The intended dependency flow is:

- `src/llm` provides model-facing SDK primitives.
- `src/core` provides product-neutral agent-loop primitives and SDK surface.
- `src/agent/cli/product` owns LHA-specific agent behavior and orchestration
  on top of those primitives.
- UI, CLI, protocol, app-server, sandbox, and helper modules under
  `src/agent/cli/product` sit above the coding-agent runtime while remaining
  private to the `lha` package.

This is the target mental model for the workspace. Some older crates still reflect historical layering decisions, but new work should follow this direction.

## Current Boundary Note

Today, `src/agent/cli/product/agent_runtime` still contains substantial
LHA-specific policy around the reusable loop. The current directory layout
should be read as:

- `src/llm`: model SDK boundary
- `src/core`: shared product primitives, including the extracted agent-loop kernel and the reusable in-memory agent runtime
- `src/agent/cli`: LHA product package, including orchestration, protocol,
  app-server, sandboxing, TUI, and helper behavior

Follow-on extractions should continue to live between `src/core` and the
product-specific parts of `src/agent/cli/product`, without collapsing the
existing `src/llm` SDK boundary. Today that reusable session/runtime layer is
`lha-core`.

Workflow identities are an LHA-specific runtime policy in this layering. Their
shared protocol and rollout types now live in the private product protocol
module, while workflow state machines, artifact validation, tool filtering, and
identity-specific prompt injection belong in
`src/agent/cli/product/agent_runtime`. See
`docs/workflow-identities.md` for the detailed design. They should not move into
`lha-core` until the reusable/product-specific boundary is clearer.

The important nuance is that this extraction is not yet the same thing as
rewriting the product runtime. `lha-core` and `lha-llm` now provide the cleaner
SDK-facing layer, while `src/agent/cli/product/agent_runtime` still owns the
main LHA session loop, persistence integration, and product-specific tool
behavior. `ThreadManager` and `CodexThread` therefore remain LHA-facing
compatibility wrappers over the existing product runtime rather than a full
rewrite on top of `lha-core`.

## Workspace Root

The repository root is the only Cargo workspace root. Build and test commands should be run from here unless a crate-specific workflow says otherwise.

Examples:

```sh
cargo build -p lha --bin lha
cargo test -p lha
just fmt
```

Build artifacts are emitted to the root `target/` directory, for example:

- debug build: `target/debug/lha`
- release build: `target/release/lha`
