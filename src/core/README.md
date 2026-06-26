# lha-core

Minimal agent SDK for LHA. This package provides the reusable in-memory agent
loop and session runtime that sit between model-facing code in `lha-llm` and
product-specific orchestration in `lha`.

## Runtime and sessions

`lha-core` owns the product-neutral agent runtime:

- in-memory agent sessions and lifecycle management
- turn execution and event streams
- primary, steering, and follow-up input queues
- session snapshots and cancellation plumbing

## Getting started

Downstream applications usually combine `lha-core` with a runtime from
`lha-llm`. For simple scripts, ask once and collect the final assistant text:

```rust
let manager = lha_core::AgentBuilder::new(runtime).build();
let answer = manager.ask_once("hello").await?;
println!("{answer}");
```

For live UI rendering, tool progress, reasoning display, MCP details, or
long-lived conversations, use the lower-level session event stream:

```rust
let manager = lha_core::AgentBuilder::new(runtime).build();
let session = manager.create_session();
session
    .run(lha_core::SessionInput::from_user_text("hello"))
    .await?;
let event = session.next_event().await?;
```

For the 5-minute quickstart and a complete event-stream walkthrough, see
[Building Agents with lha-llm and lha-core](../../docs/sdk-building-agents.md).

## Tools, skills, and MCP

The crate includes generic tool registration and execution APIs, lightweight
skill abstractions, and an optional `mcp` feature for MCP-to-tool adapter types.
Product-specific tools such as shell execution, memories, image generation, and
approval UX remain outside this crate.

With the `mcp` feature enabled, SDK users can adapt an MCP client into normal
agent tools:

```rust
let provider = lha_core::mcp::McpToolProvider::load("server", client).await?;
let manager = lha_core::AgentBuilder::new(runtime)
    .try_register_mcp_provider(provider)?
    .build();
```

MCP tool names are qualified as `mcp__server__tool` for model-visible function
tools, while calls are forwarded to the MCP client with the original tool name.

## Product boundary

`lha-core` intentionally does not include the TUI, CLI parsing, persistence,
sandbox approval UX, telemetry backend, or LHA product protocol events. Those
belong to `lha`.
