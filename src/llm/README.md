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

## Telemetry hooks

The crate exposes product-neutral telemetry hooks. The `lha-cli` product layer
is responsible for adapting those hooks to its OpenTelemetry backend.
