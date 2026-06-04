#![allow(dead_code, unused_imports)]

#[path = "../product/agent_runtime/tests/all.rs"]
mod agent_runtime_all;
#[path = "../product/agent_runtime/tests/architecture_sdk_boundary.rs"]
mod agent_runtime_architecture_sdk_boundary;
#[path = "../product/agent_runtime/tests/chat_completions_payload.rs"]
mod agent_runtime_chat_completions_payload;
#[path = "../product/agent_runtime/tests/chat_completions_sse.rs"]
mod agent_runtime_chat_completions_sse;
#[path = "../product/agent_runtime/tests/responses_headers.rs"]
mod agent_runtime_responses_headers;

#[path = "../product/app_server/tests/all.rs"]
mod app_server_all;

#[path = "../product/app_server_protocol/tests/schema_fixtures.rs"]
mod app_server_protocol_schema_fixtures;

#[path = "../product/apply_patch/tests/all.rs"]
mod apply_patch_all;

#[path = "../product/exec_cli/tests/all.rs"]
mod exec_cli_all;

#[path = "../product/execpolicy/tests/basic.rs"]
mod execpolicy_basic;

#[path = "../product/linux_sandbox/tests/all.rs"]
mod linux_sandbox_all;

#[path = "../product/mcp_server/tests/all.rs"]
mod mcp_server_all;

#[path = "../product/mcp_types/tests/all.rs"]
mod mcp_types_all;

#[path = "../product/otel/tests/tests.rs"]
mod otel_tests;

#[path = "../product/rmcp_client/tests/resources.rs"]
mod rmcp_client_resources;

#[path = "../product/stdio_to_uds/tests/stdio_to_uds.rs"]
mod stdio_to_uds_tests;

#[path = "../product/tui_app/tests/all.rs"]
mod tui_app_all;
