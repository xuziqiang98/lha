# lha-core

Minimal agent SDK for LHA. This package provides the reusable in-memory agent
loop and session runtime that sit between model-facing code in `lha-llm` and
product-specific orchestration in `lha-cli`.

## Runtime and sessions

`lha-core` owns the product-neutral agent runtime:

- in-memory agent sessions and lifecycle management
- turn execution and event streams
- primary, steering, and follow-up input queues
- session snapshots and cancellation plumbing

## Tools, skills, and MCP

The crate includes generic tool registration and execution APIs, lightweight
skill abstractions, and an optional `mcp` feature for MCP-to-tool adapter types.
Product-specific tools such as shell execution, memories, image generation, and
approval UX remain outside this crate.

## Product boundary

`lha-core` intentionally does not include the TUI, CLI parsing, persistence,
sandbox approval UX, telemetry backend, or LHA product protocol events. Those
belong to `lha-cli`.
