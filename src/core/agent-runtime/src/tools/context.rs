use codex_llm::ToolCallPayload as LlmToolCallPayload;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ShellToolCallParams;

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
    LocalShell { params: ShellToolCallParams },
}

impl ToolPayload {
    pub fn from_llm(value: LlmToolCallPayload) -> Self {
        match value {
            LlmToolCallPayload::Function { arguments } => Self::Function { arguments },
            LlmToolCallPayload::Custom { input } => Self::Custom { input },
            LlmToolCallPayload::LocalShell { params } => Self::LocalShell { params },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutput {
    Function {
        content: String,
        content_items: Option<Vec<FunctionCallOutputContentItem>>,
        success: Option<bool>,
    },
}

impl ToolOutput {
    pub fn into_response(self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        match self {
            Self::Function {
                content,
                content_items,
                success,
            } => {
                if matches!(payload, ToolPayload::Custom { .. }) {
                    ResponseInputItem::CustomToolCallOutput {
                        call_id: call_id.to_string(),
                        output: content,
                    }
                } else {
                    ResponseInputItem::FunctionCallOutput {
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
