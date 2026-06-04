# Workspace Architecture

The Rust workspace is organized around the current top-level domains under `src/`:

- `src/llm`: the LLM-facing SDK boundary.
  - `src/llm/src/api` owns wire-level API clients and provider protocol details.
  - `src/llm/src/client` owns generic HTTP transport, retry, SSE, and multipart primitives.
  - `src/llm/src/types` owns model catalog, transcript, usage, and config semantics.
  - `src/llm/src` exposes the semantic runtime interface consumed by the agent layer.
    Its public surface should remain semantic and provider-neutral rather than
    re-exporting protocol compatibility helpers or wire-specific request types.
  - `src/llm/providers` is reserved for provider-specific adapters when needed.
- `src/core`: cross-surface product primitives that should not depend on a specific UI.
  - `src/core` owns the `lha-core` publish boundary: the reusable agent-loop kernel, stateful in-memory agent SDK, lightweight skills API, and optional MCP SDK skeleton.
  - `src/core/agent-core` and `src/core/agent-runtime` are temporary compatibility shims over `lha-core`.
  - `src/core/protocol` defines shared protocol types.
  - `src/core/state` owns durable state and storage primitives.
- `src/agent`: the LHA agent runtime and product logic.
  - `src/agent/runtime` contains LHA-specific agent policy, tool orchestration, model management, config, and prompt assembly.
  - `src/agent/cli`, `login`, `feedback`, and `chatgpt` provide supporting product surfaces around that runtime.
- `src/tui`: the terminal UI surface.
  - `src/tui/app` is the interactive TUI built on top of `lha-agent`.
- `src/platform`: process- and OS-facing execution layers.
  - `src/platform/exec` is the headless execution surface.
  - `src/platform/sandbox` and `src/platform/ipc` contain sandboxing and IPC support.
- `src/integrations`: external protocols and service adapters.
  - This includes `app-server`, MCP-facing crates, cloud adapters, and the responses proxy.
- `src/resources`: focused capability crates that can be reused by higher layers.
  - Examples include `apply-patch`, `file-search`, `keyring-store`, and generated backend models.
- `src/shared`: low-level shared utilities and helpers that should stay policy-light.
  - Examples include `common`, `otel`, `arg0`, `async-utils`, and `shared/utils/*`.

## Primary Product Stack

The main product path is:

`src/llm` -> `src/core` -> `src/agent` -> surface crates such as `src/tui/app`, `src/platform/exec`, and `src/integrations/app-server`

The important boundary in this stack is that `lha-agent` should talk to `lha-llm` as an SDK, not by reaching into provider-specific internals.
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
- `lha-cli`: the complete LHA product package that depends on `lha-llm` and
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

- `src/shared` and `src/resources` stay near the leaves.
- `src/llm` provides model-facing SDK primitives.
- `src/core` provides product-neutral protocol, state, and agent-loop primitives.
- `src/agent` owns LHA-specific agent behavior and orchestration on top of those primitives.
- UI and protocol surfaces such as `src/tui`, `src/platform/exec`, and `src/integrations/app-server` sit above the coding-agent runtime.

This is the target mental model for the workspace. Some older crates still reflect historical layering decisions, but new work should follow this direction.

## Current Boundary Note

Today, `src/agent/runtime` still contains substantial LHA-specific policy around the reusable loop. The current directory layout should be read as:

- `src/llm`: model SDK boundary
- `src/core`: shared product primitives, including the extracted agent-loop kernel and the reusable in-memory agent runtime
- `src/agent`: LHA orchestration and product behavior
- `src/tui`: presentation layer

Follow-on extractions should continue to live between `src/core` and the product-specific parts of `src/agent`, without collapsing the existing `src/llm` SDK boundary. Today that reusable session/runtime layer is `lha-core`.

Workflow identities are an LHA-specific runtime policy in this layering. Their
shared protocol and rollout types belong in `src/core/protocol`, while workflow
state machines, artifact validation, tool filtering, and identity-specific
prompt injection belong in `src/agent/runtime`. See
`docs/workflow-identities.md` for the detailed design. They should not move into
`lha-core` until the reusable/product-specific boundary is clearer.

The important nuance is that this extraction is not yet the same thing as migrating the product runtime. `lha-core` and `lha-llm` now provide the cleaner SDK-facing layer, while `src/agent/runtime` still owns the main LHA session loop, persistence integration, and product-specific tool behavior. `ThreadManager` and `CodexThread` therefore remain LHA-facing compatibility wrappers over the existing product runtime rather than a full rewrite on top of `lha-core`.

## Workspace Root

The repository root is the only Cargo workspace root. Build and test commands should be run from here unless a crate-specific workflow says otherwise.

Examples:

```sh
cargo build -p lha-cli --bin lha
cargo test -p lha-tui
just fmt
```

Build artifacts are emitted to the root `target/` directory, for example:

- debug build: `target/debug/lha`
- release build: `target/release/lha`
