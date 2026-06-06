# Agent Core SDK Layer

`lha-core` is the reusable, product-neutral agent SDK that sits between
`lha-llm` and product runtimes such as the LHA runtime modules inside
the `lha` package.

`lha-agent-core` and `lha-agent-runtime` are now transitional workspace
compatibility shims that re-export the consolidated `lha-core` SDK. See
[Three-Crate Publish Boundary Refactor](./three-crate-publish-boundaries.md)
for the publishing plan.

## Layering

The intended stack is:

`lha-llm` -> `lha-core` -> product runtime

- `lha-llm` is the model/runtime SDK boundary.
- `lha-core::kernel` is the low-level turn-stream kernel.
- `lha-core` is the stateful session SDK.
- `lha` contains the LHA product-specific runtime modules built on top of
  that layer.

## What `lha-core` owns

- Session lifecycle and status
- In-memory transcript state
- Turn execution built on `lha_core::kernel`
- Generic event streaming
- Generic tool registration and execution
- Lightweight skill provider abstractions
- Optional MCP-to-tool SDK skeleton behind the `mcp` feature
- Steering and follow-up input queues
- Session snapshots for adapters and callers

## What stays out of `lha-core`

- Rollout persistence and thread indexing
- SQLite-backed session state
- LHA protocol `Op` / `EventMsg`
- Approval, sandbox, exec-policy, and coding tool UX
- Product skill installation, project-doc injection, review flows, and CLI-backed delegated jobs

`lha-core` also should not depend on `lha-llm` compatibility bridges
that reconstruct provider-facing transcript items. Session/runtime code should
append semantic transcript items directly from tool calls and tool results.
It should also treat tool names such as `local_shell` as ordinary semantic tool
identifiers; product-specific interpretation and defaulting stays in higher
layers.

Those concerns remain in the `lha` product runtime or other higher-level
product crates.

## Current migration shape

The current migration path is:

1. Build new generic agents directly on `lha-core`.
2. Keep the `lha` product runtime modules as the compatibility/product
   layer.
3. Keep `lha-agent-core` and `lha-agent-runtime` only as temporary re-export shims while downstream imports migrate.

This lets the workspace expose a small reusable agent SDK without forcing an all-at-once product rewrite.

Recent cleanup in `lha-llm` also keeps its public API focused on semantic
runtime types. Tool-call/transcript reconstruction helpers are expected to live
with the semantic types or with higher-level adapters rather than as public
runtime bridge APIs.

## Minimal SDK shape

A downstream crate can build a minimal in-memory agent using `lha-llm` and `lha-core` without importing `lha_protocol::*` directly:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use lha_core::AgentBuilder;
use lha_core::SessionInput;
use lha_llm::RuntimeCapabilities;
use lha_llm::RuntimeMetadata;
use lha_llm::SemanticConversationCompactor;
use lha_llm::SemanticRuntime;
use lha_llm::SemanticRuntimeSession;
use lha_llm::TurnEvent;
use lha_llm::TurnEventStream;
use lha_llm::TurnRequest;
use tokio::sync::mpsc;

struct FakeRuntime;

#[async_trait]
impl SemanticConversationCompactor for FakeRuntime {
    async fn compact_conversation_history(
        &self,
        input: &TurnRequest,
    ) -> lha_llm::Result<Vec<lha_llm::TranscriptItem>> {
        Ok(input.conversation.clone())
    }
}

#[async_trait]
impl SemanticRuntime for FakeRuntime {
    fn new_session(&self) -> Box<dyn SemanticRuntimeSession> {
        Box::new(FakeRuntimeSession)
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            supports_parallel_tool_calls: true,
            enforce_declared_tool_names: false,
            supports_dynamic_context_window_probe: false,
            supports_reasoning_summaries: true,
            supports_output_schema: true,
            supports_remote_compaction: false,
        }
    }

    fn metadata(&self) -> RuntimeMetadata {
        RuntimeMetadata {
            endpoint_name: "fake".to_string(),
            model: "test-model".to_string(),
        }
    }

    fn estimated_input_tokens(&self, _input: &TurnRequest) -> Option<i64> {
        None
    }
}

struct FakeRuntimeSession;

#[async_trait]
impl SemanticRuntimeSession for FakeRuntimeSession {
    async fn run_turn(&mut self, _input: &TurnRequest) -> lha_llm::Result<TurnEventStream> {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(TurnEvent::Completed {
                    response_id: "resp-1".to_string(),
                    token_usage: None,
                }))
                .await;
        });
        Ok(TurnEventStream::from_receiver(rx))
    }
}

let manager = AgentBuilder::new(Arc::new(FakeRuntime)).build();
let session = manager.create_session();
session.run(SessionInput::from_user_text("hi")).await?;
```
