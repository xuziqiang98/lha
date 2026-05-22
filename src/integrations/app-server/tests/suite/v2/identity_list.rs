//! Validates that the identity list endpoint returns the expected default presets.
//!
//! The test drives the app server through the MCP harness and asserts that the list response
//! includes the default identities with their default reasoning effort settings, which keeps the
//! API contract visible in one place.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use adam_agent::models_manager::test_builtin_identity_presets;
use adam_app_server_protocol::IdentityListParams;
use adam_app_server_protocol::IdentityListResponse;
use adam_app_server_protocol::JSONRPCResponse;
use adam_app_server_protocol::RequestId;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::config_types::IdentityMask;
use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Confirms the server returns the default identity presets in a stable order.
#[tokio::test]
async fn list_identities_returns_presets() -> Result<()> {
    let adam_home = TempDir::new()?;
    let mut mcp = McpProcess::new(adam_home.path()).await?;

    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_list_identities_request(IdentityListParams {})
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let IdentityListResponse { data: items } = to_response::<IdentityListResponse>(response)?;

    let expected = [
        preset(IdentityKind::Nobody),
        preset(IdentityKind::Planner),
        preset(IdentityKind::Programmer),
        preset(IdentityKind::Explorer),
        preset(IdentityKind::Reviewer),
    ];
    assert_eq!(expected.len(), items.len());
    for (expected_mask, actual_mask) in expected.iter().zip(items.iter()) {
        assert_eq!(expected_mask.name, actual_mask.name);
        assert_eq!(expected_mask.kind, actual_mask.kind);
        assert_eq!(expected_mask.model, actual_mask.model);
        assert_eq!(expected_mask.reasoning_effort, actual_mask.reasoning_effort);
        let expected_instructions = expected_mask.developer_instructions.clone().flatten();
        let actual_instructions = actual_mask.developer_instructions.clone().flatten();
        assert_eq!(expected_instructions, actual_instructions);
    }
    Ok(())
}

fn preset(kind: IdentityKind) -> IdentityMask {
    let presets = test_builtin_identity_presets();
    presets
        .into_iter()
        .find(|preset| preset.kind == Some(kind))
        .unwrap()
}
