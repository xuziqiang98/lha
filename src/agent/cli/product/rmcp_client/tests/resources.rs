use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::product::mcp_types::ClientCapabilities;
use crate::product::mcp_types::Implementation;
use crate::product::mcp_types::InitializeRequestParams;
use crate::product::mcp_types::ListResourceTemplatesResult;
use crate::product::mcp_types::ReadResourceRequestParams;
use crate::product::mcp_types::ReadResourceResultContents;
use crate::product::mcp_types::Resource;
use crate::product::mcp_types::ResourceTemplate;
use crate::product::mcp_types::TextResourceContents;
use crate::product::rmcp_client::ElicitationAction;
use crate::product::rmcp_client::ElicitationResponse;
use crate::product::rmcp_client::RmcpClient;
use crate::test_support::cargo_bin::CargoBinError;
use futures::FutureExt as _;
use serde_json::json;

const RESOURCE_URI: &str = "memo://codex/example-note";

fn stdio_server_bin() -> Result<PathBuf, CargoBinError> {
    crate::test_support::cargo_bin::cargo_bin("test_stdio_server")
}

fn init_params() -> InitializeRequestParams {
    InitializeRequestParams {
        capabilities: ClientCapabilities {
            experimental: None,
            roots: None,
            sampling: None,
            elicitation: Some(json!({})),
        },
        client_info: Implementation {
            name: "codex-test".into(),
            version: "0.0.0-test".into(),
            title: Some("LHA rmcp resource test".into()),
            user_agent: None,
        },
        protocol_version: crate::product::mcp_types::MCP_SCHEMA_VERSION.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn rmcp_client_can_list_and_read_resources() -> anyhow::Result<()> {
    let client = RmcpClient::new_stdio_client(
        stdio_server_bin()?.into(),
        Vec::<OsString>::new(),
        None,
        &[],
        None,
    )
    .await?;

    client
        .initialize(
            init_params(),
            Some(Duration::from_secs(5)),
            Box::new(|_, _| {
                async {
                    Ok(ElicitationResponse {
                        action: ElicitationAction::Accept,
                        content: Some(json!({})),
                    })
                }
                .boxed()
            }),
        )
        .await?;

    let list = client
        .list_resources(None, Some(Duration::from_secs(5)))
        .await?;
    let memo = list
        .resources
        .iter()
        .find(|resource| resource.uri == RESOURCE_URI)
        .expect("memo resource present");
    assert_eq!(
        memo,
        &Resource {
            annotations: None,
            description: Some("A sample MCP resource exposed for integration tests.".to_string()),
            mime_type: Some("text/plain".to_string()),
            name: "example-note".to_string(),
            size: None,
            title: Some("Example Note".to_string()),
            uri: RESOURCE_URI.to_string(),
        }
    );
    let templates = client
        .list_resource_templates(None, Some(Duration::from_secs(5)))
        .await?;
    assert_eq!(
        templates,
        ListResourceTemplatesResult {
            next_cursor: None,
            resource_templates: vec![ResourceTemplate {
                annotations: None,
                description: Some(
                    "Template for memo://codex/{slug} resources used in tests.".to_string()
                ),
                mime_type: Some("text/plain".to_string()),
                name: "codex-memo".to_string(),
                title: Some("LHA Memo".to_string()),
                uri_template: "memo://codex/{slug}".to_string(),
            }],
        }
    );

    let read = client
        .read_resource(
            ReadResourceRequestParams {
                uri: RESOURCE_URI.to_string(),
            },
            Some(Duration::from_secs(5)),
        )
        .await?;
    let ReadResourceResultContents::TextResourceContents(text) =
        read.contents.first().expect("resource contents present")
    else {
        panic!("expected text resource");
    };
    assert_eq!(
        text,
        &TextResourceContents {
            text: "This is a sample MCP resource served by the rmcp test server.".to_string(),
            uri: RESOURCE_URI.to_string(),
            mime_type: Some("text/plain".to_string()),
        }
    );

    Ok(())
}
