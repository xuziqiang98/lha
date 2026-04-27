use std::collections::HashSet;

use adam_llm::ToolCallPayload;
use adam_llm::ToolResultPayload;
use adam_protocol::models::TranscriptItem;

use crate::util::error_or_panic;
use tracing::info;

pub(crate) fn ensure_call_outputs_present(items: &mut Vec<TranscriptItem>) {
    let mut missing_outputs_to_insert: Vec<(usize, TranscriptItem)> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        if let TranscriptItem::ToolCall {
            call_id,
            tool_name,
            payload,
            ..
        } = item
        {
            let has_output = items.iter().any(
                |i| matches!(i, TranscriptItem::ToolResult { call_id: existing, .. } if existing == call_id),
            );

            if !has_output {
                let result = match payload {
                    ToolCallPayload::JsonArguments { .. } => {
                        info!("Tool result is missing for call id: {call_id}");
                        TranscriptItem::ToolResult {
                            call_id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            payload: ToolResultPayload::Structured {
                                content: "aborted".to_string(),
                                content_items: None,
                                success: None,
                            },
                        }
                    }
                    ToolCallPayload::TextInput { .. } => {
                        error_or_panic(format!(
                            "Custom tool call output is missing for call id: {call_id}"
                        ));
                        TranscriptItem::ToolResult {
                            call_id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            payload: ToolResultPayload::Text {
                                output: "aborted".to_string(),
                            },
                        }
                    }
                };
                missing_outputs_to_insert.push((idx, result));
            }
        }
    }

    for (idx, output_item) in missing_outputs_to_insert.into_iter().rev() {
        items.insert(idx + 1, output_item);
    }
}

pub(crate) fn remove_orphan_outputs(items: &mut Vec<TranscriptItem>) {
    let tool_call_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::ToolCall { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();

    items.retain(|item| match item {
        TranscriptItem::ToolResult { call_id, .. } => {
            if call_id.is_empty() {
                return false;
            }
            let has_match = tool_call_ids.contains(call_id);
            if !has_match {
                error_or_panic(format!("Orphan tool result for call id: {call_id}"));
            }
            has_match
        }
        _ => true,
    });
}

pub(crate) fn remove_corresponding_for(items: &mut Vec<TranscriptItem>, item: &TranscriptItem) {
    match item {
        TranscriptItem::ToolCall { call_id, .. } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    TranscriptItem::ToolResult {
                        call_id: existing, ..
                    } if existing == call_id
                )
            });
        }
        TranscriptItem::ToolResult { call_id, .. } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    TranscriptItem::ToolCall {
                        call_id: existing, ..
                    } if existing == call_id
                )
            });
        }
        _ => {}
    }
}

fn remove_first_matching<F>(items: &mut Vec<TranscriptItem>, predicate: F)
where
    F: Fn(&TranscriptItem) -> bool,
{
    if let Some(pos) = items.iter().position(predicate) {
        items.remove(pos);
    }
}
