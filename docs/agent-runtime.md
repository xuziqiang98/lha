# Agent Runtime Layer

`lha-agent-runtime` is the reusable, product-neutral agent SDK that sits between `lha-llm` and product runtimes such as `lha-agent`.

`lha-agent-runtime` is also a transitional workspace crate. The target
crates.io publish boundary is `lha-core`, where the current `lha-agent-core`
kernel and `lha-agent-runtime` session SDK will be combined. See
[Three-Crate Publish Boundary Refactor](./three-crate-publish-boundaries.md)
for the publishing plan.

## Layering

The intended stack is:

`lha-llm` -> `lha-agent-core` -> `lha-agent-runtime` -> product runtime

- `lha-llm` is the model/runtime SDK boundary.
- `lha-agent-core` is the low-level turn-stream kernel.
- `lha-agent-runtime` is the stateful session SDK.
- `lha-agent` is one product-specific runtime built on top of those layers.

## What `lha-agent-runtime` owns

- Session lifecycle and status
- In-memory transcript state
- Turn execution built on `lha-agent-core`
- Generic event streaming
- Generic tool registration and execution
- Steering and follow-up input queues
- Session snapshots for adapters and callers

## What stays out of `lha-agent-runtime`

- Rollout persistence and thread indexing
- SQLite-backed session state
- LHA protocol `Op` / `EventMsg`
- Approval, sandbox, exec-policy, and coding tool UX
- Skills, project-doc injection, review flows, and CLI-backed delegated jobs

`lha-agent-runtime` also should not depend on `lha-llm` compatibility bridges
that reconstruct provider-facing transcript items. Session/runtime code should
append semantic transcript items directly from tool calls and tool results.
It should also treat tool names such as `local_shell` as ordinary semantic tool
identifiers; product-specific interpretation and defaulting stays in higher
layers.

Those concerns remain in `lha-agent` or other higher-level crates.

## Current migration shape

The current migration path is:

1. Build new generic agents directly on `lha-agent-runtime`.
2. Keep `lha-agent` as the compatibility/product layer.
3. Gradually move generic runtime behavior out of `src/agent/runtime` and into `src/core/agent-runtime`.

This lets the workspace expose a small reusable agent SDK without forcing an all-at-once product rewrite.

Recent cleanup in `lha-llm` also keeps its public API focused on semantic
runtime types. Tool-call/transcript reconstruction helpers are expected to live
with the semantic types or with higher-level adapters rather than as public
runtime bridge APIs.

## Minimal SDK shape

A downstream crate can build a minimal in-memory agent using `lha-llm` and
`lha-agent-runtime` without importing `lha_protocol::*` directly. In the
three-crate publishing target, the `lha_agent_runtime` imports in this example
move to `lha_core`:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use lha_agent_runtime::AgentBuilder;
use lha_agent_runtime::SessionInput;
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
