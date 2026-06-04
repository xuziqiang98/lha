use async_trait::async_trait;
use thiserror::Error;

pub mod types;

pub const MCP_TOOL_NAME_PREFIX: &str = "mcp";
pub const MCP_TOOL_NAME_DELIMITER: &str = "__";

pub fn qualify_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "{MCP_TOOL_NAME_PREFIX}{MCP_TOOL_NAME_DELIMITER}{server_name}{MCP_TOOL_NAME_DELIMITER}{tool_name}"
    )
}

pub fn split_qualified_tool_name(qualified_name: &str) -> Option<(String, String)> {
    let mut parts = qualified_name.split(MCP_TOOL_NAME_DELIMITER);
    let prefix = parts.next()?;
    if prefix != MCP_TOOL_NAME_PREFIX {
        return None;
    }
    let server_name = parts.next()?;
    let tool_name = parts.collect::<Vec<_>>().join(MCP_TOOL_NAME_DELIMITER);
    if tool_name.is_empty() {
        return None;
    }
    Some((server_name.to_string(), tool_name))
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("{0}")]
    Fatal(String),
}

#[async_trait]
pub trait McpClient: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<types::McpTool>, McpError>;

    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<types::McpCallToolResult, McpError>;
}

pub struct McpToolProvider<C> {
    pub server_name: String,
    pub client: C,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::types::McpCallToolResult;
    use crate::mcp::types::McpContentBlock;
    use crate::mcp::types::McpTool;
    use crate::mcp::types::McpToolInputSchema;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn qualifies_tool_name() {
        assert_eq!(
            qualify_tool_name("alpha", "do_thing"),
            "mcp__alpha__do_thing"
        );
    }

    #[test]
    fn splits_qualified_tool_name() {
        assert_eq!(
            split_qualified_tool_name("mcp__alpha__do_thing"),
            Some(("alpha".to_string(), "do_thing".to_string()))
        );
    }

    #[test]
    fn preserves_nested_tool_names() {
        assert_eq!(
            split_qualified_tool_name("mcp__alpha__nested__op"),
            Some(("alpha".to_string(), "nested__op".to_string()))
        );
    }

    #[test]
    fn rejects_invalid_qualified_tool_names() {
        assert_eq!(split_qualified_tool_name("other__alpha__do_thing"), None);
        assert_eq!(split_qualified_tool_name("mcp__alpha__"), None);
    }

    #[test]
    fn mcp_types_roundtrip_json() {
        let tool = McpTool {
            name: "do_thing".to_string(),
            title: Some("Do Thing".to_string()),
            description: Some("does a thing".to_string()),
            input_schema: McpToolInputSchema {
                r#type: "object".to_string(),
                properties: Some(json!({"value": {"type": "string"}})),
                required: Some(vec!["value".to_string()]),
            },
        };
        let tool_json = serde_json::to_value(&tool).expect("serialize tool");
        assert_eq!(
            serde_json::from_value::<McpTool>(tool_json).expect("deserialize tool"),
            tool
        );

        let result = McpCallToolResult {
            content: vec![
                McpContentBlock::Text {
                    text: "ok".to_string(),
                },
                McpContentBlock::Json(json!({"kind": "details"})),
            ],
            is_error: Some(false),
            structured_content: Some(json!({"ok": true})),
        };
        let result_json = serde_json::to_value(&result).expect("serialize result");
        assert_eq!(
            serde_json::from_value::<McpCallToolResult>(result_json).expect("deserialize result"),
            result
        );
    }
}
