use std::collections::HashMap;

use codex_git::GhostCommit;
use mcp_types::CallToolResult;
use mcp_types::ContentBlock;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::ser::Serializer;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseInputItem {
    Message {
        role: String,
        content: Vec<ContentItem>,
    },
    FunctionCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
    },
    McpToolCallOutput {
        call_id: String,
        result: Result<CallToolResult, String>,
    },
    CustomToolCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItem {
    Message {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        end_turn: Option<bool>,
    },
    Reasoning {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: String,
        summary: Vec<ReasoningItemReasoningSummary>,
        #[serde(default, skip_serializing_if = "should_serialize_reasoning_content")]
        #[ts(optional)]
        content: Option<Vec<ReasoningItemContent>>,
        encrypted_content: Option<String>,
    },
    LocalShellCall {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        call_id: Option<String>,
        status: LocalShellStatus,
        action: LocalShellAction,
    },
    FunctionCall {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        name: String,
        arguments: String,
        call_id: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
    },
    CustomToolCall {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        call_id: String,
        name: String,
        input: String,
    },
    CustomToolCallOutput {
        call_id: String,
        output: String,
    },
    WebSearchCall {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        action: Option<WebSearchAction>,
    },
    GhostSnapshot {
        ghost_commit: GhostCommit,
    },
    #[serde(alias = "compaction_summary")]
    Compaction {
        encrypted_content: String,
    },
    #[serde(other)]
    Other,
}

pub const BASE_INSTRUCTIONS_DEFAULT: &str =
    include_str!("../../../core/protocol/src/prompts/base_instructions/default.md");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename = "base_instructions", rename_all = "snake_case")]
pub struct BaseInstructions {
    pub text: String,
}

impl Default for BaseInstructions {
    fn default() -> Self {
        Self {
            text: BASE_INSTRUCTIONS_DEFAULT.to_string(),
        }
    }
}

fn should_serialize_reasoning_content(content: &Option<Vec<ReasoningItemContent>>) -> bool {
    match content {
        Some(content) => !content
            .iter()
            .any(|c| matches!(c, ReasoningItemContent::ReasoningText { .. })),
        None => false,
    }
}

impl From<ResponseInputItem> for ConversationItem {
    fn from(item: ResponseInputItem) -> Self {
        match item {
            ResponseInputItem::Message { role, content } => Self::Message {
                role,
                content,
                id: None,
                end_turn: None,
            },
            ResponseInputItem::FunctionCallOutput { call_id, output } => {
                Self::FunctionCallOutput { call_id, output }
            }
            ResponseInputItem::McpToolCallOutput { call_id, result } => {
                let output = match result {
                    Ok(result) => FunctionCallOutputPayload::from(&result),
                    Err(tool_call_err) => FunctionCallOutputPayload {
                        content: format!("err: {tool_call_err:?}"),
                        success: Some(false),
                        ..Default::default()
                    },
                };
                Self::FunctionCallOutput { call_id, output }
            }
            ResponseInputItem::CustomToolCallOutput { call_id, output } => {
                Self::CustomToolCallOutput { call_id, output }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum LocalShellStatus {
    Completed,
    InProgress,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalShellAction {
    Exec(LocalShellExecAction),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct LocalShellExecAction {
    pub command: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub working_directory: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSearchAction {
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        queries: Option<Vec<String>>,
    },
    OpenPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
    },
    FindInPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pattern: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningItemReasoningSummary {
    SummaryText { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningItemContent {
    ReasoningText { text: String },
    Text { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionCallOutputContentItem {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(Debug, Default, Clone, PartialEq, JsonSchema, TS)]
pub struct FunctionCallOutputPayload {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_items: Option<Vec<FunctionCallOutputContentItem>>,
    pub success: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FunctionCallOutputPayloadSerde {
    Text(String),
    Items(Vec<FunctionCallOutputContentItem>),
}

impl Serialize for FunctionCallOutputPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(items) = &self.content_items {
            items.serialize(serializer)
        } else {
            serializer.serialize_str(&self.content)
        }
    }
}

impl<'de> Deserialize<'de> for FunctionCallOutputPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match FunctionCallOutputPayloadSerde::deserialize(deserializer)? {
            FunctionCallOutputPayloadSerde::Text(content) => Ok(FunctionCallOutputPayload {
                content,
                ..Default::default()
            }),
            FunctionCallOutputPayloadSerde::Items(items) => {
                let content = serde_json::to_string(&items).map_err(serde::de::Error::custom)?;
                Ok(FunctionCallOutputPayload {
                    content,
                    content_items: Some(items),
                    success: None,
                })
            }
        }
    }
}

impl From<&CallToolResult> for FunctionCallOutputPayload {
    fn from(call_tool_result: &CallToolResult) -> Self {
        let CallToolResult {
            content,
            structured_content,
            is_error,
        } = call_tool_result;

        let is_success = is_error != &Some(true);

        if let Some(structured_content) = structured_content
            && !structured_content.is_null()
        {
            return match serde_json::to_string(structured_content) {
                Ok(serialized_structured_content) => FunctionCallOutputPayload {
                    content: serialized_structured_content,
                    success: Some(is_success),
                    ..Default::default()
                },
                Err(err) => FunctionCallOutputPayload {
                    content: err.to_string(),
                    success: Some(false),
                    ..Default::default()
                },
            };
        }

        let serialized_content = match serde_json::to_string(content) {
            Ok(serialized_content) => serialized_content,
            Err(err) => {
                return FunctionCallOutputPayload {
                    content: err.to_string(),
                    success: Some(false),
                    ..Default::default()
                };
            }
        };

        let content_items = convert_content_blocks_to_items(content);

        FunctionCallOutputPayload {
            content: serialized_content,
            content_items,
            success: Some(is_success),
        }
    }
}

fn convert_content_blocks_to_items(
    blocks: &[ContentBlock],
) -> Option<Vec<FunctionCallOutputContentItem>> {
    let mut saw_image = false;
    let mut items = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            ContentBlock::TextContent(text) => {
                items.push(FunctionCallOutputContentItem::InputText {
                    text: text.text.clone(),
                });
            }
            ContentBlock::ImageContent(image) => {
                saw_image = true;
                let image_url = if image.data.starts_with("data:") {
                    image.data.clone()
                } else {
                    format!("data:{};base64,{}", image.mime_type, image.data)
                };
                items.push(FunctionCallOutputContentItem::InputImage { image_url });
            }
            _ => return None,
        }
    }

    if saw_image { Some(items) } else { None }
}

impl std::fmt::Display for FunctionCallOutputPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.content)
    }
}

impl std::ops::Deref for FunctionCallOutputPayload {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

pub type TranscriptItem = ConversationItem;
pub type ToolResultContentItem = FunctionCallOutputContentItem;
pub type ToolResultItem = ResponseInputItem;
