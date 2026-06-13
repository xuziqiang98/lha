use lha_llm::ToolResultPayload;
use lha_llm::TranscriptItem;

use crate::product::agent::input_slimming::INPUT_RETRIEVE_TOOL_NAME;
use crate::product::agent::input_slimming::INPUT_SLIMMING_MARKER_PREFIX;
use crate::product::agent::input_slimming::InputSlimmingSkip;
use crate::product::agent::input_slimming::InputSlimmingSkipReason;
use crate::product::agent::truncate::approx_token_count;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) index: usize,
    pub(super) tool_name: String,
    pub(super) text: String,
    pub(super) success: Option<bool>,
    pub(super) original_tokens: usize,
}

impl Candidate {
    pub(super) fn skip(&self, reason: InputSlimmingSkipReason) -> InputSlimmingSkip {
        InputSlimmingSkip {
            reason,
            tool_name: Some(self.tool_name.clone()),
        }
    }
}

pub(super) fn latest_user_message_index(items: &[TranscriptItem]) -> Option<usize> {
    items.iter().rposition(|item| match item {
        TranscriptItem::Message { role, .. } => role == "user",
        TranscriptItem::Reasoning { .. }
        | TranscriptItem::HostedActivity { .. }
        | TranscriptItem::ToolCall { .. }
        | TranscriptItem::ToolResult { .. }
        | TranscriptItem::Unknown { .. } => false,
    })
}

pub(super) fn candidate_from_item(
    index: usize,
    item: &TranscriptItem,
    min_candidate_tokens: usize,
) -> Result<Candidate, Option<InputSlimmingSkip>> {
    let TranscriptItem::ToolResult {
        tool_name, payload, ..
    } = item
    else {
        return Err(None);
    };

    if tool_name == INPUT_RETRIEVE_TOOL_NAME {
        return Err(Some(InputSlimmingSkip {
            reason: InputSlimmingSkipReason::AlreadySlimmed,
            tool_name: Some(tool_name.clone()),
        }));
    }

    let (text, success) = match payload {
        ToolResultPayload::Text { output } => (output, None),
        ToolResultPayload::Structured {
            content,
            content_items: None,
            success,
        } => (content, *success),
        ToolResultPayload::Structured {
            content_items: Some(_),
            ..
        } => {
            return Err(Some(InputSlimmingSkip {
                reason: InputSlimmingSkipReason::StructuredContentItems,
                tool_name: Some(tool_name.clone()),
            }));
        }
    };

    if text.contains(INPUT_SLIMMING_MARKER_PREFIX) {
        return Err(Some(InputSlimmingSkip {
            reason: InputSlimmingSkipReason::AlreadySlimmed,
            tool_name: Some(tool_name.clone()),
        }));
    }

    let original_tokens = approx_token_count(text);
    if original_tokens < min_candidate_tokens {
        return Err(Some(InputSlimmingSkip {
            reason: InputSlimmingSkipReason::BelowSizeFloor,
            tool_name: Some(tool_name.clone()),
        }));
    }

    Ok(Candidate {
        index,
        tool_name: tool_name.clone(),
        text: text.clone(),
        success,
        original_tokens,
    })
}

pub(super) fn skip_for_current_turn_item(item: &TranscriptItem) -> Option<InputSlimmingSkip> {
    match item {
        TranscriptItem::ToolResult { tool_name, .. } => Some(InputSlimmingSkip {
            reason: InputSlimmingSkipReason::CurrentUserTurn,
            tool_name: Some(tool_name.clone()),
        }),
        TranscriptItem::Message { .. }
        | TranscriptItem::Reasoning { .. }
        | TranscriptItem::HostedActivity { .. }
        | TranscriptItem::ToolCall { .. }
        | TranscriptItem::Unknown { .. } => None,
    }
}

pub(super) fn skip_for_protected_item(item: &TranscriptItem) -> Option<InputSlimmingSkip> {
    match item {
        TranscriptItem::Message { role, .. } if role == "assistant" => Some(InputSlimmingSkip {
            reason: InputSlimmingSkipReason::RecentAssistant,
            tool_name: None,
        }),
        TranscriptItem::Message { .. }
        | TranscriptItem::Reasoning { .. }
        | TranscriptItem::HostedActivity { .. } => Some(InputSlimmingSkip {
            reason: InputSlimmingSkipReason::ProtectedRole,
            tool_name: None,
        }),
        TranscriptItem::ToolCall { .. } | TranscriptItem::Unknown { .. } => {
            Some(InputSlimmingSkip {
                reason: InputSlimmingSkipReason::UnsupportedItem,
                tool_name: None,
            })
        }
        TranscriptItem::ToolResult { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lha_llm::ContentItem;
    use lha_llm::ToolResultContentItem;
    use pretty_assertions::assert_eq;

    fn user(text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }

    fn assistant(text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }

    fn tool_text(tool_name: &str, output: &str) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: "call".to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolResultPayload::Text {
                output: output.to_string(),
            },
        }
    }

    #[test]
    fn latest_user_message_index_ignores_non_user_items() {
        let items = vec![user("one"), assistant("assistant"), user("two")];
        assert_eq!(latest_user_message_index(&items), Some(2));
    }

    #[test]
    fn selects_old_text_tool_result() {
        let text = "x".repeat(8_000);
        let item = tool_text("shell", &text);

        let candidate = candidate_from_item(3, &item, 1).expect("candidate");

        assert_eq!(
            candidate,
            Candidate {
                index: 3,
                tool_name: "shell".to_string(),
                text,
                success: None,
                original_tokens: 2_000,
            }
        );
    }

    #[test]
    fn skips_structured_content_items() {
        let item = TranscriptItem::ToolResult {
            call_id: "call".to_string(),
            tool_name: "mcp".to_string(),
            payload: ToolResultPayload::Structured {
                content: "long".repeat(3000),
                content_items: Some(vec![ToolResultContentItem::InputText {
                    text: "visible".to_string(),
                }]),
                success: Some(true),
            },
        };

        assert_eq!(
            candidate_from_item(0, &item, 1),
            Err(Some(InputSlimmingSkip {
                reason: InputSlimmingSkipReason::StructuredContentItems,
                tool_name: Some("mcp".to_string()),
            }))
        );
    }

    #[test]
    fn skips_existing_marker() {
        let item = tool_text("shell", "before <<lha-input:abcdef>> after");

        assert_eq!(
            candidate_from_item(0, &item, 1),
            Err(Some(InputSlimmingSkip {
                reason: InputSlimmingSkipReason::AlreadySlimmed,
                tool_name: Some("shell".to_string()),
            }))
        );
    }

    #[test]
    fn protected_messages_are_reported_with_skip_reason() {
        assert_eq!(
            skip_for_protected_item(&user("old")),
            Some(InputSlimmingSkip {
                reason: InputSlimmingSkipReason::ProtectedRole,
                tool_name: None,
            })
        );
        assert_eq!(
            skip_for_protected_item(&assistant("old")),
            Some(InputSlimmingSkip {
                reason: InputSlimmingSkipReason::RecentAssistant,
                tool_name: None,
            })
        );
    }

    #[test]
    fn current_turn_tool_results_are_reported_with_skip_reason() {
        assert_eq!(
            skip_for_current_turn_item(&tool_text("shell", "later")),
            Some(InputSlimmingSkip {
                reason: InputSlimmingSkipReason::CurrentUserTurn,
                tool_name: Some("shell".to_string()),
            })
        );
    }
}
