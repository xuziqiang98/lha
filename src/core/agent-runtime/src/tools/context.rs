use codex_llm::ToolCallPayload as LlmToolCallPayload;
use codex_llm::ToolResultContentItem;
use codex_llm::ToolResultItem;
use codex_llm_types::ToolResultPayload;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    pub call_id: String,
    pub tool_name: String,
    pub payload: ToolPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolPayload {
    JsonArguments { arguments: String },
    TextInput { input: String },
}

impl ToolPayload {
    pub fn from_llm(value: LlmToolCallPayload) -> Self {
        match value {
            LlmToolCallPayload::JsonArguments { arguments } => Self::JsonArguments { arguments },
            LlmToolCallPayload::TextInput { input } => Self::TextInput { input },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutput {
    Function {
        content: String,
        content_items: Option<Vec<ToolResultContentItem>>,
        success: Option<bool>,
    },
}

impl ToolOutput {
    pub fn into_response(self, call_id: &str, payload: &ToolPayload) -> ToolResultItem {
        match self {
            Self::Function {
                content,
                content_items,
                success,
            } => {
                let payload = match payload {
                    ToolPayload::TextInput { .. } => ToolResultPayload::Text { output: content },
                    ToolPayload::JsonArguments { .. } => ToolResultPayload::Structured {
                        content,
                        content_items,
                        success,
                    },
                };

                ToolResultItem {
                    call_id: call_id.to_string(),
                    tool_name: String::new(),
                    payload,
                }
            }
        }
    }
}
