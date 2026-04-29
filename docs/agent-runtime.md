# Agent Runtime Layer

`adam-agent-runtime` is the reusable, product-neutral agent SDK that sits between `adam-llm` and product runtimes such as `adam-agent`.

## Layering

The intended stack is:

`adam-llm` -> `adam-agent-core` -> `adam-agent-runtime` -> product runtime

- `adam-llm` is the model/runtime SDK boundary.
- `adam-agent-core` is the low-level turn-stream kernel.
- `adam-agent-runtime` is the stateful session SDK.
- `adam-agent` is one product-specific runtime built on top of those layers.

## What `adam-agent-runtime` owns

- Session lifecycle and status
- In-memory transcript state
- Turn execution built on `adam-agent-core`
- Generic event streaming
- Generic tool registration and execution
- Steering and follow-up input queues
- Session snapshots for adapters and callers

## What stays out of `adam-agent-runtime`

- Rollout persistence and thread indexing
- SQLite-backed session state
- Codex protocol `Op` / `EventMsg`
- Approval, sandbox, exec-policy, and coding tool UX
- Skills, project-doc injection, review flows, and subagents

`adam-agent-runtime` also should not depend on `adam-llm` compatibility bridges
that reconstruct provider-facing transcript items. Session/runtime code should
append semantic transcript items directly from tool calls and tool results.
It should also treat tool names such as `local_shell` as ordinary semantic tool
identifiers; product-specific interpretation and defaulting stays in higher
layers.

Those concerns remain in `adam-agent` or other higher-level crates.

## Current migration shape

The current migration path is:

1. Build new generic agents directly on `adam-agent-runtime`.
2. Keep `adam-agent` as the compatibility/product layer.
3. Gradually move generic runtime behavior out of `src/agent/runtime` and into `src/core/agent-runtime`.

This lets the workspace expose a small reusable agent SDK without forcing an all-at-once product rewrite.

Recent cleanup in `adam-llm` also keeps its public API focused on semantic
runtime types. Tool-call/transcript reconstruction helpers are expected to live
with the semantic types or with higher-level adapters rather than as public
runtime bridge APIs.

## Minimal SDK shape

A downstream crate can build a minimal in-memory agent using `adam-llm` and `adam-agent-runtime` without importing `adam_protocol::*` directly:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use adam_agent_runtime::AgentBuilder;
use adam_agent_runtime::SessionInput;
use adam_llm::RuntimeCapabilities;
use adam_llm::RuntimeMetadata;
use adam_llm::SemanticConversationCompactor;
use adam_llm::SemanticRuntime;
use adam_llm::SemanticRuntimeSession;
use adam_llm::TurnEvent;
use adam_llm::TurnEventStream;
use adam_llm::TurnRequest;
use tokio::sync::mpsc;

struct FakeRuntime;

#[async_trait]
impl SemanticConversationCompactor for FakeRuntime {
    async fn compact_conversation_history(
        &self,
        input: &TurnRequest,
    ) -> adam_llm::Result<Vec<adam_llm::TranscriptItem>> {
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
    async fn run_turn(&mut self, _input: &TurnRequest) -> adam_llm::Result<TurnEventStream> {
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
