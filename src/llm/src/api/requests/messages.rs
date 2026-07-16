use crate::api::error::ApiError;
use crate::api::provider::Provider;
use http::HeaderMap;
use http::HeaderValue;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::types::ContentItem;
use crate::types::ToolCallPayload;
use crate::types::ToolResultContentItem;
use crate::types::ToolResultPayload;
use crate::types::TranscriptItem;

const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8_192;
const NON_MESSAGES_ROLE_CONTENT_ERROR: &str =
    "messages system prompt folding only supports text content for non-user/assistant messages";

#[derive(Debug)]
pub struct MessagesRequest {
    pub body: Value,
    pub headers: HeaderMap,
}

pub struct MessagesRequestBuilder<'a> {
    model: &'a str,
    instructions: &'a str,
    input: &'a [TranscriptItem],
    tools: &'a [Value],
    parallel_tool_calls: bool,
    max_tokens: u32,
}

impl<'a> MessagesRequestBuilder<'a> {
    pub fn new(
        model: &'a str,
        instructions: &'a str,
        input: &'a [TranscriptItem],
        tools: &'a [Value],
    ) -> Self {
        Self {
            model,
            instructions,
            input,
            tools,
            parallel_tool_calls: false,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    pub fn parallel_tool_calls(mut self, enabled: bool) -> Self {
        self.parallel_tool_calls = enabled;
        self
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn build(self, provider: &Provider) -> Result<MessagesRequest, ApiError> {
        let system = build_system_prompt(self.instructions, self.input)?;
        let messages = build_messages(self.input)?;
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(self.model.to_string()));
        body.insert("system".to_string(), Value::String(system));
        body.insert("messages".to_string(), Value::Array(messages));
        body.insert("stream".to_string(), Value::Bool(true));
        body.insert("max_tokens".to_string(), Value::from(self.max_tokens));

        if !self.tools.is_empty() {
            let mut tool_choice = Map::new();
            tool_choice.insert("type".to_string(), Value::String("auto".to_string()));
            if !self.parallel_tool_calls {
                tool_choice.insert("disable_parallel_tool_use".to_string(), Value::Bool(true));
            }

            body.insert("tools".to_string(), Value::Array(self.tools.to_vec()));
            body.insert("tool_choice".to_string(), Value::Object(tool_choice));
        }

        let mut headers = HeaderMap::new();
        if !provider.headers.contains_key("anthropic-version") {
            headers.insert(
                "anthropic-version",
                HeaderValue::from_static(DEFAULT_ANTHROPIC_VERSION),
            );
        }

        Ok(MessagesRequest {
            body: Value::Object(body),
            headers,
        })
    }
}

fn build_messages(input: &[TranscriptItem]) -> Result<Vec<Value>, ApiError> {
    let mut messages = Vec::new();

    for item in input {
        match item {
            TranscriptItem::Message { role, content, .. } => {
                if is_messages_role(role) {
                    let blocks = map_message_content(content);
                    if !blocks.is_empty() {
                        messages.push(json!({"role": role, "content": blocks}));
                    }
                }
            }
            TranscriptItem::ToolCall {
                call_id,
                tool_name,
                payload: ToolCallPayload::JsonArguments { arguments },
                ..
            } => {
                push_assistant_tool_use(
                    &mut messages,
                    json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": tool_name,
                        "input": parse_tool_input(arguments)?,
                    }),
                );
            }
            TranscriptItem::ToolCall {
                call_id,
                tool_name,
                payload: ToolCallPayload::TextInput { input },
                ..
            } => {
                push_assistant_tool_use(
                    &mut messages,
                    json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": tool_name,
                        "input": {"input": input},
                    }),
                );
            }
            TranscriptItem::ToolResult {
                call_id, payload, ..
            } => {
                push_user_tool_result(&mut messages, build_tool_result_block(call_id, payload));
            }
            TranscriptItem::Reasoning { .. }
            | TranscriptItem::HostedActivity { .. }
            | TranscriptItem::Unknown { .. } => {}
        }
    }

    Ok(messages)
}

fn build_system_prompt(
    base_instructions: &str,
    input: &[TranscriptItem],
) -> Result<String, ApiError> {
    let mut parts = Vec::new();

    if !base_instructions.is_empty() {
        parts.push(base_instructions.to_string());
    }

    for item in input {
        if let TranscriptItem::Message { role, content, .. } = item
            && !is_messages_role(role)
            && let Some(text) = message_text_for_system(content)?
        {
            parts.push(text);
        }
    }

    Ok(parts.join("\n\n"))
}

fn is_messages_role(role: &str) -> bool {
    matches!(role, "user" | "assistant")
}

fn message_text_for_system(content: &[ContentItem]) -> Result<Option<String>, ApiError> {
    let mut text_parts = Vec::new();

    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                if !text.is_empty() {
                    text_parts.push(text.as_str());
                }
            }
            ContentItem::InputImage { .. } => {
                return Err(ApiError::Stream(
                    NON_MESSAGES_ROLE_CONTENT_ERROR.to_string(),
                ));
            }
        }
    }

    if text_parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text_parts.join("\n")))
    }
}

fn map_message_content(content: &[ContentItem]) -> Vec<Value> {
    content
        .iter()
        .map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                json!({"type": "text", "text": text})
            }
            ContentItem::InputImage { image_url } => {
                json!({
                    "type": "image",
                    "source": {
                        "type": "url",
                        "url": image_url,
                    }
                })
            }
        })
        .collect()
}

fn map_tool_result_content(payload: &ToolResultPayload) -> Vec<Value> {
    match payload {
        ToolResultPayload::Structured {
            content,
            content_items,
            ..
        } => {
            if let Some(items) = content_items {
                return items
                    .iter()
                    .map(|item| match item {
                        ToolResultContentItem::InputText { text } => {
                            json!({"type": "text", "text": text})
                        }
                        ToolResultContentItem::InputImage { image_url } => {
                            json!({
                                "type": "image",
                                "source": {
                                    "type": "url",
                                    "url": image_url,
                                }
                            })
                        }
                    })
                    .collect();
            }

            vec![json!({"type": "text", "text": content})]
        }
        ToolResultPayload::Text { output } => {
            vec![json!({"type": "text", "text": output})]
        }
    }
}

fn build_tool_result_block(call_id: &str, payload: &ToolResultPayload) -> Value {
    let mut block = json!({
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": map_tool_result_content(payload),
    });

    if matches!(
        payload,
        ToolResultPayload::Structured {
            success: Some(false),
            ..
        }
    ) && let Some(obj) = block.as_object_mut()
    {
        obj.insert("is_error".to_string(), Value::Bool(true));
    }

    block
}

fn parse_tool_input(arguments: &str) -> Result<Value, ApiError> {
    let parsed = serde_json::from_str::<Value>(arguments).map_err(|err| {
        ApiError::Stream(format!(
            "messages tool input must be valid JSON object: {err}"
        ))
    })?;
    if parsed.is_object() {
        Ok(parsed)
    } else {
        Err(ApiError::Stream(
            "messages tool input must decode to a JSON object".to_string(),
        ))
    }
}

fn push_assistant_tool_use(messages: &mut Vec<Value>, block: Value) {
    if let Some(Value::Object(message)) = messages.last_mut()
        && message.get("role").and_then(Value::as_str) == Some("assistant")
        && let Some(content) = message.get_mut("content").and_then(Value::as_array_mut)
        && content
            .iter()
            .all(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
    {
        content.push(block);
        return;
    }

    messages.push(json!({"role": "assistant", "content": [block]}));
}

fn push_user_tool_result(messages: &mut Vec<Value>, block: Value) {
    if let Some(Value::Object(message)) = messages.last_mut()
        && message.get("role").and_then(Value::as_str) == Some("user")
        && let Some(content) = message.get_mut("content").and_then(Value::as_array_mut)
        && content
            .iter()
            .all(|item| item.get("type").and_then(Value::as_str) == Some("tool_result"))
    {
        content.push(block);
        return;
    }

    messages.push(json!({"role": "user", "content": [block]}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::provider::RetryConfig;
    use crate::api::provider::WireApi;
    use crate::types::ToolCallPayload;
    use crate::types::ToolResultPayload;
    use crate::types::TranscriptItem;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    fn provider() -> Provider {
        Provider {
            name: "anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            query_params: None,
            wire: WireApi::Messages,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn builds_messages_request_without_tools() {
        let input = vec![
            TranscriptItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "hello".to_string(),
                }],
                end_turn: None,
            },
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"src/main.rs"}"#.to_string(),
                },
            },
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "done".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            },
        ];

        let request = MessagesRequestBuilder::new("claude", "be helpful", &input, &[])
            .build(&provider())
            .expect("request should build");

        assert_eq!(
            request.body["system"],
            Value::String("be helpful".to_string())
        );
        assert_eq!(request.body["max_tokens"], Value::from(DEFAULT_MAX_TOKENS));
        assert_eq!(
            request.body["messages"].as_array().expect("messages").len(),
            3
        );
        assert!(request.body.get("tools").is_none());
        assert!(request.body.get("tool_choice").is_none());
        assert_eq!(
            request
                .headers
                .get("anthropic-version")
                .and_then(|h| h.to_str().ok()),
            Some(DEFAULT_ANTHROPIC_VERSION)
        );
    }

    #[test]
    fn builds_messages_request_with_tools() {
        let input = vec![TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hello".to_string(),
            }],
            end_turn: None,
        }];
        let tools = vec![json!({
            "name": "read_file",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        })];

        let request = MessagesRequestBuilder::new("claude", "be helpful", &input, &tools)
            .build(&provider())
            .expect("request should build");

        assert_eq!(request.body["tools"], Value::Array(tools));
        assert_eq!(
            request.body["tool_choice"]["type"],
            Value::String("auto".to_string())
        );
        assert_eq!(
            request.body["tool_choice"]["disable_parallel_tool_use"],
            Value::Bool(true)
        );
    }

    #[test]
    fn keeps_tool_results_adjacent_after_assistant_text() {
        let input = vec![
            TranscriptItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "read the file".to_string(),
                }],
                end_turn: None,
            },
            TranscriptItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "I will read it.".to_string(),
                }],
                end_turn: None,
            },
            TranscriptItem::ToolCall {
                id: None,
                call_id: "toolu-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"README.md"}"#.to_string(),
                },
            },
            TranscriptItem::ToolResult {
                call_id: "toolu-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "contents".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            },
        ];

        let request = MessagesRequestBuilder::new("claude", "be helpful", &input, &[])
            .build(&provider())
            .expect("request should build");
        let messages = request.body["messages"].as_array().expect("messages");

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["type"], "text");
        assert_eq!(messages[1]["content"][0]["text"], "I will read it.");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"][0]["type"], "tool_use");
        assert_eq!(messages[2]["content"][0]["id"], "toolu-1");
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"][0]["type"], "tool_result");
        assert_eq!(messages[3]["content"][0]["tool_use_id"], "toolu-1");
    }

    #[test]
    fn folds_non_user_assistant_messages_into_system_prompt() {
        let input = vec![
            TranscriptItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: "perm msg".to_string(),
                }],
                end_turn: None,
            },
            TranscriptItem::Message {
                id: None,
                role: "system".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "legacy system msg".to_string(),
                }],
                end_turn: None,
            },
            TranscriptItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "hello".to_string(),
                }],
                end_turn: None,
            },
            TranscriptItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "hi".to_string(),
                }],
                end_turn: None,
            },
        ];

        let request = MessagesRequestBuilder::new("claude", "be helpful", &input, &[])
            .build(&provider())
            .expect("request should build");

        let system = request.body["system"]
            .as_str()
            .expect("system should be text");
        assert!(system.contains("be helpful"));
        assert!(system.contains("perm msg"));
        assert!(system.contains("legacy system msg"));
        assert!(
            system.find("perm msg") < system.find("legacy system msg"),
            "folded system text should preserve message order"
        );

        let messages = request.body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], Value::String("user".to_string()));
        assert_eq!(messages[1]["role"], Value::String("assistant".to_string()));
    }

    #[test]
    fn parallel_tool_calls_omit_disable_parallel_tool_use() {
        let input = vec![TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hello".to_string(),
            }],
            end_turn: None,
        }];
        let tools = vec![json!({
            "name": "read_file",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            }
        })];

        let request = MessagesRequestBuilder::new("claude", "be helpful", &input, &tools)
            .parallel_tool_calls(true)
            .build(&provider())
            .expect("request should build");

        assert_eq!(
            request.body["tool_choice"]["type"],
            Value::String("auto".to_string())
        );
        assert!(
            request.body["tool_choice"]
                .get("disable_parallel_tool_use")
                .is_none()
        );
    }

    #[test]
    fn failed_function_call_outputs_include_is_error() {
        let input = vec![TranscriptItem::ToolResult {
            call_id: "call-1".to_string(),
            tool_name: "read_file".to_string(),
            payload: ToolResultPayload::Structured {
                content: "denied".to_string(),
                content_items: None,
                success: Some(false),
            },
        }];

        let request = MessagesRequestBuilder::new("claude", "be helpful", &input, &[])
            .build(&provider())
            .expect("request should build");
        let message = &request.body["messages"].as_array().expect("messages")[0];
        let block = &message["content"].as_array().expect("content")[0];

        assert_eq!(block["type"], Value::String("tool_result".to_string()));
        assert_eq!(block["tool_use_id"], Value::String("call-1".to_string()));
        assert_eq!(block["is_error"], Value::Bool(true));
    }

    #[test]
    fn successful_function_call_outputs_omit_is_error() {
        let input = vec![TranscriptItem::ToolResult {
            call_id: "call-1".to_string(),
            tool_name: "read_file".to_string(),
            payload: ToolResultPayload::Structured {
                content: "done".to_string(),
                content_items: None,
                success: Some(true),
            },
        }];

        let request = MessagesRequestBuilder::new("claude", "be helpful", &input, &[])
            .build(&provider())
            .expect("request should build");
        let message = &request.body["messages"].as_array().expect("messages")[0];
        let block = &message["content"].as_array().expect("content")[0];

        assert!(block.get("is_error").is_none());
    }

    #[test]
    fn rejects_images_in_folded_system_messages() {
        let input = vec![TranscriptItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputImage {
                image_url: "https://example.com/image.png".to_string(),
            }],
            end_turn: None,
        }];

        let err = MessagesRequestBuilder::new("claude", "be helpful", &input, &[])
            .build(&provider())
            .expect_err("request should fail");

        assert_eq!(
            err.to_string(),
            format!("stream error: {NON_MESSAGES_ROLE_CONTENT_ERROR}")
        );
    }

    #[test]
    fn rejects_non_object_tool_input() {
        let input = vec![TranscriptItem::ToolCall {
            id: None,
            call_id: "call-1".to_string(),
            tool_name: "oops".to_string(),
            payload: ToolCallPayload::JsonArguments {
                arguments: "[]".to_string(),
            },
        }];

        let err = MessagesRequestBuilder::new("claude", "be helpful", &input, &[])
            .build(&provider())
            .expect_err("request should fail");

        assert_eq!(
            err.to_string(),
            "stream error: messages tool input must decode to a JSON object"
        );
    }
}
