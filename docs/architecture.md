# Workspace Architecture

The Rust workspace is organized around the current top-level domains under `src/`:

- `src/llm`: the LLM-facing SDK boundary.
  - `src/llm/api` owns wire-level API clients and provider protocol details.
  - `src/llm/runtime` exposes the semantic runtime interface consumed by the agent layer.
  - `src/llm/providers` is reserved for provider-specific adapters when needed.
- `src/core`: cross-surface product primitives that should not depend on a specific UI.
  - `src/core/protocol` defines shared protocol types.
  - `src/core/state` owns durable state and storage primitives.
- `src/coding-agent`: the Codex agent runtime and product logic.
  - `src/coding-agent/runtime` contains the turn loop, tool orchestration, model management, config, and prompt assembly.
  - `src/coding-agent/cli`, `login`, `feedback`, and `chatgpt` provide supporting product surfaces around that runtime.
- `src/tui`: the terminal UI surface.
  - `src/tui/app` is the interactive TUI built on top of `codex-coding-agent`.
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

`src/llm` -> `src/core` -> `src/coding-agent` -> surface crates such as `src/tui/app`, `src/platform/exec`, and `src/integrations/app-server`

The important boundary in this stack is that `codex-coding-agent` should talk to `codex-llm` as an SDK, not by reaching into provider-specific internals.

## Intended Dependency Direction

The intended dependency flow is:

- `src/shared` and `src/resources` stay near the leaves.
- `src/llm` provides model-facing SDK primitives.
- `src/core` provides product-neutral protocol and state primitives.
- `src/coding-agent` owns the agent behavior and orchestration.
- UI and protocol surfaces such as `src/tui`, `src/platform/exec`, and `src/integrations/app-server` sit above the coding-agent runtime.

This is the target mental model for the workspace. Some older crates still reflect historical layering decisions, but new work should follow this direction.

## Current Boundary Note

Today, `src/coding-agent/runtime` still contains both the reusable agent loop and Codex-specific coding-agent policy. That is an acceptable intermediate state, but the directory layout should be read as:

- `src/llm`: model SDK boundary
- `src/core`: shared product primitives
- `src/coding-agent`: agent orchestration and product behavior
- `src/tui`: presentation layer

If the agent loop is split further in the future, it should remain between `src/core` and the product-specific parts of `src/coding-agent`, without collapsing the existing `src/llm` SDK boundary.

## Workspace Root

The repository root is the only Cargo workspace root. Build and test commands should be run from here unless a crate-specific workflow says otherwise.

Examples:

```sh
cargo build -p codex-cli --bin codey
cargo test -p codex-tui
just fmt
```

Build artifacts are emitted to the root `target/` directory, for example:

- debug build: `target/debug/codey`
- release build: `target/release/codey`
