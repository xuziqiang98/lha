use lha_llm::ToolResultContentItem;
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
    pub(super) target: CandidateTarget,
    pub(super) zone: CandidateZone,
    pub(super) call_id: String,
    pub(super) tool_name: String,
    pub(super) text: String,
    pub(super) success: Option<bool>,
    pub(super) original_tokens_approx: usize,
}

impl Candidate {
    pub(super) fn skip(&self, reason: InputSlimmingSkipReason) -> InputSlimmingSkip {
        InputSlimmingSkip {
            reason,
            tool_name: Some(self.tool_name.clone()),
        }
    }

    pub(super) fn reject(
        &self,
        reason: InputSlimmingSkipReason,
    ) -> crate::product::agent::input_slimming::RejectedSlimming {
        crate::product::agent::input_slimming::RejectedSlimming {
            skip: self.skip(reason),
            gate_method: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CandidateTarget {
    TextToolOutput,
    StructuredContent,
    StructuredContentItem { item_index: usize },
}

impl CandidateTarget {
    pub(super) fn stable_key(self) -> String {
        match self {
            Self::TextToolOutput => "text_tool_output".to_string(),
            Self::StructuredContent => "structured_content".to_string(),
            Self::StructuredContentItem { item_index } => {
                format!("structured_content_item:{item_index}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CandidateZone {
    HistoricalToolOutput,
    LiveToolOutput,
}

impl CandidateZone {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HistoricalToolOutput => "historical_tool_output",
            Self::LiveToolOutput => "live_tool_output",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct CandidateCollection {
    pub(super) candidates: Vec<Candidate>,
    pub(super) skips: Vec<InputSlimmingSkip>,
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

pub(super) fn candidates_from_item(
    index: usize,
    item: &TranscriptItem,
    zone: CandidateZone,
    min_candidate_tokens: usize,
) -> CandidateCollection {
    let TranscriptItem::ToolResult {
        call_id,
        tool_name,
        payload,
    } = item
    else {
        return CandidateCollection::default();
    };

    if tool_name == INPUT_RETRIEVE_TOOL_NAME {
        return CandidateCollection {
            candidates: Vec::new(),
            skips: vec![InputSlimmingSkip {
                reason: InputSlimmingSkipReason::AlreadySlimmed,
                tool_name: Some(tool_name.clone()),
            }],
        };
    }

    match payload {
        ToolResultPayload::Text { output } => single_text_candidate(
            index,
            CandidateTarget::TextToolOutput,
            CandidateSource {
                zone,
                call_id,
                tool_name,
                min_candidate_tokens,
            },
            output,
            None,
        ),
        ToolResultPayload::Structured {
            content,
            content_items: None,
            success,
        } => single_text_candidate(
            index,
            CandidateTarget::StructuredContent,
            CandidateSource {
                zone,
                call_id,
                tool_name,
                min_candidate_tokens,
            },
            content,
            *success,
        ),
        ToolResultPayload::Structured {
            content_items: Some(items),
            success,
            ..
        } => content_item_candidates(
            index,
            zone,
            call_id,
            tool_name,
            items,
            *success,
            min_candidate_tokens,
        ),
    }
}

#[derive(Clone, Copy)]
struct CandidateSource<'a> {
    zone: CandidateZone,
    call_id: &'a str,
    tool_name: &'a str,
    min_candidate_tokens: usize,
}

fn single_text_candidate(
    index: usize,
    target: CandidateTarget,
    source: CandidateSource<'_>,
    text: &str,
    success: Option<bool>,
) -> CandidateCollection {
    if text_contains_protected_marker(text) {
        return CandidateCollection {
            candidates: Vec::new(),
            skips: vec![InputSlimmingSkip {
                reason: InputSlimmingSkipReason::AlreadySlimmed,
                tool_name: Some(source.tool_name.to_string()),
            }],
        };
    }

    let original_tokens_approx = approx_token_count(text);
    if original_tokens_approx < source.min_candidate_tokens {
        return CandidateCollection {
            candidates: Vec::new(),
            skips: vec![InputSlimmingSkip {
                reason: InputSlimmingSkipReason::BelowSizeFloor,
                tool_name: Some(source.tool_name.to_string()),
            }],
        };
    }

    CandidateCollection {
        candidates: vec![Candidate {
            index,
            target,
            zone: source.zone,
            call_id: source.call_id.to_string(),
            tool_name: source.tool_name.to_string(),
            text: text.to_string(),
            success,
            original_tokens_approx,
        }],
        skips: Vec::new(),
    }
}

fn text_contains_protected_marker(text: &str) -> bool {
    text.contains(INPUT_SLIMMING_MARKER_PREFIX)
        || text.starts_with(
            "Another language model started to solve this problem and produced a summary",
        )
        || text.starts_with("A proposed plan from before compaction is preserved below.")
        || text.starts_with(
            "Runtime note: the active programmer goal references a user-provided proposed plan",
        )
        || text.starts_with("The active programmer goal references a proposed plan stored at:")
        || text.starts_with("<skill>\n")
        || text.starts_with("<skill source=\"compact_backfill\">\n")
        || text.starts_with("# AGENTS.md instructions for ")
}

fn content_item_candidates(
    index: usize,
    zone: CandidateZone,
    call_id: &str,
    tool_name: &str,
    items: &[ToolResultContentItem],
    success: Option<bool>,
    min_candidate_tokens: usize,
) -> CandidateCollection {
    let mut collection = CandidateCollection::default();
    let mut saw_text = false;

    for (item_index, item) in items.iter().enumerate() {
        let ToolResultContentItem::InputText { text } = item else {
            continue;
        };
        saw_text = true;
        let mut item_collection = single_text_candidate(
            index,
            CandidateTarget::StructuredContentItem { item_index },
            CandidateSource {
                zone,
                call_id,
                tool_name,
                min_candidate_tokens,
            },
            text,
            success,
        );
        collection
            .candidates
            .append(&mut item_collection.candidates);
        collection.skips.append(&mut item_collection.skips);
    }

    if !saw_text {
        collection.skips.push(InputSlimmingSkip {
            reason: InputSlimmingSkipReason::StructuredContentItems,
            tool_name: Some(tool_name.to_string()),
        });
    }

    collection
}

pub(super) fn skip_for_current_user_item(item: &TranscriptItem) -> Option<InputSlimmingSkip> {
    match item {
        TranscriptItem::Message { role, .. } if role == "user" => Some(InputSlimmingSkip {
            reason: InputSlimmingSkipReason::CurrentUserTurn,
            tool_name: None,
        }),
        TranscriptItem::Message { .. }
        | TranscriptItem::Reasoning { .. }
        | TranscriptItem::HostedActivity { .. }
        | TranscriptItem::ToolCall { .. }
        | TranscriptItem::ToolResult { .. }
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

        let collection = candidates_from_item(3, &item, CandidateZone::HistoricalToolOutput, 1);

        assert_eq!(
            collection.candidates,
            vec![Candidate {
                index: 3,
                target: CandidateTarget::TextToolOutput,
                zone: CandidateZone::HistoricalToolOutput,
                call_id: "call".to_string(),
                tool_name: "shell".to_string(),
                text,
                success: None,
                original_tokens_approx: 2_000,
            }]
        );
        assert_eq!(collection.skips, Vec::new());
    }

    #[test]
    fn selects_structured_text_content_items() {
        let item = TranscriptItem::ToolResult {
            call_id: "call".to_string(),
            tool_name: "mcp".to_string(),
            payload: ToolResultPayload::Structured {
                content: "visible".to_string(),
                content_items: Some(vec![
                    ToolResultContentItem::InputText {
                        text: "long".repeat(3000),
                    },
                    ToolResultContentItem::InputImage {
                        image_url: "data:image/png;base64,abc".to_string(),
                    },
                ]),
                success: Some(true),
            },
        };

        let collection = candidates_from_item(0, &item, CandidateZone::LiveToolOutput, 1);

        assert_eq!(collection.candidates.len(), 1);
        assert_eq!(
            collection.candidates[0].target,
            CandidateTarget::StructuredContentItem { item_index: 0 }
        );
        assert_eq!(collection.candidates[0].zone, CandidateZone::LiveToolOutput);
        assert_eq!(collection.skips, Vec::new());
    }

    #[test]
    fn skips_structured_content_items_without_text() {
        let item = TranscriptItem::ToolResult {
            call_id: "call".to_string(),
            tool_name: "mcp".to_string(),
            payload: ToolResultPayload::Structured {
                content: "image".to_string(),
                content_items: Some(vec![ToolResultContentItem::InputImage {
                    image_url: "data:image/png;base64,abc".to_string(),
                }]),
                success: Some(true),
            },
        };

        let collection = candidates_from_item(0, &item, CandidateZone::HistoricalToolOutput, 1);

        assert_eq!(collection.candidates, Vec::new());
        assert_eq!(
            collection.skips,
            vec![InputSlimmingSkip {
                reason: InputSlimmingSkipReason::StructuredContentItems,
                tool_name: Some("mcp".to_string()),
            }]
        );
    }

    #[test]
    fn skips_existing_marker() {
        let item = tool_text("shell", "before <<lha-input:abcdef>> after");

        let collection = candidates_from_item(0, &item, CandidateZone::HistoricalToolOutput, 1);

        assert_eq!(collection.candidates, Vec::new());
        assert_eq!(
            collection.skips,
            vec![InputSlimmingSkip {
                reason: InputSlimmingSkipReason::AlreadySlimmed,
                tool_name: Some("shell".to_string()),
            }]
        );
    }

    #[test]
    fn skips_protected_runtime_markers() {
        let protected = [
            "Another language model started to solve this problem and produced a summary",
            "A proposed plan from before compaction is preserved below.",
            "Runtime note: the active programmer goal references a user-provided proposed plan",
            "The active programmer goal references a proposed plan stored at:",
            "<skill>\n<name>demo</name>",
            "<skill source=\"compact_backfill\">\n<name>demo</name>",
            "# AGENTS.md instructions for /tmp/project",
        ];

        for text in protected {
            let item = tool_text("shell", &format!("{text}\n{}", "x".repeat(8_000)));
            let collection = candidates_from_item(0, &item, CandidateZone::HistoricalToolOutput, 1);

            assert_eq!(collection.candidates, Vec::new());
            assert_eq!(
                collection.skips,
                vec![InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::AlreadySlimmed,
                    tool_name: Some("shell".to_string()),
                }]
            );
        }
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
    fn current_user_message_is_reported_with_skip_reason() {
        assert_eq!(
            skip_for_current_user_item(&user("now")),
            Some(InputSlimmingSkip {
                reason: InputSlimmingSkipReason::CurrentUserTurn,
                tool_name: None,
            })
        );
    }
}
