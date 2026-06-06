use async_trait::async_trait;
use lha_llm::FunctionToolDescriptor;
use lha_llm::ToolDescriptor;
use lha_llm::ToolInputSchema;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::tools::ToolError;
use crate::tools::ToolHandler;
use crate::tools::ToolInvocation;
use crate::tools::ToolOutput;
use crate::tools::ToolPayload;

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
    #[error("failed to convert MCP tool `{tool_name}` schema: {error}")]
    Schema {
        tool_name: String,
        error: serde_json::Error,
    },
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
    pub tools: Vec<types::McpTool>,
}

impl<C> McpToolProvider<C>
where
    C: McpClient + Send + Sync + 'static,
{
    pub async fn load(server_name: impl Into<String>, client: C) -> Result<Self, McpError> {
        let tools = client.list_tools().await?;
        Ok(Self::from_tools(server_name, client, tools))
    }

    pub fn from_tools(
        server_name: impl Into<String>,
        client: C,
        tools: Vec<types::McpTool>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            client,
            tools,
        }
    }

    pub fn into_tool_handlers(
        self,
    ) -> Result<Vec<std::sync::Arc<dyn crate::tools::ToolHandler>>, McpError> {
        let client = Arc::new(self.client);
        self.tools
            .into_iter()
            .map(|tool| {
                let original_tool_name = tool.name.clone();
                let descriptor = mcp_tool_to_descriptor(self.server_name.as_str(), tool)?;
                Ok(Arc::new(McpToolHandler {
                    client: Arc::clone(&client),
                    original_tool_name,
                    descriptor,
                }) as Arc<dyn ToolHandler>)
            })
            .collect()
    }
}

struct McpToolHandler<C> {
    client: Arc<C>,
    original_tool_name: String,
    descriptor: ToolDescriptor,
}

#[async_trait]
impl<C> ToolHandler for McpToolHandler<C>
where
    C: McpClient + Send + Sync + 'static,
{
    fn spec(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _cancellation_token: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let arguments = match mcp_arguments(&invocation.payload) {
            Ok(arguments) => arguments,
            Err(content) => {
                return Ok(failed_tool_output(content));
            }
        };

        match self
            .client
            .call_tool(self.original_tool_name.as_str(), arguments)
            .await
        {
            Ok(result) => Ok(tool_output_from_call_tool_result(&result)),
            Err(err) => Ok(failed_tool_output(format!("tool call error: {err}"))),
        }
    }
}

fn mcp_tool_to_descriptor(
    server_name: &str,
    tool: types::McpTool,
) -> Result<ToolDescriptor, McpError> {
    let qualified_name = qualify_tool_name(server_name, tool.name.as_str());
    let parameters = mcp_input_schema_to_tool_input_schema(tool.input_schema).map_err(|error| {
        McpError::Schema {
            tool_name: qualified_name.clone(),
            error,
        }
    })?;
    Ok(ToolDescriptor::Function(FunctionToolDescriptor {
        name: qualified_name,
        description: tool.description.or(tool.title).unwrap_or_default(),
        strict: false,
        parameters,
    }))
}

fn mcp_input_schema_to_tool_input_schema(
    input_schema: types::McpToolInputSchema,
) -> Result<ToolInputSchema, serde_json::Error> {
    let mut value = serde_json::to_value(input_schema)?;
    if let Some(map) = value.as_object_mut()
        && !map.contains_key("properties")
    {
        map.insert(
            "properties".to_string(),
            Value::Object(serde_json::Map::new()),
        );
    }
    sanitize_json_schema(&mut value);
    serde_json::from_value(value)
}

fn sanitize_json_schema(value: &mut Value) {
    match value {
        Value::Bool(_) => {
            *value = json!({ "type": "string" });
        }
        Value::Array(items) => {
            for item in items {
                sanitize_json_schema(item);
            }
        }
        Value::Object(map) => {
            if let Some(properties) = map.get_mut("properties")
                && let Some(properties) = properties.as_object_mut()
            {
                for value in properties.values_mut() {
                    sanitize_json_schema(value);
                }
            }

            if let Some(items) = map.get_mut("items") {
                sanitize_json_schema(items);
            }

            for key in ["oneOf", "anyOf", "allOf", "prefixItems"] {
                if let Some(value) = map.get_mut(key) {
                    sanitize_json_schema(value);
                }
            }

            let mut ty = map.get("type").and_then(Value::as_str).map(str::to_string);

            if ty.is_none()
                && let Some(Value::Array(types)) = map.get("type")
            {
                ty = types.iter().find_map(|value| {
                    let value = value.as_str()?;
                    matches!(
                        value,
                        "object" | "array" | "string" | "number" | "integer" | "boolean"
                    )
                    .then(|| value.to_string())
                });
            }

            if ty.is_none() {
                ty = infer_json_schema_type(map);
            }

            let ty = normalize_json_schema_type(ty.as_deref());
            map.insert("type".to_string(), Value::String(ty.to_string()));

            if ty == "object" {
                map.entry("properties".to_string())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                if let Some(additional_properties) = map.get_mut("additionalProperties")
                    && !additional_properties.is_boolean()
                {
                    sanitize_json_schema(additional_properties);
                }
            }

            if ty == "array" {
                map.entry("items".to_string())
                    .or_insert_with(|| json!({ "type": "string" }));
            }
        }
        _ => {}
    }
}

fn infer_json_schema_type(map: &serde_json::Map<String, Value>) -> Option<String> {
    if map.contains_key("properties")
        || map.contains_key("required")
        || map.contains_key("additionalProperties")
    {
        Some("object".to_string())
    } else if map.contains_key("items") || map.contains_key("prefixItems") {
        Some("array".to_string())
    } else if map.contains_key("enum") || map.contains_key("const") || map.contains_key("format") {
        Some("string".to_string())
    } else if map.contains_key("minimum")
        || map.contains_key("maximum")
        || map.contains_key("exclusiveMinimum")
        || map.contains_key("exclusiveMaximum")
        || map.contains_key("multipleOf")
    {
        Some("number".to_string())
    } else {
        None
    }
}

fn normalize_json_schema_type(ty: Option<&str>) -> &'static str {
    match ty {
        Some("object") => "object",
        Some("array") => "array",
        Some("string") => "string",
        Some("number") | Some("integer") => "number",
        Some("boolean") => "boolean",
        Some(_) | None => "string",
    }
}

fn mcp_arguments(payload: &ToolPayload) -> Result<Option<Value>, String> {
    match payload {
        ToolPayload::JsonArguments { arguments } => {
            let value = if arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(arguments)
                    .map_err(|err| format!("failed to parse MCP tool arguments: {err}"))?
            };
            if value.is_null() {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
        ToolPayload::TextInput { .. } => {
            Err("MCP function tools do not support text input".to_string())
        }
    }
}

fn tool_output_from_call_tool_result(result: &types::McpCallToolResult) -> ToolOutput {
    let success = Some(result.is_error != Some(true));
    let content = if let Some(structured_content) = &result.structured_content {
        if !structured_content.is_null() {
            serde_json::to_string(structured_content)
        } else {
            serde_json::to_string(&result.content)
        }
    } else {
        serde_json::to_string(&result.content)
    };

    match content {
        Ok(content) => ToolOutput::Function {
            content,
            content_items: None,
            success,
        },
        Err(err) => failed_tool_output(err.to_string()),
    }
}

fn failed_tool_output(content: String) -> ToolOutput {
    ToolOutput::Function {
        content,
        content_items: None,
        success: Some(false),
    }
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
    use std::collections::BTreeMap;
    use std::sync::Mutex as StdMutex;

    type RecordedMcpCalls = Arc<StdMutex<Vec<(String, Option<Value>)>>>;

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

    #[derive(Clone)]
    struct TestMcpClient {
        tools: Vec<McpTool>,
        result: McpCallToolResult,
        calls: RecordedMcpCalls,
        error: Option<String>,
    }

    #[async_trait]
    impl McpClient for TestMcpClient {
        async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
            Ok(self.tools.clone())
        }

        async fn call_tool(
            &self,
            tool_name: &str,
            arguments: Option<Value>,
        ) -> Result<McpCallToolResult, McpError> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((tool_name.to_string(), arguments));
            if let Some(error) = &self.error {
                Err(McpError::Fatal(error.clone()))
            } else {
                Ok(self.result.clone())
            }
        }
    }

    fn test_mcp_tool() -> McpTool {
        McpTool {
            name: "do_thing".to_string(),
            title: Some("Do Thing".to_string()),
            description: None,
            input_schema: McpToolInputSchema {
                r#type: "object".to_string(),
                properties: Some(json!({
                    "count": {
                        "type": "integer",
                    },
                    "mode": {
                        "enum": ["fast", "safe"],
                    },
                })),
                required: Some(vec!["count".to_string()]),
            },
        }
    }

    fn test_client(result: McpCallToolResult) -> (TestMcpClient, RecordedMcpCalls) {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        (
            TestMcpClient {
                tools: vec![test_mcp_tool()],
                result,
                calls: Arc::clone(&calls),
                error: None,
            },
            calls,
        )
    }

    #[tokio::test]
    async fn mcp_provider_loads_tools_and_converts_to_function_handlers()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = McpCallToolResult {
            content: vec![McpContentBlock::Text {
                text: "ok".to_string(),
            }],
            is_error: None,
            structured_content: None,
        };
        let client = TestMcpClient {
            tools: vec![test_mcp_tool()],
            result,
            calls: Arc::new(StdMutex::new(Vec::new())),
            error: None,
        };

        let provider = McpToolProvider::load("alpha", client).await?;
        let handlers = provider.into_tool_handlers()?;
        assert_eq!(handlers.len(), 1);
        assert_eq!(
            handlers[0].spec(),
            ToolDescriptor::Function(FunctionToolDescriptor {
                name: "mcp__alpha__do_thing".to_string(),
                description: "Do Thing".to_string(),
                strict: false,
                parameters: ToolInputSchema::Object {
                    properties: BTreeMap::from([
                        (
                            "count".to_string(),
                            ToolInputSchema::Number { description: None },
                        ),
                        (
                            "mode".to_string(),
                            ToolInputSchema::String {
                                description: None,
                                enum_values: Some(vec!["fast".to_string(), "safe".to_string()]),
                            },
                        ),
                    ]),
                    required: Some(vec!["count".to_string()]),
                    additional_properties: None,
                },
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_handler_calls_original_tool_name_and_serializes_content()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = McpCallToolResult {
            content: vec![McpContentBlock::Text {
                text: "ok".to_string(),
            }],
            is_error: None,
            structured_content: None,
        };
        let (client, calls) = test_client(result);
        let handler = McpToolProvider::from_tools("alpha", client, vec![test_mcp_tool()])
            .into_tool_handlers()?
            .remove(0);

        let output = handler
            .handle(
                ToolInvocation {
                    call_id: "call-1".to_string(),
                    tool_name: "mcp__alpha__do_thing".to_string(),
                    payload: ToolPayload::JsonArguments {
                        arguments: r#"{"count":2}"#.to_string(),
                    },
                },
                CancellationToken::new(),
            )
            .await?;

        assert_eq!(
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            vec![("do_thing".to_string(), Some(json!({"count": 2})))]
        );
        assert_eq!(
            output,
            ToolOutput::Function {
                content: serde_json::to_string(&vec![McpContentBlock::Text {
                    text: "ok".to_string(),
                }])?,
                content_items: None,
                success: Some(true),
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_handler_prefers_structured_content() -> Result<(), Box<dyn std::error::Error>> {
        let result = McpCallToolResult {
            content: vec![McpContentBlock::Text {
                text: "fallback".to_string(),
            }],
            is_error: None,
            structured_content: Some(json!({"answer": 42})),
        };
        let (client, _calls) = test_client(result);
        let handler = McpToolProvider::from_tools("alpha", client, vec![test_mcp_tool()])
            .into_tool_handlers()?
            .remove(0);

        let output = handler
            .handle(
                ToolInvocation {
                    call_id: "call-1".to_string(),
                    tool_name: "mcp__alpha__do_thing".to_string(),
                    payload: ToolPayload::JsonArguments {
                        arguments: "{}".to_string(),
                    },
                },
                CancellationToken::new(),
            )
            .await?;

        assert_eq!(
            output,
            ToolOutput::Function {
                content: r#"{"answer":42}"#.to_string(),
                content_items: None,
                success: Some(true),
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_handler_marks_is_error_results_unsuccessful()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = McpCallToolResult {
            content: vec![McpContentBlock::Text {
                text: "bad".to_string(),
            }],
            is_error: Some(true),
            structured_content: None,
        };
        let (client, calls) = test_client(result);
        let handler = McpToolProvider::from_tools("alpha", client, vec![test_mcp_tool()])
            .into_tool_handlers()?
            .remove(0);

        let output = handler
            .handle(
                ToolInvocation {
                    call_id: "call-1".to_string(),
                    tool_name: "mcp__alpha__do_thing".to_string(),
                    payload: ToolPayload::JsonArguments {
                        arguments: "null".to_string(),
                    },
                },
                CancellationToken::new(),
            )
            .await?;

        assert_eq!(
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            vec![("do_thing".to_string(), None)]
        );
        assert_eq!(
            output,
            ToolOutput::Function {
                content: serde_json::to_string(&vec![McpContentBlock::Text {
                    text: "bad".to_string(),
                }])?,
                content_items: None,
                success: Some(false),
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_handler_returns_failed_output_for_invalid_json_arguments()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = McpCallToolResult {
            content: Vec::new(),
            is_error: None,
            structured_content: None,
        };
        let (client, calls) = test_client(result);
        let handler = McpToolProvider::from_tools("alpha", client, vec![test_mcp_tool()])
            .into_tool_handlers()?
            .remove(0);

        let output = handler
            .handle(
                ToolInvocation {
                    call_id: "call-1".to_string(),
                    tool_name: "mcp__alpha__do_thing".to_string(),
                    payload: ToolPayload::JsonArguments {
                        arguments: "{bad json".to_string(),
                    },
                },
                CancellationToken::new(),
            )
            .await?;

        assert!(
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        match output {
            ToolOutput::Function {
                content,
                content_items,
                success,
            } => {
                assert!(content.contains("failed to parse MCP tool arguments"));
                assert_eq!(content_items, None);
                assert_eq!(success, Some(false));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn mcp_handler_treats_empty_arguments_as_empty_object()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = McpCallToolResult {
            content: Vec::new(),
            is_error: None,
            structured_content: None,
        };
        let (client, calls) = test_client(result);
        let handler = McpToolProvider::from_tools("alpha", client, vec![test_mcp_tool()])
            .into_tool_handlers()?
            .remove(0);

        let _output = handler
            .handle(
                ToolInvocation {
                    call_id: "call-1".to_string(),
                    tool_name: "mcp__alpha__do_thing".to_string(),
                    payload: ToolPayload::JsonArguments {
                        arguments: String::new(),
                    },
                },
                CancellationToken::new(),
            )
            .await?;

        assert_eq!(
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            vec![("do_thing".to_string(), Some(json!({})))]
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_handler_returns_failed_output_for_text_input()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = McpCallToolResult {
            content: Vec::new(),
            is_error: None,
            structured_content: None,
        };
        let (client, calls) = test_client(result);
        let handler = McpToolProvider::from_tools("alpha", client, vec![test_mcp_tool()])
            .into_tool_handlers()?
            .remove(0);

        let output = handler
            .handle(
                ToolInvocation {
                    call_id: "call-1".to_string(),
                    tool_name: "mcp__alpha__do_thing".to_string(),
                    payload: ToolPayload::TextInput {
                        input: "raw".to_string(),
                    },
                },
                CancellationToken::new(),
            )
            .await?;

        assert!(
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        match output {
            ToolOutput::Function {
                content,
                content_items,
                success,
            } => {
                assert!(content.contains("do not support text input"));
                assert_eq!(content_items, None);
                assert_eq!(success, Some(false));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn mcp_handler_returns_failed_output_for_mcp_call_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let client = TestMcpClient {
            tools: vec![test_mcp_tool()],
            result: McpCallToolResult {
                content: Vec::new(),
                is_error: None,
                structured_content: None,
            },
            calls: Arc::clone(&calls),
            error: Some("boom".to_string()),
        };
        let handler = McpToolProvider::from_tools("alpha", client, vec![test_mcp_tool()])
            .into_tool_handlers()?
            .remove(0);

        let output = handler
            .handle(
                ToolInvocation {
                    call_id: "call-1".to_string(),
                    tool_name: "mcp__alpha__do_thing".to_string(),
                    payload: ToolPayload::JsonArguments {
                        arguments: "{}".to_string(),
                    },
                },
                CancellationToken::new(),
            )
            .await?;

        assert_eq!(
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            vec![("do_thing".to_string(), Some(json!({})))]
        );
        assert_eq!(
            output,
            ToolOutput::Function {
                content: "tool call error: boom".to_string(),
                content_items: None,
                success: Some(false),
            }
        );
        Ok(())
    }
}
