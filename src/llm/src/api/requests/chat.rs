use crate::api::error::ApiError;
use crate::api::provider::Provider;
use crate::api::requests::headers::build_conversation_headers;
use crate::api::requests::headers::insert_header;
use crate::api::requests::headers::subagent_header;
use crate::types::ContentItem;
use crate::types::ReasoningContentItem;
use crate::types::ToolCallPayload;
use crate::types::ToolResultContentItem;
use crate::types::ToolResultPayload;
use crate::types::TranscriptItem;
use http::HeaderMap;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;

/// Assembled request body plus headers for Chat Completions streaming calls.
pub struct ChatRequest {
    pub body: Value,
    pub headers: HeaderMap,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeveloperRoleHandling {
    #[default]
    Preserve,
    DowngradeToSystem,
}

pub struct ChatRequestBuilder<'a> {
    model: &'a str,
    instructions: &'a str,
    input: &'a [TranscriptItem],
    tools: &'a [Value],
    conversation_id: Option<String>,
    origin_tag: Option<String>,
    developer_role_handling: DeveloperRoleHandling,
}

impl<'a> ChatRequestBuilder<'a> {
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
            conversation_id: None,
            origin_tag: None,
            developer_role_handling: DeveloperRoleHandling::Preserve,
        }
    }

    pub fn conversation_id(mut self, id: Option<String>) -> Self {
        self.conversation_id = id;
        self
    }

    pub fn origin_tag(mut self, origin_tag: Option<String>) -> Self {
        self.origin_tag = origin_tag;
        self
    }

    pub fn developer_role_handling(mut self, handling: DeveloperRoleHandling) -> Self {
        self.developer_role_handling = handling;
        self
    }

    pub fn build(self, _provider: &Provider) -> Result<ChatRequest, ApiError> {
        let mut messages = Vec::<Value>::new();
        messages.push(json!({"role": "system", "content": self.instructions}));

        let input = self.input;
        let mut reasoning_by_anchor_index: HashMap<usize, String> = HashMap::new();
        let mut last_emitted_role: Option<&str> = None;
        for item in input {
            match item {
                TranscriptItem::Message { role, .. } => last_emitted_role = Some(role.as_str()),
                TranscriptItem::ToolCall { .. } => last_emitted_role = Some("assistant"),
                TranscriptItem::ToolResult { .. } => last_emitted_role = Some("tool"),
                TranscriptItem::Reasoning { .. }
                | TranscriptItem::HostedActivity { .. }
                | TranscriptItem::Unknown { .. } => {}
            }
        }

        let mut last_user_index: Option<usize> = None;
        for (idx, item) in input.iter().enumerate() {
            if let TranscriptItem::Message { role, .. } = item
                && role == "user"
            {
                last_user_index = Some(idx);
            }
        }

        if !matches!(last_emitted_role, Some("user")) {
            for (idx, item) in input.iter().enumerate() {
                if let Some(u_idx) = last_user_index
                    && idx <= u_idx
                {
                    continue;
                }

                if let TranscriptItem::Reasoning {
                    content: Some(items),
                    ..
                } = item
                {
                    let mut text = String::new();
                    for entry in items {
                        match entry {
                            ReasoningContentItem::ReasoningText { text: segment }
                            | ReasoningContentItem::Text { text: segment } => {
                                text.push_str(segment)
                            }
                        }
                    }
                    if text.trim().is_empty() {
                        continue;
                    }

                    let mut attached = false;
                    if idx > 0
                        && let TranscriptItem::Message { role, .. } = &input[idx - 1]
                        && role == "assistant"
                    {
                        reasoning_by_anchor_index
                            .entry(idx - 1)
                            .and_modify(|v| v.push_str(&text))
                            .or_insert(text.clone());
                        attached = true;
                    }

                    if !attached && idx + 1 < input.len() {
                        // Normalized completions retain text before tool calls, but provider
                        // reasoning must remain attached to the tool-call assistant message.
                        if idx + 2 < input.len()
                            && matches!(
                                &input[idx + 1],
                                TranscriptItem::Message { role, .. } if role == "assistant"
                            )
                            && matches!(&input[idx + 2], TranscriptItem::ToolCall { .. })
                        {
                            reasoning_by_anchor_index
                                .entry(idx + 2)
                                .and_modify(|v| v.push_str(&text))
                                .or_insert(text.clone());
                            continue;
                        }

                        match &input[idx + 1] {
                            TranscriptItem::ToolCall { .. } => {
                                reasoning_by_anchor_index
                                    .entry(idx + 1)
                                    .and_modify(|v| v.push_str(&text))
                                    .or_insert(text.clone());
                            }
                            TranscriptItem::Message { role, .. } if role == "assistant" => {
                                reasoning_by_anchor_index
                                    .entry(idx + 1)
                                    .and_modify(|v| v.push_str(&text))
                                    .or_insert(text.clone());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        let mut last_assistant_text: Option<String> = None;

        for (idx, item) in input.iter().enumerate() {
            match item {
                TranscriptItem::Message { role, content, .. } => {
                    let role = match (role.as_str(), self.developer_role_handling) {
                        ("developer", DeveloperRoleHandling::DowngradeToSystem) => "system",
                        _ => role.as_str(),
                    };
                    let mut text = String::new();
                    let mut items: Vec<Value> = Vec::new();
                    let mut saw_image = false;

                    for c in content {
                        match c {
                            ContentItem::InputText { text: t }
                            | ContentItem::OutputText { text: t } => {
                                text.push_str(t);
                                items.push(json!({"type":"text","text": t}));
                            }
                            ContentItem::InputImage { image_url } => {
                                saw_image = true;
                                items.push(
                                    json!({"type":"image_url","image_url": {"url": image_url}}),
                                );
                            }
                        }
                    }

                    if role == "assistant" {
                        if let Some(prev) = &last_assistant_text
                            && prev == &text
                        {
                            continue;
                        }
                        last_assistant_text = Some(text.clone());
                    }

                    if role == "assistant"
                        && !saw_image
                        && let Some(Value::Object(obj)) = messages.last_mut()
                        && obj.get("role").and_then(Value::as_str) == Some("assistant")
                        && obj
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .is_some_and(|calls| !calls.is_empty())
                    {
                        // Same-response trailing text: fold it into the preceding
                        // tool_calls assistant message so tool results still immediately
                        // follow it. Strict chat validators reject a separate assistant
                        // message in between ("insufficient tool messages following
                        // tool_calls message").
                        match obj.get_mut("content") {
                            Some(Value::String(existing)) => existing.push_str(&text),
                            _ => {
                                obj.insert("content".to_string(), Value::String(text.clone()));
                            }
                        }
                        if let Some(reasoning) = reasoning_by_anchor_index.get(&idx) {
                            match obj.get_mut("reasoning") {
                                Some(Value::String(existing)) if !existing.is_empty() => {
                                    existing.push('\n');
                                    existing.push_str(reasoning);
                                }
                                _ => {
                                    obj.insert(
                                        "reasoning".to_string(),
                                        Value::String(reasoning.clone()),
                                    );
                                }
                            }
                        }
                        continue;
                    }

                    let content_value = if role == "assistant" {
                        json!(text)
                    } else if saw_image {
                        json!(items)
                    } else {
                        json!(text)
                    };

                    let mut msg = json!({"role": role, "content": content_value});
                    if role == "assistant"
                        && let Some(reasoning) = reasoning_by_anchor_index.get(&idx)
                        && let Some(obj) = msg.as_object_mut()
                    {
                        obj.insert("reasoning".to_string(), json!(reasoning));
                    }
                    messages.push(msg);
                }
                TranscriptItem::ToolCall {
                    call_id,
                    tool_name,
                    payload: ToolCallPayload::JsonArguments { arguments },
                    ..
                } => {
                    let reasoning = reasoning_by_anchor_index.get(&idx).map(String::as_str);
                    let tool_call = json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "arguments": arguments,
                        }
                    });
                    push_tool_call_message(&mut messages, tool_call, reasoning);
                }
                TranscriptItem::ToolCall {
                    id,
                    tool_name,
                    payload: ToolCallPayload::TextInput { input },
                    ..
                } => {
                    let tool_call = json!({
                        "id": id,
                        "type": "custom",
                        "custom": {
                            "name": tool_name,
                            "input": input,
                        }
                    });
                    let reasoning = reasoning_by_anchor_index.get(&idx).map(String::as_str);
                    push_tool_call_message(&mut messages, tool_call, reasoning);
                }
                TranscriptItem::ToolResult {
                    call_id,
                    payload:
                        ToolResultPayload::Structured {
                            content,
                            content_items,
                            ..
                        },
                    ..
                } => {
                    let content_value = if let Some(items) = content_items {
                        let mapped: Vec<Value> = items
                            .iter()
                            .map(|it| match it {
                                ToolResultContentItem::InputText { text } => {
                                    json!({"type":"text","text": text})
                                }
                                ToolResultContentItem::InputImage { image_url } => {
                                    json!({"type":"image_url","image_url": {"url": image_url}})
                                }
                            })
                            .collect();
                        json!(mapped)
                    } else {
                        json!(content)
                    };

                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": content_value,
                    }));
                }
                TranscriptItem::ToolResult {
                    call_id,
                    payload: ToolResultPayload::Text { output },
                    ..
                } => {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output,
                    }));
                }
                TranscriptItem::Reasoning { .. }
                | TranscriptItem::HostedActivity { .. }
                | TranscriptItem::Unknown { .. } => {
                    continue;
                }
            }
        }

        let payload = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "tools": self.tools,
        });

        let mut headers = build_conversation_headers(self.conversation_id);
        if let Some(subagent) = subagent_header(&self.origin_tag) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        Ok(ChatRequest {
            body: payload,
            headers,
        })
    }
}

fn push_tool_call_message(messages: &mut Vec<Value>, tool_call: Value, reasoning: Option<&str>) {
    // Chat Completions requires that tool calls are grouped into a single assistant message
    // (with `tool_calls: [...]`) followed by tool role responses.
    if let Some(Value::Object(obj)) = messages.last_mut()
        && obj.get("role").and_then(Value::as_str) == Some("assistant")
        && obj.get("content").is_some_and(Value::is_null)
        && let Some(tool_calls) = obj.get_mut("tool_calls").and_then(Value::as_array_mut)
    {
        tool_calls.push(tool_call);
        if let Some(reasoning) = reasoning {
            if let Some(Value::String(existing)) = obj.get_mut("reasoning") {
                if !existing.is_empty() {
                    existing.push('\n');
                }
                existing.push_str(reasoning);
            } else {
                obj.insert(
                    "reasoning".to_string(),
                    Value::String(reasoning.to_string()),
                );
            }
        }
        return;
    }

    let mut msg = json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [tool_call],
    });
    if let Some(reasoning) = reasoning
        && let Some(obj) = msg.as_object_mut()
    {
        obj.insert("reasoning".to_string(), json!(reasoning));
    }
    messages.push(msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::provider::RetryConfig;
    use crate::api::provider::WireApi;
    use crate::types::ToolCallPayload;
    use crate::types::ToolResultPayload;
    use crate::types::TranscriptItem;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    fn text_message(role: &str, text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }

    fn provider() -> Provider {
        Provider {
            name: "openai".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            query_params: None,
            wire: WireApi::Chat,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(10),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn attaches_conversation_and_subagent_headers() {
        let prompt_input = vec![TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hi".to_string(),
            }],
            end_turn: None,
        }];
        let req = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .conversation_id(Some("conv-1".into()))
            .origin_tag(Some("review".into()))
            .build(&provider())
            .expect("request");

        assert_eq!(
            req.headers.get("session_id"),
            Some(&HeaderValue::from_static("conv-1"))
        );
        assert_eq!(
            req.headers.get("x-openai-subagent"),
            Some(&HeaderValue::from_static("review"))
        );
    }

    #[test]
    fn groups_consecutive_tool_calls_into_a_single_assistant_message() {
        let prompt_input = vec![
            text_message("user", "read these"),
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-a".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"a.txt"}"#.to_string(),
                },
            },
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-b".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"b.txt"}"#.to_string(),
                },
            },
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-c".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"c.txt"}"#.to_string(),
                },
            },
            TranscriptItem::ToolResult {
                call_id: "call-a".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "A".to_string(),
                    content_items: None,
                    success: None,
                },
            },
            TranscriptItem::ToolResult {
                call_id: "call-b".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "B".to_string(),
                    content_items: None,
                    success: None,
                },
            },
            TranscriptItem::ToolResult {
                call_id: "call-c".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "C".to_string(),
                    content_items: None,
                    success: None,
                },
            },
        ];

        let req = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .build(&provider())
            .expect("request");

        let messages = req
            .body
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages array");
        // system + user + assistant(tool_calls=[...]) + 3 tool outputs
        assert_eq!(messages.len(), 6);

        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");

        let tool_calls_msg = &messages[2];
        assert_eq!(tool_calls_msg["role"], "assistant");
        assert_eq!(tool_calls_msg["content"], serde_json::Value::Null);
        let tool_calls = tool_calls_msg["tool_calls"]
            .as_array()
            .expect("tool_calls array");
        assert_eq!(tool_calls.len(), 3);
        assert_eq!(tool_calls[0]["id"], "call-a");
        assert_eq!(tool_calls[1]["id"], "call-b");
        assert_eq!(tool_calls[2]["id"], "call-c");

        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call-a");
        assert_eq!(messages[4]["role"], "tool");
        assert_eq!(messages[4]["tool_call_id"], "call-b");
        assert_eq!(messages[5]["role"], "tool");
        assert_eq!(messages[5]["tool_call_id"], "call-c");
    }

    #[test]
    fn keeps_tool_results_adjacent_after_assistant_text_and_preserves_tool_reasoning() {
        let prompt_input = vec![
            text_message("user", "read the file"),
            TranscriptItem::Reasoning {
                id: "reasoning-1".to_string(),
                summary: vec![],
                content: Some(vec![ReasoningContentItem::ReasoningText {
                    text: "Need the file contents.".to_string(),
                }]),
                encrypted_content: None,
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
                call_id: "call-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"README.md"}"#.to_string(),
                },
            },
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "contents".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            },
        ];

        let request = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .build(&provider())
            .expect("request");
        let messages = request.body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 5);
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "I will read it.");
        assert!(messages[2].get("reasoning").is_none());
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[3]["content"], serde_json::Value::Null);
        assert_eq!(messages[3]["reasoning"], "Need the file contents.");
        assert_eq!(messages[3]["tool_calls"][0]["id"], "call-1");
        assert_eq!(messages[4]["role"], "tool");
        assert_eq!(messages[4]["tool_call_id"], "call-1");
    }

    #[test]
    fn preserves_developer_role_by_default() {
        let prompt_input = vec![text_message("developer", "stay sharp")];

        let req = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .build(&provider())
            .expect("request");

        let messages = req.body["messages"].as_array().expect("messages array");
        assert_eq!(messages[1]["role"], "developer");
        assert_eq!(messages[1]["content"], "stay sharp");
    }

    #[test]
    fn downgrades_developer_role_to_system_when_requested() {
        let prompt_input = vec![
            text_message("user", "hi"),
            text_message("developer", "follow repo rules"),
            text_message("assistant", "ok"),
        ];

        let req = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .developer_role_handling(DeveloperRoleHandling::DowngradeToSystem)
            .build(&provider())
            .expect("request");

        let messages = req.body["messages"].as_array().expect("messages array");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "system");
        assert_eq!(messages[2]["content"], "follow repo rules");
        assert_eq!(messages[3]["role"], "assistant");
    }

    #[test]
    fn merges_same_response_trailing_text_into_tool_calls_message() {
        let prompt_input = vec![
            text_message("user", "check the repo"),
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"README.md"}"#.to_string(),
                },
            },
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-2".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                },
            },
            text_message("assistant", "\n"),
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "readme".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            },
            TranscriptItem::ToolResult {
                call_id: "call-2".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "cargo".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            },
        ];

        let request = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .build(&provider())
            .expect("request");
        let messages = request.body["messages"].as_array().expect("messages array");

        // system + user + merged assistant{content, tool_calls} + 2 tool results;
        // no standalone assistant text message may sit between the tool_calls
        // message and the tool results.
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "\n");
        let tool_calls = messages[2]["tool_calls"]
            .as_array()
            .expect("tool_calls array");
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0]["id"], "call-1");
        assert_eq!(tool_calls[1]["id"], "call-2");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call-1");
        assert_eq!(messages[4]["role"], "tool");
        assert_eq!(messages[4]["tool_call_id"], "call-2");
    }

    #[test]
    fn merges_same_response_real_text_into_tool_calls_message() {
        let prompt_input = vec![
            text_message("user", "find the entrypoint"),
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-1".to_string(),
                tool_name: "grep".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"pattern":"fn main"}"#.to_string(),
                },
            },
            text_message("assistant", "Let me check the repo first."),
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "grep".to_string(),
                payload: ToolResultPayload::Text {
                    output: "src/main.rs:1".to_string(),
                },
            },
        ];

        let request = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .build(&provider())
            .expect("request");
        let messages = request.body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "Let me check the repo first.");
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call-1");
    }

    #[test]
    fn merges_reasoning_anchored_to_trailing_assistant_text() {
        let prompt_input = vec![
            text_message("user", "run the checks"),
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-1".to_string(),
                tool_name: "shell".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"command":"cargo test"}"#.to_string(),
                },
            },
            TranscriptItem::Reasoning {
                id: "reasoning-1".to_string(),
                summary: vec![],
                content: Some(vec![ReasoningContentItem::ReasoningText {
                    text: "Tests will surface regressions.".to_string(),
                }]),
                encrypted_content: None,
            },
            text_message("assistant", "Running the test suite."),
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "shell".to_string(),
                payload: ToolResultPayload::Text {
                    output: "ok. 12 passed".to_string(),
                },
            },
        ];

        let request = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .build(&provider())
            .expect("request");
        let messages = request.body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "Running the test suite.");
        assert_eq!(messages[2]["reasoning"], "Tests will surface regressions.");
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call-1");
    }

    #[test]
    fn merges_multiple_trailing_texts_into_the_same_tool_calls_message() {
        let prompt_input = vec![
            text_message("user", "inspect the layout"),
            TranscriptItem::ToolCall {
                id: None,
                call_id: "call-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: r#"{"path":"layout.rs"}"#.to_string(),
                },
            },
            text_message("assistant", "Checking."),
            text_message("assistant", " Found it."),
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "read_file".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "layout".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            },
        ];

        let request = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .build(&provider())
            .expect("request");
        let messages = request.body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "Checking. Found it.");
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call-1");
    }
}
