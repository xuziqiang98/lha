use codex_llm::ToolCallPayload as LlmToolCallPayload;
use codex_llm::ToolResultContentItem;
use codex_llm::ToolResultItem;
use codex_llm_types::FunctionCallOutputPayload;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    pub call_id: String,
    pub tool_name: String,
    pub payload: ToolPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolPayload {
    Function { arguments: String },
    Custom { input: String },
}

impl ToolPayload {
    pub fn from_llm(value: LlmToolCallPayload) -> Self {
        match value {
            LlmToolCallPayload::Function { arguments } => Self::Function { arguments },
            LlmToolCallPayload::Custom { input } => Self::Custom { input },
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
                if matches!(payload, ToolPayload::Custom { .. }) {
                    ToolResultItem::CustomToolCallOutput {
                        call_id: call_id.to_string(),
                        output: content,
                    }
                } else {
                    ToolResultItem::FunctionCallOutput {
                        call_id: call_id.to_string(),
                        output: FunctionCallOutputPayload {
                            content,
                            content_items,
                            success,
                        },
                    }
                }
            }
        }
    }
}
