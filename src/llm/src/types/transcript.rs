use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

pub const BASE_INSTRUCTIONS_DEFAULT: &str = include_str!("base_instructions_default.md");

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

pub type MessageRole = String;
pub type MessageContentItem = ContentItem;
pub type ReasoningSummaryItem = ReasoningItemReasoningSummary;
pub type ReasoningContentItem = ReasoningItemContent;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContentItem {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct MessageItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    pub role: MessageRole,
    pub content: Vec<MessageContentItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub end_turn: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct ReasoningItem {
    pub id: String,
    pub summary: Vec<ReasoningSummaryItem>,
    #[serde(default, skip_serializing_if = "should_serialize_reasoning_content")]
    #[ts(optional)]
    pub content: Option<Vec<ReasoningContentItem>>,
    pub encrypted_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct HostedActivityItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    pub activity_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallPayload {
    JsonArguments { arguments: String },
    TextInput { input: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct ToolCallItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    pub call_id: String,
    pub tool_name: String,
    pub payload: ToolCallPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultPayload {
    Structured {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        content_items: Option<Vec<ToolResultContentItem>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        success: Option<bool>,
    },
    Text {
        output: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct ToolResultItem {
    pub call_id: String,
    pub tool_name: String,
    pub payload: ToolResultPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct UnknownItem {
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptItem {
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<String>,
        role: MessageRole,
        content: Vec<MessageContentItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        end_turn: Option<bool>,
    },
    Reasoning {
        id: String,
        summary: Vec<ReasoningSummaryItem>,
        #[serde(default, skip_serializing_if = "should_serialize_reasoning_content")]
        #[ts(optional)]
        content: Option<Vec<ReasoningContentItem>>,
        encrypted_content: Option<String>,
    },
    HostedActivity {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<String>,
        activity_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        payload: Value,
    },
    ToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        id: Option<String>,
        call_id: String,
        tool_name: String,
        payload: ToolCallPayload,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        payload: ToolResultPayload,
    },
    Unknown {
        raw: Value,
    },
}

impl TranscriptItem {
    pub fn id(&self) -> Option<&String> {
        match self {
            Self::Message { id, .. } => id.as_ref(),
            Self::Reasoning { id, .. } => Some(id),
            Self::HostedActivity { id, .. } => id.as_ref(),
            Self::ToolCall { id, .. } => id.as_ref(),
            Self::ToolResult { .. } | Self::Unknown { .. } => None,
        }
    }
}

impl From<MessageItem> for TranscriptItem {
    fn from(value: MessageItem) -> Self {
        Self::Message {
            id: value.id,
            role: value.role,
            content: value.content,
            end_turn: value.end_turn,
        }
    }
}

impl From<ReasoningItem> for TranscriptItem {
    fn from(value: ReasoningItem) -> Self {
        Self::Reasoning {
            id: value.id,
            summary: value.summary,
            content: value.content,
            encrypted_content: value.encrypted_content,
        }
    }
}

impl From<HostedActivityItem> for TranscriptItem {
    fn from(value: HostedActivityItem) -> Self {
        Self::HostedActivity {
            id: value.id,
            activity_type: value.activity_type,
            status: value.status,
            payload: value.payload,
        }
    }
}

impl From<ToolCallItem> for TranscriptItem {
    fn from(value: ToolCallItem) -> Self {
        Self::ToolCall {
            id: value.id,
            call_id: value.call_id,
            tool_name: value.tool_name,
            payload: value.payload,
        }
    }
}

impl From<ToolResultItem> for TranscriptItem {
    fn from(value: ToolResultItem) -> Self {
        Self::ToolResult {
            call_id: value.call_id,
            tool_name: value.tool_name,
            payload: value.payload,
        }
    }
}

impl From<UnknownItem> for TranscriptItem {
    fn from(value: UnknownItem) -> Self {
        Self::Unknown { raw: value.raw }
    }
}

impl ToolCallItem {
    pub fn from_transcript_item(item: TranscriptItem) -> Option<Self> {
        match item {
            TranscriptItem::ToolCall {
                id,
                call_id,
                tool_name,
                payload,
            } => Some(Self {
                id,
                call_id,
                tool_name,
                payload,
            }),
            _ => None,
        }
    }

    pub fn to_transcript_item(&self) -> TranscriptItem {
        self.clone().into()
    }
}

impl ToolResultItem {
    pub fn to_transcript_item(&self) -> TranscriptItem {
        self.clone().into()
    }
}
