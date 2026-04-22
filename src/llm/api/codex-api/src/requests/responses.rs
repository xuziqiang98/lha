use crate::common::Reasoning;
use crate::common::ResponsesApiRequest;
use crate::common::TextControls;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::headers::build_conversation_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use codex_llm_types::ToolCallPayload;
use codex_llm_types::ToolResultPayload;
use codex_llm_types::TranscriptItem;
use http::HeaderMap;
use serde_json::Value;
use serde_json::json;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Compression {
    #[default]
    None,
    Zstd,
}

/// Assembled request body plus headers for a Responses stream request.
pub struct ResponsesRequest {
    pub body: Value,
    pub headers: HeaderMap,
    pub compression: Compression,
}

#[derive(Default)]
pub struct ResponsesRequestBuilder<'a> {
    model: Option<&'a str>,
    instructions: Option<&'a str>,
    input: Option<&'a [TranscriptItem]>,
    tools: Option<&'a [Value]>,
    parallel_tool_calls: bool,
    reasoning: Option<Reasoning>,
    include: Vec<String>,
    prompt_cache_key: Option<String>,
    text: Option<TextControls>,
    conversation_id: Option<String>,
    origin_tag: Option<String>,
    store_override: Option<bool>,
    headers: HeaderMap,
    compression: Compression,
}

impl<'a> ResponsesRequestBuilder<'a> {
    pub fn new(model: &'a str, instructions: &'a str, input: &'a [TranscriptItem]) -> Self {
        Self {
            model: Some(model),
            instructions: Some(instructions),
            input: Some(input),
            ..Default::default()
        }
    }

    pub fn tools(mut self, tools: &'a [Value]) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn parallel_tool_calls(mut self, enabled: bool) -> Self {
        self.parallel_tool_calls = enabled;
        self
    }

    pub fn reasoning(mut self, reasoning: Option<Reasoning>) -> Self {
        self.reasoning = reasoning;
        self
    }

    pub fn include(mut self, include: Vec<String>) -> Self {
        self.include = include;
        self
    }

    pub fn prompt_cache_key(mut self, key: Option<String>) -> Self {
        self.prompt_cache_key = key;
        self
    }

    pub fn text(mut self, text: Option<TextControls>) -> Self {
        self.text = text;
        self
    }

    pub fn conversation(mut self, conversation_id: Option<String>) -> Self {
        self.conversation_id = conversation_id;
        self
    }

    pub fn origin_tag(mut self, origin_tag: Option<String>) -> Self {
        self.origin_tag = origin_tag;
        self
    }

    pub fn store_override(mut self, store: Option<bool>) -> Self {
        self.store_override = store;
        self
    }

    pub fn extra_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    pub fn build(self, provider: &Provider) -> Result<ResponsesRequest, ApiError> {
        let model = self
            .model
            .ok_or_else(|| ApiError::Stream("missing model for responses request".into()))?;
        let instructions = self
            .instructions
            .ok_or_else(|| ApiError::Stream("missing instructions for responses request".into()))?;
        let input = self
            .input
            .ok_or_else(|| ApiError::Stream("missing input for responses request".into()))?;
        let tools = self.tools.unwrap_or_default();

        let store = self
            .store_override
            .unwrap_or_else(|| provider.is_azure_responses_endpoint());

        let req = ResponsesApiRequest {
            model,
            instructions,
            input,
            tools,
            tool_choice: "auto",
            parallel_tool_calls: self.parallel_tool_calls,
            reasoning: self.reasoning,
            store,
            stream: true,
            include: self.include,
            prompt_cache_key: self.prompt_cache_key,
            text: self.text,
        };

        let mut body = serde_json::to_value(&req)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;

        if let Some(obj) = body.as_object_mut() {
            let provider_input = input
                .iter()
                .map(transcript_item_to_provider_value)
                .collect::<Result<Vec<_>, _>>()?;
            obj.insert("input".to_string(), Value::Array(provider_input));
        }

        if store && provider.is_azure_responses_endpoint() {
            attach_item_ids(&mut body, input);
        }

        let mut headers = self.headers;
        headers.extend(build_conversation_headers(self.conversation_id));
        if let Some(subagent) = subagent_header(&self.origin_tag) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        Ok(ResponsesRequest {
            body,
            headers,
            compression: self.compression,
        })
    }
}

fn transcript_item_to_provider_value(item: &TranscriptItem) -> Result<Value, ApiError> {
    Ok(match item {
        TranscriptItem::Message {
            role,
            content,
            end_turn,
            ..
        } => json!({
            "type": "message",
            "role": role,
            "content": content,
            "end_turn": end_turn,
        }),
        TranscriptItem::Reasoning {
            summary,
            content,
            encrypted_content,
            ..
        } => json!({
            "type": "reasoning",
            "summary": summary,
            "content": content,
            "encrypted_content": encrypted_content,
        }),
        TranscriptItem::HostedActivity {
            activity_type,
            status,
            payload,
            ..
        } => json!({
            "type": if activity_type == "web_search" {
                "web_search_call"
            } else {
                "hosted_activity"
            },
            "status": status,
            "action": payload,
        }),
        TranscriptItem::ToolCall {
            call_id,
            tool_name,
            payload,
            ..
        } => match payload {
            ToolCallPayload::JsonArguments { arguments } => json!({
                "type": "function_call",
                "call_id": call_id,
                "name": tool_name,
                "arguments": arguments,
            }),
            ToolCallPayload::TextInput { input } => json!({
                "type": "custom_tool_call",
                "call_id": call_id,
                "name": tool_name,
                "input": input,
            }),
        },
        TranscriptItem::ToolResult {
            call_id, payload, ..
        } => match payload {
            ToolResultPayload::Structured { content, .. } => json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": content,
            }),
            ToolResultPayload::Text { output } => json!({
                "type": "custom_tool_call_output",
                "call_id": call_id,
                "output": output,
            }),
        },
        TranscriptItem::Unknown { raw } => raw.clone(),
    })
}

fn attach_item_ids(payload_json: &mut Value, original_items: &[TranscriptItem]) {
    let Some(input_value) = payload_json.get_mut("input") else {
        return;
    };
    let Value::Array(items) = input_value else {
        return;
    };

    for (value, item) in items.iter_mut().zip(original_items.iter()) {
        if let Some(id) = item.id() {
            if id.is_empty() {
                continue;
            }

            if let Some(obj) = value.as_object_mut() {
                obj.insert("id".to_string(), Value::String(id.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::RetryConfig;
    use crate::provider::WireApi;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    fn provider(name: &str, base_url: &str) -> Provider {
        Provider {
            name: name.to_string(),
            base_url: base_url.to_string(),
            query_params: None,
            wire: WireApi::Responses,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(50),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn azure_default_store_attaches_ids_and_headers() {
        let provider = provider("azure", "https://example.openai.azure.com/v1");
        let input = vec![
            TranscriptItem::Message {
                id: Some("m1".into()),
                role: "assistant".into(),
                content: Vec::new(),
                end_turn: None,
            },
            TranscriptItem::Message {
                id: None,
                role: "assistant".into(),
                content: Vec::new(),
                end_turn: None,
            },
        ];

        let request = ResponsesRequestBuilder::new("gpt-test", "inst", &input)
            .conversation(Some("conv-1".into()))
            .origin_tag(Some("review".into()))
            .build(&provider)
            .expect("request");

        assert_eq!(request.body.get("store"), Some(&Value::Bool(true)));

        let ids: Vec<Option<String>> = request
            .body
            .get("input")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .map(|item| item.get("id").and_then(|v| v.as_str().map(str::to_string)))
            .collect();
        assert_eq!(ids, vec![Some("m1".to_string()), None]);

        assert_eq!(
            request.headers.get("session_id"),
            Some(&HeaderValue::from_static("conv-1"))
        );
        assert_eq!(
            request.headers.get("x-openai-subagent"),
            Some(&HeaderValue::from_static("review"))
        );
    }
}
