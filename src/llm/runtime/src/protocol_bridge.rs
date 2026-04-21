use crate::ToolCallPayload;
use crate::ToolCallRequest;
use crate::ToolResultItem;
use crate::TranscriptItem;
use codex_llm_types::ConversationItem;
use codex_llm_types::FunctionCallOutputPayload;
use codex_llm_types::ResponseInputItem;

pub fn tool_call_to_transcript_item(call: &ToolCallRequest) -> Option<TranscriptItem> {
    match &call.payload {
        ToolCallPayload::Function { arguments } => Some(ConversationItem::FunctionCall {
            id: Some(call.call_id.clone()),
            name: call.tool_name.clone(),
            arguments: arguments.clone(),
            call_id: call.call_id.clone(),
        }),
        ToolCallPayload::Custom { input } => Some(ConversationItem::CustomToolCall {
            id: Some(call.call_id.clone()),
            status: None,
            call_id: call.call_id.clone(),
            name: call.tool_name.clone(),
            input: input.clone(),
        }),
    }
}

pub fn tool_result_to_transcript_item(result: &ToolResultItem) -> Option<TranscriptItem> {
    match result {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            Some(ConversationItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: output.clone(),
            })
        }
        ResponseInputItem::CustomToolCallOutput { call_id, output } => {
            Some(ConversationItem::CustomToolCallOutput {
                call_id: call_id.clone(),
                output: output.clone(),
            })
        }
        ResponseInputItem::Message { role, content } => Some(ConversationItem::Message {
            id: None,
            role: role.clone(),
            content: content.clone(),
            end_turn: None,
        }),
        ResponseInputItem::McpToolCallOutput { call_id, result } => {
            let output = match result {
                Ok(call_tool_result) => FunctionCallOutputPayload::from(call_tool_result),
                Err(err) => FunctionCallOutputPayload {
                    content: err.clone(),
                    success: Some(false),
                    ..Default::default()
                },
            };
            Some(ConversationItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output,
            })
        }
    }
}
