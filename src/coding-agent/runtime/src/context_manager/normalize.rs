use std::collections::HashSet;

use codex_protocol::legacy_transcript::ConversationItem;
use codex_protocol::legacy_transcript::FunctionCallOutputPayload;

use crate::util::error_or_panic;
use tracing::info;

pub(crate) fn ensure_call_outputs_present(items: &mut Vec<ConversationItem>) {
    // Collect synthetic outputs to insert immediately after their calls.
    // Store the insertion position (index of call) alongside the item so
    // we can insert in reverse order and avoid index shifting.
    let mut missing_outputs_to_insert: Vec<(usize, ConversationItem)> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        match item {
            ConversationItem::FunctionCall { call_id, .. } => {
                let has_output = items.iter().any(|i| match i {
                    ConversationItem::FunctionCallOutput {
                        call_id: existing, ..
                    } => existing == call_id,
                    _ => false,
                });

                if !has_output {
                    info!("Function call output is missing for call id: {call_id}");
                    missing_outputs_to_insert.push((
                        idx,
                        ConversationItem::FunctionCallOutput {
                            call_id: call_id.clone(),
                            output: FunctionCallOutputPayload {
                                content: "aborted".to_string(),
                                ..Default::default()
                            },
                        },
                    ));
                }
            }
            ConversationItem::CustomToolCall { call_id, .. } => {
                let has_output = items.iter().any(|i| match i {
                    ConversationItem::CustomToolCallOutput {
                        call_id: existing, ..
                    } => existing == call_id,
                    _ => false,
                });

                if !has_output {
                    error_or_panic(format!(
                        "Custom tool call output is missing for call id: {call_id}"
                    ));
                    missing_outputs_to_insert.push((
                        idx,
                        ConversationItem::CustomToolCallOutput {
                            call_id: call_id.clone(),
                            output: "aborted".to_string(),
                        },
                    ));
                }
            }
            // LocalShellCall is represented in upstream streams by a FunctionCallOutput
            ConversationItem::LocalShellCall { call_id, .. } => {
                if let Some(call_id) = call_id.as_ref() {
                    let has_output = items.iter().any(|i| match i {
                        ConversationItem::FunctionCallOutput {
                            call_id: existing, ..
                        } => existing == call_id,
                        _ => false,
                    });

                    if !has_output {
                        error_or_panic(format!(
                            "Local shell call output is missing for call id: {call_id}"
                        ));
                        missing_outputs_to_insert.push((
                            idx,
                            ConversationItem::FunctionCallOutput {
                                call_id: call_id.clone(),
                                output: FunctionCallOutputPayload {
                                    content: "aborted".to_string(),
                                    ..Default::default()
                                },
                            },
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    // Insert synthetic outputs in reverse index order to avoid re-indexing.
    for (idx, output_item) in missing_outputs_to_insert.into_iter().rev() {
        items.insert(idx + 1, output_item);
    }
}

pub(crate) fn remove_orphan_outputs(items: &mut Vec<ConversationItem>) {
    let function_call_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| match i {
            ConversationItem::FunctionCall { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();

    let local_shell_call_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| match i {
            ConversationItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => Some(call_id.clone()),
            _ => None,
        })
        .collect();

    let custom_tool_call_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| match i {
            ConversationItem::CustomToolCall { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();

    items.retain(|item| match item {
        ConversationItem::FunctionCallOutput { call_id, .. } => {
            if call_id.is_empty() {
                return false;
            }
            let has_match =
                function_call_ids.contains(call_id) || local_shell_call_ids.contains(call_id);
            if !has_match {
                error_or_panic(format!(
                    "Orphan function call output for call id: {call_id}"
                ));
            }
            has_match
        }
        ConversationItem::CustomToolCallOutput { call_id, .. } => {
            if call_id.is_empty() {
                return false;
            }
            let has_match = custom_tool_call_ids.contains(call_id);
            if !has_match {
                error_or_panic(format!(
                    "Orphan custom tool call output for call id: {call_id}"
                ));
            }
            has_match
        }
        _ => true,
    });
}

pub(crate) fn remove_corresponding_for(items: &mut Vec<ConversationItem>, item: &ConversationItem) {
    match item {
        ConversationItem::FunctionCall { call_id, .. } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    ConversationItem::FunctionCallOutput {
                        call_id: existing, ..
                    } if existing == call_id
                )
            });
        }
        ConversationItem::FunctionCallOutput { call_id, .. } => {
            if let Some(pos) = items.iter().position(|i| {
                matches!(i, ConversationItem::FunctionCall { call_id: existing, .. } if existing == call_id)
            }) {
                items.remove(pos);
            } else if let Some(pos) = items.iter().position(|i| {
                matches!(i, ConversationItem::LocalShellCall { call_id: Some(existing), .. } if existing == call_id)
            }) {
                items.remove(pos);
            }
        }
        ConversationItem::CustomToolCall { call_id, .. } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    ConversationItem::CustomToolCallOutput {
                        call_id: existing, ..
                    } if existing == call_id
                )
            });
        }
        ConversationItem::CustomToolCallOutput { call_id, .. } => {
            remove_first_matching(
                items,
                |i| matches!(i, ConversationItem::CustomToolCall { call_id: existing, .. } if existing == call_id),
            );
        }
        ConversationItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    ConversationItem::FunctionCallOutput {
                        call_id: existing, ..
                    } if existing == call_id
                )
            });
        }
        _ => {}
    }
}

fn remove_first_matching<F>(items: &mut Vec<ConversationItem>, predicate: F)
where
    F: Fn(&ConversationItem) -> bool,
{
    if let Some(pos) = items.iter().position(predicate) {
        items.remove(pos);
    }
}
