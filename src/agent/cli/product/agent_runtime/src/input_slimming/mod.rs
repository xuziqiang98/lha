mod candidate;
mod metrics;
mod store;
mod strategy;
mod tool;

use std::fmt;

use lha_llm::ToolResultPayload;
use lha_llm::TranscriptItem;
use lha_llm::TurnRequest;

use crate::product::agent::input_slimming::candidate::Candidate;
use crate::product::agent::input_slimming::candidate::candidate_from_item;
use crate::product::agent::input_slimming::candidate::latest_user_message_index;
use crate::product::agent::input_slimming::candidate::skip_for_current_turn_item;
use crate::product::agent::input_slimming::candidate::skip_for_protected_item;
use crate::product::agent::input_slimming::metrics::emit_metrics;
use crate::product::agent::input_slimming::strategy::StrategyOutput;
use crate::product::agent::input_slimming::strategy::slim_text;
use crate::product::agent::truncate::approx_token_count;
use crate::product::otel::OtelManager;

pub(crate) use store::InputSlimmingStore;
pub(crate) use store::StoredInputMetadata;
use store::hash_text;
pub(crate) use tool::INPUT_RETRIEVE_TOOL_NAME;
pub(crate) use tool::InputRetrieveHandler;
pub(crate) use tool::create_lha_input_retrieve_tool;

pub(crate) const INPUT_SLIMMING_MARKER_PREFIX: &str = "<<lha-input:";
const INPUT_SLIMMING_MARKER_SUFFIX: &str = ">>";
pub(crate) const DEFAULT_STORE_CAPACITY: usize = 1000;
pub(crate) const DEFAULT_STORE_TTL_SECONDS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputSlimmingOptions {
    pub(crate) min_candidate_tokens: usize,
    pub(crate) target_tokens: usize,
    pub(crate) min_saved_tokens: usize,
}

impl Default for InputSlimmingOptions {
    fn default() -> Self {
        Self {
            min_candidate_tokens: 1_024,
            target_tokens: 512,
            min_saved_tokens: 128,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct InputSlimmer {
    options: InputSlimmingOptions,
}

impl InputSlimmer {
    pub(crate) fn new(options: InputSlimmingOptions) -> Self {
        Self { options }
    }

    pub(crate) async fn slim_request(
        &self,
        request: &TurnRequest,
        store: &InputSlimmingStore,
    ) -> Result<InputSlimmingOutcome, InputSlimmingError> {
        let mut slimmed_request = request.clone();
        let mut metrics = InputSlimmingMetrics::default();

        let Some(latest_user_index) = latest_user_message_index(&request.conversation) else {
            metrics.approx_tokens_after = metrics.approx_tokens_before;
            return Ok(InputSlimmingOutcome::new(slimmed_request, metrics));
        };

        for (idx, item) in request.conversation.iter().enumerate() {
            if idx >= latest_user_index {
                if let Some(skip) = skip_for_current_turn_item(item) {
                    metrics.skipped.push(skip);
                }
                continue;
            }

            let candidate = match candidate_from_item(idx, item, self.options.min_candidate_tokens)
            {
                Ok(candidate) => candidate,
                Err(Some(skip)) => {
                    metrics.skipped.push(skip);
                    continue;
                }
                Err(None) => {
                    if let Some(skip) = skip_for_protected_item(item) {
                        metrics.skipped.push(skip);
                    }
                    continue;
                }
            };

            metrics.candidates += 1;
            metrics.approx_tokens_before = metrics
                .approx_tokens_before
                .saturating_add(candidate.original_tokens);

            match self.slim_candidate(&candidate, store).await {
                Ok(Some(accepted)) => {
                    metrics.slimmed += 1;
                    metrics.approx_tokens_after = metrics
                        .approx_tokens_after
                        .saturating_add(accepted.after_tokens);
                    metrics.approx_tokens_saved = metrics
                        .approx_tokens_saved
                        .saturating_add(accepted.saved_tokens);
                    metrics.refs.push(accepted.reference);
                    replace_candidate_text(
                        &mut slimmed_request.conversation[candidate.index],
                        accepted.replacement,
                    );
                }
                Ok(None) => {}
                Err(skip) => metrics.skipped.push(skip),
            }
        }

        metrics.approx_tokens_after = metrics
            .approx_tokens_before
            .saturating_sub(metrics.approx_tokens_saved);

        Ok(InputSlimmingOutcome::new(slimmed_request, metrics))
    }

    async fn slim_candidate(
        &self,
        candidate: &Candidate,
        store: &InputSlimmingStore,
    ) -> Result<Option<AcceptedSlimming>, InputSlimmingSkip> {
        let Some(strategy_output) =
            slim_text(candidate.text.as_str(), candidate.success, self.options)
        else {
            if candidate.success == Some(false) {
                return Err(candidate.skip(InputSlimmingSkipReason::FailedNonLogResult));
            }
            return Err(candidate.skip(InputSlimmingSkipReason::CompressionError));
        };

        if candidate.success == Some(false)
            && strategy_output.strategy != InputSlimmingStrategy::LogCompact
        {
            return Err(candidate.skip(InputSlimmingSkipReason::FailedNonLogResult));
        }

        let hash = hash_text(candidate.text.as_str());
        let replacement = build_replacement(candidate.original_tokens, &hash, &strategy_output);
        let after_tokens = approx_token_count(&replacement);

        if after_tokens >= candidate.original_tokens
            || candidate.original_tokens.saturating_sub(after_tokens)
                < self.options.min_saved_tokens
        {
            return Err(candidate.skip(InputSlimmingSkipReason::NotTokenSaving));
        }

        let stored_hash = store
            .put(
                candidate.text.clone(),
                StoredInputMetadata {
                    strategy: strategy_output.strategy,
                    tool_name: candidate.tool_name.clone(),
                    original_tokens: candidate.original_tokens,
                },
            )
            .await;
        if stored_hash != hash {
            return Err(candidate.skip(InputSlimmingSkipReason::RetrievalUnavailable));
        }

        Ok(Some(AcceptedSlimming {
            replacement,
            after_tokens,
            saved_tokens: candidate.original_tokens.saturating_sub(after_tokens),
            reference: InputSlimmingRef {
                hash,
                strategy: strategy_output.strategy,
                tool_name: candidate.tool_name.clone(),
                original_tokens: candidate.original_tokens,
            },
        }))
    }

    pub(crate) fn emit_metrics(outcome: &InputSlimmingOutcome, otel: &OtelManager, model: &str) {
        emit_metrics(&outcome.metrics, otel, model);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InputSlimmingOutcome {
    pub(crate) request: TurnRequest,
    pub(crate) metrics: InputSlimmingMetrics,
    pub(crate) requires_retrieval_tool: bool,
    pub(crate) approx_tokens_saved: usize,
}

impl InputSlimmingOutcome {
    fn new(request: TurnRequest, metrics: InputSlimmingMetrics) -> Self {
        let approx_tokens_saved = metrics.approx_tokens_saved;
        let requires_retrieval_tool = metrics.slimmed > 0;
        Self {
            request,
            metrics,
            requires_retrieval_tool,
            approx_tokens_saved,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct InputSlimmingMetrics {
    pub(crate) candidates: usize,
    pub(crate) slimmed: usize,
    pub(crate) skipped: Vec<InputSlimmingSkip>,
    pub(crate) refs: Vec<InputSlimmingRef>,
    pub(crate) approx_tokens_before: usize,
    pub(crate) approx_tokens_after: usize,
    pub(crate) approx_tokens_saved: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSlimmingRef {
    pub(crate) hash: String,
    pub(crate) strategy: InputSlimmingStrategy,
    pub(crate) tool_name: String,
    pub(crate) original_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSlimmingSkip {
    pub(crate) reason: InputSlimmingSkipReason,
    pub(crate) tool_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputSlimmingSkipReason {
    ProtectedRole,
    CurrentUserTurn,
    RecentAssistant,
    BelowSizeFloor,
    AlreadySlimmed,
    NotTokenSaving,
    RetrievalUnavailable,
    StructuredContentItems,
    FailedNonLogResult,
    UnsupportedItem,
    CompressionError,
}

impl InputSlimmingSkipReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProtectedRole => "protected_role",
            Self::CurrentUserTurn => "current_user_turn",
            Self::RecentAssistant => "recent_assistant",
            Self::BelowSizeFloor => "below_size_floor",
            Self::AlreadySlimmed => "already_slimmed",
            Self::NotTokenSaving => "not_token_saving",
            Self::RetrievalUnavailable => "retrieval_unavailable",
            Self::StructuredContentItems => "structured_content_items",
            Self::FailedNonLogResult => "failed_non_log_result",
            Self::UnsupportedItem => "unsupported_item",
            Self::CompressionError => "compression_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputSlimmingStrategy {
    JsonArraySample,
    SearchResultCompact,
    LogCompact,
    DiffCompact,
    PlainTextHeadTail,
}

impl InputSlimmingStrategy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::JsonArraySample => "json_array_sample",
            Self::SearchResultCompact => "search_result_compact",
            Self::LogCompact => "log_compact",
            Self::DiffCompact => "diff_compact",
            Self::PlainTextHeadTail => "plain_text_head_tail",
        }
    }
}

#[derive(Debug)]
pub(crate) struct InputSlimmingError {
    message: String,
}

impl fmt::Display for InputSlimmingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

struct AcceptedSlimming {
    replacement: String,
    after_tokens: usize,
    saved_tokens: usize,
    reference: InputSlimmingRef,
}

fn build_replacement(before_tokens: usize, hash: &str, strategy_output: &StrategyOutput) -> String {
    format!(
        "[Input Slimming: original approx {before_tokens} tokens, strategy={}. Retrieve the original with lha_input_retrieve(hash=\"{hash}\") if more detail is needed.]\n{}{}{}\n\n{}",
        strategy_output.strategy.as_str(),
        INPUT_SLIMMING_MARKER_PREFIX,
        hash,
        INPUT_SLIMMING_MARKER_SUFFIX,
        strategy_output.body
    )
}

fn replace_candidate_text(item: &mut TranscriptItem, replacement: String) {
    match item {
        TranscriptItem::ToolResult {
            payload: ToolResultPayload::Text { output },
            ..
        } => {
            *output = replacement;
        }
        TranscriptItem::ToolResult {
            payload:
                ToolResultPayload::Structured {
                    content,
                    content_items: None,
                    ..
                },
            ..
        } => {
            *content = replacement;
        }
        TranscriptItem::Message { .. }
        | TranscriptItem::Reasoning { .. }
        | TranscriptItem::HostedActivity { .. }
        | TranscriptItem::ToolCall { .. }
        | TranscriptItem::ToolResult { .. }
        | TranscriptItem::Unknown { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lha_llm::ContentItem;
    use lha_llm::ToolResultContentItem;
    use pretty_assertions::assert_eq;

    fn request_with(conversation: Vec<TranscriptItem>) -> TurnRequest {
        TurnRequest {
            conversation,
            ..Default::default()
        }
    }

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

    fn tool_text(tool_name: &str, output: String) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: "call".to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolResultPayload::Text { output },
        }
    }

    fn structured_tool(
        content: String,
        content_items: Option<Vec<ToolResultContentItem>>,
    ) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: "call".to_string(),
            tool_name: "shell".to_string(),
            payload: ToolResultPayload::Structured {
                content,
                content_items,
                success: Some(true),
            },
        }
    }

    #[tokio::test]
    async fn slimmed_request_contains_marker_and_keeps_original_request_unchanged() {
        let original_text = format!("{}\n{}", "line one".repeat(3000), "line two".repeat(3000));
        let request = request_with(vec![
            tool_text("apply_patch", original_text.clone()),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert!(outcome.requires_retrieval_tool);
        assert!(outcome.approx_tokens_saved > 0);
        assert_eq!(
            request.conversation[0],
            tool_text("apply_patch", original_text.clone())
        );

        let TranscriptItem::ToolResult {
            payload: ToolResultPayload::Text { output },
            ..
        } = &outcome.request.conversation[0]
        else {
            panic!("expected text tool result");
        };
        assert!(output.contains("<<lha-input:"));
        assert!(output.contains("plain_text_head_tail"));
    }

    #[tokio::test]
    async fn no_op_outcome_does_not_require_retrieval_tool() {
        let request = request_with(vec![tool_text("shell", "short".to_string()), user("next")]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.request.conversation, request.conversation);
        assert!(!outcome.requires_retrieval_tool);
        assert_eq!(outcome.approx_tokens_saved, 0);
    }

    #[tokio::test]
    async fn multiple_accepted_candidates_produce_multiple_hashes() {
        let first = "first ".repeat(5_000);
        let second = "second ".repeat(5_000);
        let request = request_with(vec![
            tool_text("shell", first),
            tool_text("rg", second),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.metrics.slimmed, 2);
        assert_eq!(outcome.metrics.refs.len(), 2);
        assert_ne!(outcome.metrics.refs[0].hash, outcome.metrics.refs[1].hash);
        for reference in &outcome.metrics.refs {
            assert_eq!(reference.hash.len(), 24);
        }
    }

    #[tokio::test]
    async fn structured_content_without_content_items_can_be_slimmed() {
        let request = request_with(vec![
            structured_tool("structured ".repeat(5_000), None),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert!(outcome.requires_retrieval_tool);
        let TranscriptItem::ToolResult {
            payload:
                ToolResultPayload::Structured {
                    content,
                    content_items: None,
                    ..
                },
            ..
        } = &outcome.request.conversation[0]
        else {
            panic!("expected structured tool result");
        };
        assert!(content.contains("<<lha-input:"));
    }

    #[tokio::test]
    async fn structured_content_items_are_skipped() {
        let request = request_with(vec![
            structured_tool(
                "x".repeat(8_000),
                Some(vec![ToolResultContentItem::InputText {
                    text: "visible".to_string(),
                }]),
            ),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.request.conversation, request.conversation);
        assert_eq!(
            outcome.metrics.skipped,
            vec![InputSlimmingSkip {
                reason: InputSlimmingSkipReason::StructuredContentItems,
                tool_name: Some("shell".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn tool_results_after_latest_user_message_are_not_candidates() {
        let text = "after user".repeat(5000);
        let request = request_with(vec![user("first"), tool_text("shell", text)]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.metrics.candidates, 0);
        assert_eq!(outcome.request.conversation, request.conversation);
    }
}
