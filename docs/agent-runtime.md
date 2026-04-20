# Agent Runtime Layer

`codex-agent-runtime` is the reusable, product-neutral agent SDK that sits between `codex-llm` and product runtimes such as `codex-coding-agent`.

## Layering

The intended stack is:

`codex-llm` -> `codex-agent-core` -> `codex-agent-runtime` -> product runtime

- `codex-llm` is the model/runtime SDK boundary.
- `codex-agent-core` is the low-level turn-stream kernel.
- `codex-agent-runtime` is the stateful session SDK.
- `codex-coding-agent` is one product-specific runtime built on top of those layers.

## What `codex-agent-runtime` owns

- Session lifecycle and status
- In-memory transcript state
- Turn execution built on `codex-agent-core`
- Generic event streaming
- Generic tool registration and execution
- Steering and follow-up input queues
- Session snapshots for adapters and callers

## What stays out of `codex-agent-runtime`

- Rollout persistence and thread indexing
- SQLite-backed session state
- Codex protocol `Op` / `EventMsg`
- Approval, sandbox, exec-policy, and coding tool UX
- Skills, project-doc injection, review flows, and subagents

Those concerns remain in `codex-coding-agent` or other higher-level crates.

## Current migration shape

The current migration path is:

1. Build new generic agents directly on `codex-agent-runtime`.
2. Keep `codex-coding-agent` as the compatibility/product layer.
3. Gradually move generic runtime behavior out of `src/coding-agent/runtime` and into `src/core/agent-runtime`.

This lets the workspace expose a small reusable agent SDK without forcing an all-at-once product rewrite.
