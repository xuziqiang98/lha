use serde_json::Value;
use serde_json::json;

use crate::Error;
use crate::Result;
use crate::prompt::ToolSpec;

/// Returns JSON values that are compatible with Function Calling in the
/// Responses API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=responses
pub fn create_tools_json_for_responses_api(tools: &[ToolSpec]) -> Result<Vec<Value>> {
    tools
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn create_tools_json_for_messages_api(tools: &[ToolSpec]) -> Result<Vec<Value>> {
    let mut tools_json = Vec::new();

    for tool in tools {
        match tool {
            ToolSpec::Function(tool) => tools_json.push(json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })),
            ToolSpec::WebSearch { .. } | ToolSpec::Freeform(_) => {
                return Err(Error::UnsupportedOperation(format!(
                    "Messages API only supports function tools; unsupported tool: {}",
                    tool.name()
                )));
            }
        }
    }

    Ok(tools_json)
}

/// Returns JSON values that are compatible with Function Calling in the
/// Chat Completions API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=chat
pub fn create_tools_json_for_chat_completions_api(tools: &[ToolSpec]) -> Result<Vec<Value>> {
    let responses_api_tools_json = create_tools_json_for_responses_api(tools)?;
    Ok(responses_api_tools_json
        .into_iter()
        .filter_map(|mut tool| {
            if tool.get("type") != Some(&Value::String("function".to_string())) {
                return None;
            }

            tool.as_object_mut().map(|map| {
                let name = map
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                map.remove("type");
                json!({
                    "type": "function",
                    "name": name,
                    "function": map,
                })
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::JsonSchema;
    use crate::prompt::ResponsesApiTool;
    use std::collections::BTreeMap;

    #[test]
    fn chat_tools_include_top_level_name() {
        let properties =
            BTreeMap::from([("foo".to_string(), JsonSchema::String { description: None })]);
        let tools = vec![ToolSpec::Function(ResponsesApiTool {
            name: "demo".to_string(),
            description: "A demo tool".to_string(),
            strict: false,
            parameters: JsonSchema::Object {
                properties,
                required: None,
                additional_properties: None,
            },
        })];

        let responses_json = create_tools_json_for_responses_api(&tools).unwrap();
        assert_eq!(
            responses_json,
            vec![json!({
                "type": "function",
                "name": "demo",
                "description": "A demo tool",
                "strict": false,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "foo": { "type": "string" }
                    },
                },
            })]
        );

        let tools_json = create_tools_json_for_chat_completions_api(&tools).unwrap();

        assert_eq!(
            tools_json,
            vec![json!({
                "type": "function",
                "name": "demo",
                "function": {
                    "name": "demo",
                    "description": "A demo tool",
                    "strict": false,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "foo": { "type": "string" }
                        },
                    },
                }
            })]
        );
    }

    #[test]
    fn messages_tools_accept_function_named_local_shell() {
        let tools = vec![ToolSpec::Function(ResponsesApiTool {
            name: "local_shell".to_string(),
            description: "Execute a local shell command.".to_string(),
            strict: false,
            parameters: JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(false.into()),
            },
        })];

        let tools_json =
            create_tools_json_for_messages_api(&tools).expect("local_shell function tool");

        assert_eq!(
            tools_json,
            vec![json!({
                "name": "local_shell",
                "description": "Execute a local shell command.",
                "input_schema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                },
            })]
        );
    }

    #[test]
    fn messages_tools_reject_web_search() {
        let tools = vec![ToolSpec::WebSearch {
            external_web_access: None,
        }];

        let err = create_tools_json_for_messages_api(&tools).expect_err("should reject web search");

        assert_eq!(
            err.to_string(),
            "unsupported operation: Messages API only supports function tools; unsupported tool: web_search"
        );
    }
}
