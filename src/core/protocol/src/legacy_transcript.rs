use std::collections::HashMap;

use crate::models::DeveloperInstructions;
use crate::models::WebSearchAction;
use codex_git::GhostCommit;
use codex_llm_types::ContentItem;
use codex_llm_types::ReasoningItemContent;
use codex_llm_types::ReasoningItemReasoningSummary;
use codex_llm_types::ToolCallPayload;
use codex_llm_types::ToolResultPayload;
use codex_llm_types::TranscriptItem as SemanticTranscriptItem;
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
pub enum TranscriptItem {
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

pub type ConversationItem = TranscriptItem;

fn should_serialize_reasoning_content(content: &Option<Vec<ReasoningItemContent>>) -> bool {
    match content {
        Some(content) => !content
            .iter()
            .any(|c| matches!(c, ReasoningItemContent::ReasoningText { .. })),
        None => false,
    }
}

impl From<ResponseInputItem> for TranscriptItem {
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

impl ResponseInputItem {
    pub fn to_tool_result_item(&self) -> Option<codex_llm_types::ToolResultItem> {
        match self {
            Self::Message { .. } => None,
            Self::FunctionCallOutput { call_id, output } => Some(codex_llm_types::ToolResultItem {
                call_id: call_id.clone(),
                tool_name: String::new(),
                payload: ToolResultPayload::Structured {
                    content: output.content.clone(),
                    content_items: output.content_items.clone().map(|items| {
                        items
                            .into_iter()
                            .map(|item| match item {
                                FunctionCallOutputContentItem::InputText { text } => {
                                    codex_llm_types::ToolResultContentItem::InputText { text }
                                }
                                FunctionCallOutputContentItem::InputImage { image_url } => {
                                    codex_llm_types::ToolResultContentItem::InputImage { image_url }
                                }
                            })
                            .collect()
                    }),
                    success: output.success,
                },
            }),
            Self::McpToolCallOutput { call_id, result } => {
                let payload = match result {
                    Ok(call_tool_result) => FunctionCallOutputPayload::from(call_tool_result),
                    Err(err) => FunctionCallOutputPayload {
                        content: err.clone(),
                        success: Some(false),
                        ..Default::default()
                    },
                };
                Some(codex_llm_types::ToolResultItem {
                    call_id: call_id.clone(),
                    tool_name: "mcp".to_string(),
                    payload: ToolResultPayload::Structured {
                        content: payload.content,
                        content_items: payload.content_items.map(|items| {
                            items
                                .into_iter()
                                .map(|item| match item {
                                    FunctionCallOutputContentItem::InputText { text } => {
                                        codex_llm_types::ToolResultContentItem::InputText { text }
                                    }
                                    FunctionCallOutputContentItem::InputImage { image_url } => {
                                        codex_llm_types::ToolResultContentItem::InputImage {
                                            image_url,
                                        }
                                    }
                                })
                                .collect()
                        }),
                        success: payload.success,
                    },
                })
            }
            Self::CustomToolCallOutput { call_id, output } => {
                Some(codex_llm_types::ToolResultItem {
                    call_id: call_id.clone(),
                    tool_name: String::new(),
                    payload: ToolResultPayload::Text {
                        output: output.clone(),
                    },
                })
            }
        }
    }

    pub fn to_transcript_item(&self) -> SemanticTranscriptItem {
        match self {
            Self::Message { role, content } => SemanticTranscriptItem::Message {
                id: None,
                role: role.clone(),
                content: content.clone(),
                end_turn: None,
            },
            _ => match self.to_tool_result_item() {
                Some(item) => item.into(),
                None => unreachable!("non-message response input should produce tool result"),
            },
        }
    }
}

impl From<TranscriptItem> for SemanticTranscriptItem {
    fn from(value: TranscriptItem) -> Self {
        match value {
            ConversationItem::Message {
                id,
                role,
                content,
                end_turn,
            } => Self::Message {
                id,
                role,
                content,
                end_turn,
            },
            ConversationItem::Reasoning {
                id,
                summary,
                content,
                encrypted_content,
            } => Self::Reasoning {
                id,
                summary,
                content,
                encrypted_content,
            },
            ConversationItem::WebSearchCall { id, status, action } => Self::HostedActivity {
                id,
                activity_type: "web_search".to_string(),
                status,
                payload: action
                    .and_then(|value| serde_json::to_value(value).ok())
                    .unwrap_or(serde_json::Value::Null),
            },
            ConversationItem::FunctionCall {
                id,
                name,
                arguments,
                call_id,
            } => Self::ToolCall {
                id,
                call_id,
                tool_name: name,
                payload: ToolCallPayload::JsonArguments { arguments },
            },
            ConversationItem::CustomToolCall {
                id,
                call_id,
                name,
                input,
                ..
            } => Self::ToolCall {
                id,
                call_id,
                tool_name: name,
                payload: ToolCallPayload::TextInput { input },
            },
            ConversationItem::LocalShellCall {
                id,
                call_id,
                action,
                ..
            } => {
                let call_id = call_id.or_else(|| id.clone()).unwrap_or_default();
                let LocalShellAction::Exec(exec) = action;
                let arguments = serde_json::to_string(&serde_json::json!({
                    "command": exec.command,
                    "workdir": exec.working_directory,
                    "timeout_ms": exec.timeout_ms,
                }))
                .unwrap_or_default();
                Self::ToolCall {
                    id,
                    call_id,
                    tool_name: "local_shell".to_string(),
                    payload: ToolCallPayload::JsonArguments { arguments },
                }
            }
            ConversationItem::FunctionCallOutput { call_id, output } => Self::ToolResult {
                call_id,
                tool_name: String::new(),
                payload: ToolResultPayload::Structured {
                    content: output.content,
                    content_items: output.content_items.map(|items| {
                        items
                            .into_iter()
                            .map(|item| match item {
                                FunctionCallOutputContentItem::InputText { text } => {
                                    codex_llm_types::ToolResultContentItem::InputText { text }
                                }
                                FunctionCallOutputContentItem::InputImage { image_url } => {
                                    codex_llm_types::ToolResultContentItem::InputImage { image_url }
                                }
                            })
                            .collect()
                    }),
                    success: output.success,
                },
            },
            ConversationItem::CustomToolCallOutput { call_id, output } => Self::ToolResult {
                call_id,
                tool_name: String::new(),
                payload: ToolResultPayload::Text { output },
            },
            ConversationItem::GhostSnapshot { ghost_commit } => Self::Unknown {
                raw: serde_json::to_value(ConversationItem::GhostSnapshot { ghost_commit })
                    .unwrap_or(serde_json::Value::Null),
            },
            ConversationItem::Compaction { encrypted_content } => Self::Unknown {
                raw: serde_json::to_value(ConversationItem::Compaction { encrypted_content })
                    .unwrap_or(serde_json::Value::Null),
            },
            ConversationItem::Other => Self::Unknown {
                raw: serde_json::Value::Null,
            },
        }
    }
}

impl From<DeveloperInstructions> for TranscriptItem {
    fn from(value: DeveloperInstructions) -> Self {
        SemanticTranscriptItem::from(value).into()
    }
}

impl From<SemanticTranscriptItem> for TranscriptItem {
    fn from(value: SemanticTranscriptItem) -> Self {
        match value {
            SemanticTranscriptItem::Message {
                id,
                role,
                content,
                end_turn,
            } => Self::Message {
                id,
                role,
                content,
                end_turn,
            },
            SemanticTranscriptItem::Reasoning {
                id,
                summary,
                content,
                encrypted_content,
            } => Self::Reasoning {
                id,
                summary,
                content,
                encrypted_content,
            },
            SemanticTranscriptItem::HostedActivity {
                id,
                activity_type,
                status,
                payload,
            } => {
                if activity_type == "web_search" {
                    Self::WebSearchCall {
                        id,
                        status,
                        action: serde_json::from_value(payload).ok(),
                    }
                } else {
                    Self::Other
                }
            }
            SemanticTranscriptItem::ToolCall {
                id,
                call_id,
                tool_name,
                payload,
            } => match payload {
                ToolCallPayload::JsonArguments { arguments } => Self::FunctionCall {
                    id,
                    name: tool_name,
                    arguments,
                    call_id,
                },
                ToolCallPayload::TextInput { input } => Self::CustomToolCall {
                    id,
                    status: None,
                    call_id,
                    name: tool_name,
                    input,
                },
            },
            SemanticTranscriptItem::ToolResult {
                call_id,
                tool_name: _,
                payload,
            } => match payload {
                ToolResultPayload::Structured {
                    content,
                    content_items,
                    success,
                } => Self::FunctionCallOutput {
                    call_id,
                    output: FunctionCallOutputPayload {
                        content,
                        content_items: content_items.map(|items| {
                            items
                                .into_iter()
                                .map(|item| match item {
                                    codex_llm_types::ToolResultContentItem::InputText { text } => {
                                        FunctionCallOutputContentItem::InputText { text }
                                    }
                                    codex_llm_types::ToolResultContentItem::InputImage {
                                        image_url,
                                    } => FunctionCallOutputContentItem::InputImage { image_url },
                                })
                                .collect()
                        }),
                        success,
                    },
                },
                ToolResultPayload::Text { output } => {
                    Self::CustomToolCallOutput { call_id, output }
                }
            },
            SemanticTranscriptItem::Unknown { raw } => {
                serde_json::from_value(raw).unwrap_or(Self::Other)
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
            FunctionCallOutputPayloadSerde::Text(content) => Ok(Self {
                content,
                ..Default::default()
            }),
            FunctionCallOutputPayloadSerde::Items(items) => {
                let content = serde_json::to_string(&items).map_err(serde::de::Error::custom)?;
                Ok(Self {
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
                Ok(serialized_structured_content) => Self {
                    content: serialized_structured_content,
                    success: Some(is_success),
                    ..Default::default()
                },
                Err(err) => Self {
                    content: err.to_string(),
                    success: Some(false),
                    ..Default::default()
                },
            };
        }

        let serialized_content = match serde_json::to_string(content) {
            Ok(serialized_content) => serialized_content,
            Err(err) => {
                return Self {
                    content: err.to_string(),
                    success: Some(false),
                    ..Default::default()
                };
            }
        };

        let content_items = convert_content_blocks_to_items(content);

        Self {
            content: serialized_content,
            content_items,
            success: Some(is_success),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CompatTranscriptItem {
    Semantic(SemanticTranscriptItem),
    Legacy(TranscriptItem),
}

impl From<CompatTranscriptItem> for SemanticTranscriptItem {
    fn from(value: CompatTranscriptItem) -> Self {
        match value {
            CompatTranscriptItem::Semantic(item) => item,
            CompatTranscriptItem::Legacy(item) => item.into(),
        }
    }
}

pub(crate) fn deserialize_transcript_item_compat<'de, D>(
    deserializer: D,
) -> Result<SemanticTranscriptItem, D::Error>
where
    D: Deserializer<'de>,
{
    CompatTranscriptItem::deserialize(deserializer).map(Into::into)
}

pub(crate) fn deserialize_optional_transcript_history_compat<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<SemanticTranscriptItem>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<Vec<CompatTranscriptItem>>::deserialize(deserializer)
        .map(|items| items.map(|items| items.into_iter().map(Into::into).collect()))
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
