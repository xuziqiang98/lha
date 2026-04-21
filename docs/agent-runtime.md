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

## Minimal SDK shape

A downstream crate can build a minimal in-memory agent using `codex-llm` and `codex-agent-runtime` without importing `codex_protocol::*` directly:

```rust
use std::sync::Arc;

use async_trait::async_trait;
use codex_agent_runtime::AgentBuilder;
use codex_agent_runtime::SessionInput;
use codex_llm::RuntimeCapabilities;
use codex_llm::RuntimeMetadata;
use codex_llm::SemanticConversationCompactor;
use codex_llm::SemanticRuntime;
use codex_llm::SemanticRuntimeSession;
use codex_llm::TurnEvent;
use codex_llm::TurnEventStream;
use codex_llm::TurnRequest;
use tokio::sync::mpsc;

struct FakeRuntime;

#[async_trait]
impl SemanticConversationCompactor for FakeRuntime {
    async fn compact_conversation_history(
        &self,
        input: &TurnRequest,
    ) -> codex_llm::Result<Vec<codex_llm::TranscriptItem>> {
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
    async fn run_turn(&mut self, _input: &TurnRequest) -> codex_llm::Result<TurnEventStream> {
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
