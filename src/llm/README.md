# lha-llm

Reusable model-facing SDK for LHA. This package contains the semantic runtime,
wire API clients, generic HTTP transport, and model/transcript types.

`lha-llm` is product-neutral: it does not include the LHA TUI, local state,
sandbox UX, or persistence layers. Product packages adapt the hooks here to
their own telemetry, configuration, and storage systems.

## API module

`lha_llm::api` contains typed clients for LHA/OpenAI APIs built on top of the
generic transport in `lha_llm::client`.

- Hosts the request/response models and prompt helpers for Responses, Chat
  Completions, Messages, Images, and Compact APIs.
- Owns provider configuration, auth header injection, retry tuning, and stream
  idle settings.
- Parses SSE streams into `ResponseEvent`/`ResponseStream`, including
  rate-limit snapshots and API-specific error mapping.
- Defaults generation requests to streaming. `ResponsesClient`, `ChatClient`,
  and `MessagesClient` also provide `complete_request` and `complete_prompt`
  for true non-streaming HTTP requests that return `CompletedResponse`.

## Client module

`lha_llm::client` is a generic transport layer that wraps HTTP requests,
retries, and streaming primitives without endpoint-specific awareness.

- Defines `HttpTransport` and `ReqwestTransport` plus thin `Request`/`Response`
  types.
- Provides retry utilities (`RetryPolicy`, `RetryOn`, `run_with_retry`,
  `backoff`) for unary and streaming calls.
- Supplies helpers for SSE byte streams and multipart requests.

## Types and runtime

`lha_llm::types` defines semantic model-facing data such as transcript items,
tool descriptors, turn requests, and turn events. The runtime layer exposes
semantic sessions that can stream model output without depending on LHA product
state or UI code.

## Response delivery

`SemanticRuntimeSession::run_turn` remains streaming by default. Use
`run_turn_with_delivery(..., ResponseDelivery::NonStreaming)` when one turn
must use a complete HTTP response instead:

```rust
use lha_llm::{ResponseDelivery, SemanticRuntime};

let mut session = runtime.new_session();
let events = session
    .run_turn_with_delivery(&request, ResponseDelivery::NonStreaming)
    .await?;
```

The semantic runtime still returns `TurnEventStream` in either mode so it
remains compatible with `lha-core`. In non-streaming mode, events are emitted
only after the provider returns its complete JSON response; it emits completed
items and the terminal event, not token deltas.

## Use with lha-core

Use `DefaultRuntimeClientFactory` with `RuntimeBuildSpec` to construct a real
provider-backed `SemanticRuntime`, then pass that runtime into
`lha_core::AgentBuilder`. The quickstart path uses
`RuntimeBuildSpec::builder_from_lha_env` with `LHA_BASE_URL`, `LHA_API_KEY`,
`LHA_MODEL`, and optional `LHA_ENDPOINT`; lower-level builders remain available
when you need explicit model metadata, endpoint configuration, telemetry, or
streaming fixtures. If you already have a custom model backend, implement
`SemanticRuntime` and `SemanticRuntimeSession` directly instead.

For the 5-minute quickstart and a complete downstream-agent walkthrough, see
[Building Agents with lha-llm and lha-core](../../docs/sdk-building-agents.md).

## Telemetry hooks

The crate exposes product-neutral telemetry hooks. The `lha` product layer
is responsible for adapting those hooks to its OpenTelemetry backend.
