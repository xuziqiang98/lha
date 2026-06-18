# Building Agents with lha-llm and lha-core

This guide is for Rust applications that want to build an agent on the reusable
LHA SDK crates without depending on the full `lha` CLI/TUI product package.

Use the crates this way:

- `lha-llm` owns model-facing concerns: provider endpoints, API clients,
  semantic transcript items, tool descriptors, runtime sessions, and streamed
  turn events.
- `lha-core` owns the product-neutral agent loop: in-memory sessions, turn
  execution, input queues, tool dispatch, lightweight skills, and optional MCP
  adapters.
- `lha` is the complete product package. It layers CLI/TUI UX, persistence,
  sandbox approvals, app-server protocol, product tools, and local config on top
  of `lha-llm` and `lha-core`.

## What you build

A minimal downstream agent has four moving pieces:

- an `Arc<dyn lha_llm::SemanticRuntime>` that can run model turns;
- an `lha_core::AgentManager` built with that runtime;
- an `lha_core::AgentSession` created by the manager; and
- an async event loop that consumes `lha_core::AgentEvent` values.

`lha-core` drives the turn loop and dispatches registered tools. Your
application owns the surrounding UX: reading user input, rendering streamed
text, logging tool calls, persisting state, or deciding when to stop.

## Cargo.toml

For a small application that uses OpenAI-compatible Responses and one local
tool, start with:

```toml
[dependencies]
anyhow = "1"
async-trait = "0.1"
lha-core = "1"
lha-llm = "1"
reqwest = "0.12"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tokio-util = "0.7"
```

If you want to adapt MCP tools into normal agent tools, enable the `mcp`
feature on `lha-core`:

```toml
lha-core = { version = "1", features = ["mcp"] }
```

## Step 1: choose a runtime

The runtime is the model-facing part of the stack. You have two choices:

- Use a real provider runtime with `lha_llm::DefaultRuntimeClientFactory` and a
  `lha_llm::RuntimeBuildSpec`.
- Implement `lha_llm::SemanticRuntime` and `lha_llm::SemanticRuntimeSession`
  yourself if you have a custom model backend, test fixture, or offline model.

For OpenAI's Responses API, use `RuntimeEndpoint::openai()`. It reads the API
key from `OPENAI_API_KEY`; it also honors `OPENAI_BASE_URL`,
`OPENAI_ORGANIZATION`, and `OPENAI_PROJECT` when those environment variables
are set.

A real-provider runtime requires these `RuntimeBuildSpec` fields:

- `endpoint_id`: stable identifier for this provider, such as `"openai"`;
- `http_client`: the `reqwest::Client` used for HTTP and streaming requests;
- `model_info`: metadata describing the selected model;
- `telemetry`: an implementation of `RuntimeTelemetry`, or
  `noop_runtime_telemetry()`;
- `endpoint`: the provider endpoint, such as `RuntimeEndpoint::openai()`;
- `effort`: optional reasoning effort override;
- `summary`: reasoning summary preference;
- `session_id`: stable ID sent to the provider for conversation correlation;
- `origin_tag`: optional label for the caller;
- `show_raw_agent_reasoning`: whether raw reasoning events should be surfaced;
- `model_verbosity`: optional model verbosity override;
- `web_search_mode`: optional web-search mode;
- `experimental_beta_feature_keys`: provider beta feature keys; and
- `sse_fixture_path`: optional fixture path for tests.

## Step 2: provide model metadata

`lha-llm` currently expects callers to provide a complete `ModelInfo` for the
selected model. The SDK does not require you to use LHA's product model catalog.
The helper below is documentation-only code you can copy into your application
and adjust for the model you select. These examples enumerate the current public
fields; if your application does not provide billing metadata, set
`pricing: None`.

```rust
fn model_info(model: &str) -> lha_llm::ModelInfo {
    lha_llm::ModelInfo {
        slug: model.to_string(),
        display_name: model.to_string(),
        description: None,
        default_reasoning_level: Some(lha_llm::ReasoningEffort::Medium),
        supported_reasoning_levels: vec![
            lha_llm::ReasoningEffortPreset {
                effort: lha_llm::ReasoningEffort::Low,
                description: "low".to_string(),
            },
            lha_llm::ReasoningEffortPreset {
                effort: lha_llm::ReasoningEffort::Medium,
                description: "medium".to_string(),
            },
            lha_llm::ReasoningEffortPreset {
                effort: lha_llm::ReasoningEffort::High,
                description: "high".to_string(),
            },
        ],
        visibility: lha_llm::ModelVisibility::List,
        supported_in_api: true,
        priority: 0,
        upgrade: None,
        base_instructions: "You are a helpful assistant.".to_string(),
        model_messages: None,
        supports_reasoning_summaries: true,
        support_verbosity: false,
        default_verbosity: None,
        truncation_policy: lha_llm::TruncationPolicyConfig::bytes(10_000),
        supports_parallel_tool_calls: true,
        context_window: Some(272_000),
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        pricing: None,
    }
}
```

Keep this metadata aligned with the provider and model you actually use. The
most important fields to double-check are `supports_reasoning_summaries`,
`supports_parallel_tool_calls`, `truncation_policy`, and `context_window`.

## Step 3: register tools

Tools are ordinary `ToolHandler` implementations. The model sees the descriptor
from `spec()`. When the model calls that tool, `lha-core` dispatches the call to
`handle()` and appends the returned tool result to the transcript.

```rust
use async_trait::async_trait;
use lha_core::tools::{ToolError, ToolHandler, ToolInvocation, ToolOutput, ToolPayload};
use lha_llm::{AdditionalProperties, FunctionToolDescriptor, ToolDescriptor, ToolInputSchema};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

struct EchoTool;

#[async_trait]
impl ToolHandler for EchoTool {
    fn spec(&self) -> ToolDescriptor {
        let mut properties = BTreeMap::new();
        properties.insert(
            "message".to_string(),
            ToolInputSchema::String {
                description: Some("Text to echo back.".to_string()),
                enum_values: None,
            },
        );

        ToolDescriptor::Function(FunctionToolDescriptor {
            name: "echo".to_string(),
            description: "Echo a message back to the model.".to_string(),
            strict: true,
            parameters: ToolInputSchema::Object {
                properties,
                required: Some(vec!["message".to_string()]),
                additional_properties: Some(AdditionalProperties::Boolean(false)),
            },
        })
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _cancellation_token: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let arguments = match invocation.payload {
            ToolPayload::JsonArguments { arguments } => arguments,
            ToolPayload::TextInput { input } => input,
        };
        let value: serde_json::Value = serde_json::from_str(&arguments)
            .map_err(|err| ToolError::RespondToModel(format!("invalid JSON: {err}")))?;
        let message = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::RespondToModel("missing message".to_string()))?;

        Ok(ToolOutput::Function {
            content: message.to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}
```

Use `ToolError::RespondToModel` when the model should receive an error-like tool
result and continue. Use `ToolError::Fatal` when the turn should fail. Return
`true` from `supports_parallel_tool_calls()` only when the handler is safe to run
concurrently with other calls.

## Step 4: build the agent

Create the runtime, register tools, then create a session:

```rust
use lha_core::{AgentBuilder, SessionInput};
use lha_llm::{
    DefaultRuntimeClientFactory, ReasoningSummary, RuntimeBuildSpec, RuntimeClientFactory,
    RuntimeEndpoint, noop_runtime_telemetry,
};
use std::sync::Arc;

let endpoint = RuntimeEndpoint::openai().with_realtime_turn_streaming_enabled(false);
let runtime = DefaultRuntimeClientFactory::new().build_client(RuntimeBuildSpec {
    endpoint_id: "openai".to_string(),
    http_client: reqwest::Client::new(),
    model_info: model_info("gpt-4.1"),
    telemetry: noop_runtime_telemetry(),
    endpoint,
    effort: None,
    summary: ReasoningSummary::Auto,
    session_id: "example-session".to_string(),
    origin_tag: Some("sdk-example".to_string()),
    show_raw_agent_reasoning: false,
    model_verbosity: None,
    web_search_mode: None,
    experimental_beta_feature_keys: Vec::new(),
    sse_fixture_path: None,
});

let manager = AgentBuilder::new(runtime)
    .with_base_instructions("You are a concise assistant.")
    .register_tool(Arc::new(EchoTool))
    .build();

let session = manager.create_session();
session
    .run(SessionInput::from_user_text("Say hello, then call echo."))
    .await?;
```

The call to `run()` starts work in the background and returns a submission ID.
Read `next_event()` to observe progress.

## Step 5: consume events

A simple event loop usually handles streamed text, tool calls, tool results, and
terminal turn states:

```rust
use lha_core::{AgentEvent, TurnItemDelta};

loop {
    match session.next_event().await? {
        AgentEvent::OutputItemDelta {
            delta: TurnItemDelta::OutputText { delta },
            ..
        } => print!("{delta}"),
        AgentEvent::ToolCallRequested { call, .. } => {
            eprintln!("calling tool {} ({})", call.tool_name, call.call_id);
        }
        AgentEvent::ToolCallCompleted { response, .. } => {
            eprintln!("tool {} completed", response.tool_name);
        }
        AgentEvent::TurnCompleted { outcome, .. } => {
            eprintln!("\nturn completed: {:?}", outcome.last_agent_message);
            break;
        }
        AgentEvent::TurnFailed { error, .. } => {
            return Err(anyhow::anyhow!("turn failed: {error}"));
        }
        AgentEvent::TurnAborted { .. } => {
            return Err(anyhow::anyhow!("turn aborted"));
        }
        AgentEvent::SessionStarted { .. }
        | AgentEvent::SessionStatusChanged { .. }
        | AgentEvent::InputQueued { .. }
        | AgentEvent::TurnStarted { .. }
        | AgentEvent::RuntimeNotice { .. }
        | AgentEvent::OutputItemStarted { .. }
        | AgentEvent::OutputItemCompleted { .. }
        | AgentEvent::ServerReasoningIncluded { .. }
        | AgentEvent::ModelsEtagUpdated { .. } => {}
        AgentEvent::OutputItemDelta {
            delta:
                TurnItemDelta::ProposedPlan { .. }
                | TurnItemDelta::ReasoningSummary { .. }
                | TurnItemDelta::ReasoningContent { .. }
                | TurnItemDelta::ReasoningSummaryPartAdded { .. },
            ..
        } => {}
    }
}
```

`lha-core` automatically dispatches registered tools and appends their results
to the transcript. Your application can render the event stream however it
wants.

## Full minimal program

This single-file example uses only `lha-core`, `lha-llm`, and ordinary support
crates. It does not depend on the `lha` product crate or any private
`src/agent/cli/product` modules.

```rust
use anyhow::Result;
use async_trait::async_trait;
use lha_core::tools::{ToolError, ToolHandler, ToolInvocation, ToolOutput, ToolPayload};
use lha_core::{AgentBuilder, AgentEvent, SessionInput, TurnItemDelta};
use lha_llm::{
    AdditionalProperties, DefaultRuntimeClientFactory, FunctionToolDescriptor, ModelInfo,
    ModelVisibility, ReasoningEffort, ReasoningEffortPreset, ReasoningSummary, RuntimeBuildSpec,
    RuntimeClientFactory, RuntimeEndpoint, ToolDescriptor, ToolInputSchema,
    TruncationPolicyConfig, noop_runtime_telemetry,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn model_info(model: &str) -> ModelInfo {
    ModelInfo {
        slug: model.to_string(),
        display_name: model.to_string(),
        description: None,
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffort::Low,
                description: "low".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Medium,
                description: "medium".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::High,
                description: "high".to_string(),
            },
        ],
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority: 0,
        upgrade: None,
        base_instructions: "You are a helpful assistant.".to_string(),
        model_messages: None,
        supports_reasoning_summaries: true,
        support_verbosity: false,
        default_verbosity: None,
        truncation_policy: TruncationPolicyConfig::bytes(10_000),
        supports_parallel_tool_calls: true,
        context_window: Some(272_000),
        auto_compact_token_limit: None,
        effective_context_window_percent: 95,
        pricing: None,
    }
}

struct EchoTool;

#[async_trait]
impl ToolHandler for EchoTool {
    fn spec(&self) -> ToolDescriptor {
        let mut properties = BTreeMap::new();
        properties.insert(
            "message".to_string(),
            ToolInputSchema::String {
                description: Some("Text to echo back.".to_string()),
                enum_values: None,
            },
        );

        ToolDescriptor::Function(FunctionToolDescriptor {
            name: "echo".to_string(),
            description: "Echo a message back to the model.".to_string(),
            strict: true,
            parameters: ToolInputSchema::Object {
                properties,
                required: Some(vec!["message".to_string()]),
                additional_properties: Some(AdditionalProperties::Boolean(false)),
            },
        })
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _cancellation_token: CancellationToken,
    ) -> std::result::Result<ToolOutput, ToolError> {
        let arguments = match invocation.payload {
            ToolPayload::JsonArguments { arguments } => arguments,
            ToolPayload::TextInput { input } => input,
        };
        let value: serde_json::Value = serde_json::from_str(&arguments)
            .map_err(|err| ToolError::RespondToModel(format!("invalid JSON: {err}")))?;
        let message = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::RespondToModel("missing message".to_string()))?;

        Ok(ToolOutput::Function {
            content: message.to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let model = std::env::var("LHA_EXAMPLE_MODEL").unwrap_or_else(|_| "gpt-4.1".to_string());
    let endpoint = RuntimeEndpoint::openai().with_realtime_turn_streaming_enabled(false);
    let runtime = DefaultRuntimeClientFactory::new().build_client(RuntimeBuildSpec {
        endpoint_id: "openai".to_string(),
        http_client: reqwest::Client::new(),
        model_info: model_info(&model),
        telemetry: noop_runtime_telemetry(),
        endpoint,
        effort: None,
        summary: ReasoningSummary::Auto,
        session_id: "example-session".to_string(),
        origin_tag: Some("sdk-example".to_string()),
        show_raw_agent_reasoning: false,
        model_verbosity: None,
        web_search_mode: None,
        experimental_beta_feature_keys: Vec::new(),
        sse_fixture_path: None,
    });

    let manager = AgentBuilder::new(runtime)
        .with_base_instructions("You are a concise assistant.")
        .register_tool(Arc::new(EchoTool))
        .build();

    let session = manager.create_session();
    session
        .run(SessionInput::from_user_text(
            "Say hello, then call echo with the message 'done'.",
        ))
        .await?;

    loop {
        match session.next_event().await? {
            AgentEvent::OutputItemDelta {
                delta: TurnItemDelta::OutputText { delta },
                ..
            } => print!("{delta}"),
            AgentEvent::ToolCallRequested { call, .. } => {
                eprintln!("calling tool {} ({})", call.tool_name, call.call_id);
            }
            AgentEvent::ToolCallCompleted { response, .. } => {
                eprintln!("tool {} completed", response.tool_name);
            }
            AgentEvent::TurnCompleted { outcome, .. } => {
                eprintln!("\nturn completed: {:?}", outcome.last_agent_message);
                break;
            }
            AgentEvent::TurnFailed { error, .. } => {
                return Err(anyhow::anyhow!("turn failed: {error}"));
            }
            AgentEvent::TurnAborted { .. } => {
                return Err(anyhow::anyhow!("turn aborted"));
            }
            AgentEvent::SessionStarted { .. }
            | AgentEvent::SessionStatusChanged { .. }
            | AgentEvent::InputQueued { .. }
            | AgentEvent::TurnStarted { .. }
            | AgentEvent::RuntimeNotice { .. }
            | AgentEvent::OutputItemStarted { .. }
            | AgentEvent::OutputItemCompleted { .. }
            | AgentEvent::ServerReasoningIncluded { .. }
            | AgentEvent::ModelsEtagUpdated { .. } => {}
            AgentEvent::OutputItemDelta {
                delta:
                    TurnItemDelta::ProposedPlan { .. }
                    | TurnItemDelta::ReasoningSummary { .. }
                    | TurnItemDelta::ReasoningContent { .. }
                    | TurnItemDelta::ReasoningSummaryPartAdded { .. },
                ..
            } => {}
        }
    }

    Ok(())
}
```

Run it with an API key and, optionally, a model override:

```sh
export OPENAI_API_KEY="..."
export LHA_EXAMPLE_MODEL="gpt-4.1"
cargo run
```

## Where lha fits

The published `lha` package builds on these same two SDK crates and adds the
product experience:

- CLI and TUI entrypoints;
- sandbox policy and approval UX;
- persistence, rollout storage, and thread indexing;
- product skills and project context loading;
- app-server protocol; and
- LHA-specific coding tools and configuration.

If you only need an embedded agent loop, depend on `lha-core` and `lha-llm`. If
you want the complete Long-Horizon Agent product, install and run `lha`.

## Optional: MCP tools

With `lha-core`'s `mcp` feature enabled, an MCP client can be adapted into
normal model-visible function tools:

```rust
let provider = lha_core::mcp::McpToolProvider::load("server", client).await?;
let manager = lha_core::AgentBuilder::new(runtime)
    .try_register_mcp_provider(provider)?
    .build();
```

MCP tool names are exposed to the model as `mcp__server__tool`, while the
adapter forwards calls to the original MCP tool name.

## Troubleshooting

- `OPENAI_API_KEY` is missing: set it in the process environment or configure a
  custom `RuntimeEndpoint` with another `env_key` or bearer token.
- Model metadata is wrong: verify `supports_reasoning_summaries`,
  `supports_parallel_tool_calls`, `truncation_policy`, and `context_window` for
  the provider and model you selected.
- A tool is never called: check the tool name, description, JSON schema, and
  prompt instructions. The model only sees `ToolDescriptor` data.
- A turn keeps failing: log `AgentEvent::TurnFailed { error, .. }` and inspect
  provider errors or `ToolError::Fatal` values.
- You need LHA product behavior: use the `lha` CLI instead of only the SDK
  crates.
