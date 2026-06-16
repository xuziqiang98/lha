#[cfg(test)]
mod bench_eval;
mod candidate;
mod metrics;
mod store;
mod strategy;
mod tool;

use std::fmt;

use lha_llm::ToolResultContentItem;
use lha_llm::ToolResultPayload;
use lha_llm::TranscriptItem;
use lha_llm::TurnRequest;

use crate::product::agent::features::Feature;
use crate::product::agent::features::Features;
use crate::product::agent::input_slimming::candidate::Candidate;
use crate::product::agent::input_slimming::candidate::CandidateTarget;
use crate::product::agent::input_slimming::candidate::candidates_from_item;
use crate::product::agent::input_slimming::candidate::latest_user_message_index;
use crate::product::agent::input_slimming::candidate::skip_for_current_user_item;
use crate::product::agent::input_slimming::candidate::skip_for_protected_item;
use crate::product::agent::input_slimming::candidate::skip_for_recent_live_output_item;
use crate::product::agent::input_slimming::metrics::emit_metrics;
use crate::product::agent::input_slimming::strategy::StrategyOutput;
use crate::product::agent::input_slimming::strategy::slim_text_for_tool;
use crate::product::agent::protocol::InputSlimmingScope;
use crate::product::agent::protocol::InputSlimmingTokenStats;
use crate::product::agent::truncate::approx_token_count;
use crate::product::otel::OtelManager;
use crate::product::protocol::protocol::InputSlimmingStoredInputItem;
use crate::product::protocol::protocol::InputSlimmingStoredInputMetadata;

pub(crate) use candidate::CandidateZone;
pub(crate) use store::InputSlimmingStore;
pub(crate) use store::InputSlimmingStoreError;
use store::SlimmedReplacement;
use store::SlimmedReplacementCacheKey;
pub(crate) use store::StoredInputMetadata;
use store::hash_text;
pub(crate) use tool::INPUT_RETRIEVE_TOOL_NAME;
pub(crate) use tool::InputRetrieveHandler;
pub(crate) use tool::create_lha_input_retrieve_tool;

pub(crate) const INPUT_SLIMMING_MARKER_PREFIX: &str = "<<lha-input:";
const INPUT_SLIMMING_MARKER_SUFFIX: &str = ">>";
pub(crate) const DEFAULT_STORE_CAPACITY: usize = 1000;
pub(crate) const DEFAULT_STORE_TTL_SECONDS: u64 = 300;
const INPUT_SLIMMING_STRATEGY_VERSION: u32 = 1;

type RequestTokenEstimator<'a> = dyn Fn(&TurnRequest) -> Option<usize> + Send + Sync + 'a;

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

pub(crate) struct InputSlimmingContext<'a> {
    pub(crate) store: &'a InputSlimmingStore,
    pub(crate) turn_id: &'a str,
    pub(crate) estimate_request_tokens: Option<&'a RequestTokenEstimator<'a>>,
    pub(crate) mode: InputSlimmingMode,
    pub(crate) scope: InputSlimmingScope,
    pub(crate) wire_api: InputSlimmingWireApi,
    pub(crate) context_window: Option<i64>,
    pub(crate) estimated_input_tokens: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputSlimmingMode {
    Apply,
    MeasureOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputSlimmingActivation {
    Disabled,
    Scope(InputSlimmingScope),
    Conflict,
}

pub(crate) const INPUT_SLIMMING_CONFLICT_WARNING: &str = "Input slimming strategies are mutually exclusive; enable only one of input_slimming or input_slimming_live_zone.";

pub(crate) fn resolve_input_slimming_scope(features: &Features) -> InputSlimmingActivation {
    match (
        features.enabled(Feature::InputSlimming),
        features.enabled(Feature::InputSlimmingLiveZone),
    ) {
        (false, false) => InputSlimmingActivation::Disabled,
        (true, false) => InputSlimmingActivation::Scope(InputSlimmingScope::HistoricalToolOutputs),
        (false, true) => InputSlimmingActivation::Scope(InputSlimmingScope::LiveZoneToolOutputs),
        (true, true) => InputSlimmingActivation::Conflict,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputSlimmingWireApi {
    Responses,
    Chat,
    Messages,
}

impl InputSlimmingWireApi {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::Chat => "chat",
            Self::Messages => "messages",
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
        self.slim_request_with_context(
            request,
            InputSlimmingContext {
                store,
                turn_id: "test",
                estimate_request_tokens: None,
                mode: InputSlimmingMode::Apply,
                scope: InputSlimmingScope::HistoricalToolOutputs,
                wire_api: InputSlimmingWireApi::Responses,
                context_window: None,
                estimated_input_tokens: None,
            },
        )
        .await
    }

    pub(crate) async fn slim_request_with_context(
        &self,
        request: &TurnRequest,
        context: InputSlimmingContext<'_>,
    ) -> Result<InputSlimmingOutcome, InputSlimmingError> {
        let mut slimmed_request = request.clone();
        let mut metrics = InputSlimmingMetrics::default();
        let mut persisted_entries = Vec::new();
        let mut context_stats_candidates = Vec::new();

        let Some(latest_user_index) = latest_user_message_index(&request.conversation) else {
            metrics.approx_tokens_after = metrics.approx_tokens_before;
            return Ok(InputSlimmingOutcome::new(
                slimmed_request,
                metrics,
                context.mode,
                persisted_entries,
                context_stats_candidates,
            ));
        };

        for (idx, item) in request.conversation.iter().enumerate() {
            let Some(zone) = candidate_zone_for_scope(context.scope, idx, latest_user_index) else {
                if idx == latest_user_index {
                    if let Some(skip) = skip_for_current_user_item(item) {
                        metrics.skipped.push(skip);
                    }
                } else if let Some(skip) = skip_for_protected_item(item) {
                    metrics.skipped.push(skip);
                }
                continue;
            };

            let collection =
                candidates_from_item(idx, item, zone, self.options.min_candidate_tokens);
            if context.scope == InputSlimmingScope::HistoricalToolOutputs
                && matches!(zone, CandidateZone::LiveToolOutput)
            {
                if collection
                    .skips
                    .iter()
                    .any(|skip| skip.reason == InputSlimmingSkipReason::AlreadySlimmed)
                {
                    metrics.skipped.extend(collection.skips);
                    continue;
                }
                if let Some(skip) = skip_for_recent_live_output_item(item) {
                    metrics.skipped.push(skip);
                    continue;
                }
            }
            if collection.candidates.is_empty() {
                if collection.skips.is_empty() {
                    if let Some(skip) = skip_for_protected_item(item) {
                        metrics.skipped.push(skip);
                    }
                } else {
                    metrics.skipped.extend(collection.skips);
                }
                continue;
            }
            metrics.skipped.extend(collection.skips);

            for candidate in collection.candidates {
                metrics.candidates += 1;
                metrics.approx_tokens_before = metrics
                    .approx_tokens_before
                    .saturating_add(candidate.original_tokens_approx);

                match self
                    .slim_candidate(&slimmed_request, &candidate, &context)
                    .await
                {
                    Ok(Some(accepted)) => {
                        let strategy_metric = InputSlimmingStrategyMetric {
                            strategy: accepted.reference.strategy,
                            tool_name: accepted.reference.tool_name.clone(),
                            zone: candidate.zone,
                            gate_method: accepted.gate.method,
                            tokens_before: accepted.gate.tokens_before,
                            tokens_after: accepted.gate.tokens_after,
                            tokens_saved: accepted.gate.tokens_saved,
                        };
                        metrics.strategy_metrics.push(strategy_metric);
                        if accepted.gate.method == InputSlimmingTokenGateMethod::ApproxTextFallback
                        {
                            metrics.token_gate_fallbacks += 1;
                        }
                        if accepted.from_cache {
                            metrics.cache_hits += 1;
                        }
                        metrics.approx_tokens_after = metrics
                            .approx_tokens_after
                            .saturating_add(accepted.replacement_tokens_approx);
                        metrics.approx_tokens_saved = metrics
                            .approx_tokens_saved
                            .saturating_add(accepted.gate.tokens_saved);
                        context_stats_candidates.push(InputSlimmingContextStatCandidate {
                            occurrence_key: accepted.occurrence_key.clone(),
                            tokens_before: usize_to_i64(accepted.reference.original_tokens),
                            tokens_after: usize_to_i64(accepted.reference.compressed_tokens),
                            tokens_saved: usize_to_i64(
                                accepted
                                    .reference
                                    .original_tokens
                                    .saturating_sub(accepted.reference.compressed_tokens),
                            ),
                        });

                        match context.mode {
                            InputSlimmingMode::Apply => {
                                metrics.slimmed += 1;
                                if let Some(entry) = accepted.persisted_entry {
                                    persisted_entries.push(entry);
                                }
                                metrics.refs.push(accepted.reference);
                                replace_candidate_text(
                                    &mut slimmed_request.conversation[candidate.index],
                                    candidate.target,
                                    accepted.replacement,
                                );
                            }
                            InputSlimmingMode::MeasureOnly => {
                                metrics.measured_only += 1;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(rejected) => {
                        if rejected.gate_method
                            == Some(InputSlimmingTokenGateMethod::ApproxTextFallback)
                        {
                            metrics.token_gate_fallbacks += 1;
                        }
                        metrics.skipped.push(rejected.skip);
                    }
                }
            }
        }

        metrics.approx_tokens_after = metrics
            .approx_tokens_before
            .saturating_sub(metrics.approx_tokens_saved);

        Ok(InputSlimmingOutcome::new(
            slimmed_request,
            metrics,
            context.mode,
            persisted_entries,
            context_stats_candidates,
        ))
    }

    async fn slim_candidate(
        &self,
        current_request: &TurnRequest,
        candidate: &Candidate,
        context: &InputSlimmingContext<'_>,
    ) -> Result<Option<AcceptedSlimming>, RejectedSlimming> {
        let options = input_slimming_options_for_context(
            context.context_window,
            context.estimated_input_tokens,
            candidate.tool_name.as_str(),
            candidate.zone,
        );
        let options = merge_options(self.options, options);
        let hash = hash_text(candidate.text.as_str());
        let occurrence_key = InputSlimmingOccurrenceKey {
            call_id: candidate.call_id.clone(),
            target: candidate.target.stable_key(),
            original_hash: hash.clone(),
        };
        let cache_key = SlimmedReplacementCacheKey {
            original_hash: hash.clone(),
            tool_name: candidate.tool_name.clone(),
            scope: context.scope,
            zone: candidate.zone,
            success: candidate.success,
            strategy_version: INPUT_SLIMMING_STRATEGY_VERSION,
            min_candidate_tokens: options.min_candidate_tokens,
            target_tokens: options.target_tokens,
            min_saved_tokens: options.min_saved_tokens,
        };
        if context.mode == InputSlimmingMode::Apply
            && let Some(cached) = context.store.get_slimmed_replacement(&cache_key).await
        {
            let gate = token_gate_decision(
                current_request,
                candidate,
                &cached.replacement,
                context.estimate_request_tokens,
            );
            if gate.tokens_after >= gate.tokens_before
                || gate.tokens_saved < options.min_saved_tokens
            {
                return Err(RejectedSlimming {
                    skip: candidate.skip(InputSlimmingSkipReason::NotTokenSaving),
                    gate_method: Some(gate.method),
                });
            }
            let persisted_entry =
                ensure_persisted_entry(context, &hash, candidate, cached.metadata.clone()).await?;
            return Ok(Some(AcceptedSlimming {
                replacement: cached.replacement,
                gate,
                reference: cached.reference,
                persisted_entry,
                replacement_tokens_approx: cached.replacement_tokens_approx,
                occurrence_key,
                from_cache: true,
            }));
        }

        let Some(strategy_output) = slim_text_for_tool(
            candidate.text.as_str(),
            candidate.success,
            options,
            candidate.tool_name.as_str(),
        ) else {
            if candidate.success == Some(false) {
                return Err(candidate.reject(InputSlimmingSkipReason::FailedNonLogResult));
            }
            return Err(candidate.reject(InputSlimmingSkipReason::CompressionError));
        };

        if candidate.success == Some(false)
            && strategy_output.strategy != InputSlimmingStrategy::LogCompact
        {
            return Err(candidate.reject(InputSlimmingSkipReason::FailedNonLogResult));
        }

        let replacement =
            build_replacement(candidate.original_tokens_approx, &hash, &strategy_output);
        let gate = token_gate_decision(
            current_request,
            candidate,
            &replacement,
            context.estimate_request_tokens,
        );

        if gate.tokens_after >= gate.tokens_before || gate.tokens_saved < options.min_saved_tokens {
            return Err(RejectedSlimming {
                skip: candidate.skip(InputSlimmingSkipReason::NotTokenSaving),
                gate_method: Some(gate.method),
            });
        }

        let replacement_tokens_approx = approx_token_count(&replacement);
        let metadata = StoredInputMetadata {
            scope: context.scope,
            strategy: strategy_output.strategy,
            tool_name: candidate.tool_name.clone(),
            original_tokens: candidate.original_tokens_approx,
            compressed_tokens: replacement_tokens_approx,
            created_turn_id: context.turn_id.to_string(),
        };
        let persisted_entry =
            ensure_persisted_entry(context, &hash, candidate, metadata.clone()).await?;
        let reference = InputSlimmingRef {
            hash: hash.clone(),
            strategy: strategy_output.strategy,
            tool_name: candidate.tool_name.clone(),
            original_tokens: candidate.original_tokens_approx,
            compressed_tokens: replacement_tokens_approx,
            zone: candidate.zone,
        };
        if context.mode == InputSlimmingMode::Apply {
            context
                .store
                .put_slimmed_replacement(
                    cache_key,
                    SlimmedReplacement {
                        replacement: replacement.clone(),
                        reference: reference.clone(),
                        gate,
                        metadata,
                        replacement_tokens_approx,
                    },
                )
                .await;
        }

        Ok(Some(AcceptedSlimming {
            replacement,
            gate,
            reference,
            persisted_entry,
            replacement_tokens_approx,
            occurrence_key,
            from_cache: false,
        }))
    }

    pub(crate) fn emit_metrics(
        outcome: &InputSlimmingOutcome,
        otel: &OtelManager,
        model: &str,
        scope: InputSlimmingScope,
        wire_api: InputSlimmingWireApi,
    ) {
        emit_metrics(&outcome.metrics, otel, model, scope, wire_api);
    }
}

fn candidate_zone_for_scope(
    scope: InputSlimmingScope,
    index: usize,
    latest_user_index: usize,
) -> Option<CandidateZone> {
    match scope {
        InputSlimmingScope::HistoricalToolOutputs => {
            if index == latest_user_index {
                None
            } else if index < latest_user_index {
                Some(CandidateZone::HistoricalToolOutput)
            } else {
                Some(CandidateZone::LiveToolOutput)
            }
        }
        InputSlimmingScope::LiveZoneToolOutputs => {
            if index > latest_user_index {
                Some(CandidateZone::LiveToolOutput)
            } else {
                None
            }
        }
    }
}

fn merge_options(
    base: InputSlimmingOptions,
    contextual: InputSlimmingOptions,
) -> InputSlimmingOptions {
    InputSlimmingOptions {
        min_candidate_tokens: base
            .min_candidate_tokens
            .min(contextual.min_candidate_tokens),
        target_tokens: base.target_tokens.min(contextual.target_tokens),
        min_saved_tokens: base.min_saved_tokens.min(contextual.min_saved_tokens),
    }
}

pub(crate) fn input_slimming_options_for_context(
    context_window: Option<i64>,
    estimated_input_tokens: Option<i64>,
    tool_name: &str,
    zone: CandidateZone,
) -> InputSlimmingOptions {
    let mut options = InputSlimmingOptions::default();
    let pressure = match (context_window, estimated_input_tokens) {
        (Some(window), Some(tokens)) if window > 0 && tokens > 0 => {
            (tokens as f64 / window as f64).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };

    if pressure >= 0.8 {
        options.min_candidate_tokens = 512;
        options.target_tokens = 384;
        options.min_saved_tokens = 64;
    } else if pressure >= 0.3 {
        options.min_candidate_tokens = 768;
        options.target_tokens = 448;
        options.min_saved_tokens = 96;
    }

    if matches!(zone, CandidateZone::LiveToolOutput) {
        options.min_candidate_tokens = options.min_candidate_tokens.max(1_024);
        options.min_saved_tokens = options.min_saved_tokens.max(128);
    }

    let lower_tool = tool_name.to_lowercase();
    if lower_tool.contains("shell")
        || lower_tool.contains("unified_exec")
        || lower_tool.contains("cargo")
        || lower_tool.contains("test")
    {
        options.target_tokens = options.target_tokens.min(384);
    } else if lower_tool.contains("rg")
        || lower_tool.contains("grep")
        || lower_tool.contains("search")
    {
        options.target_tokens = options.target_tokens.min(448);
    } else if lower_tool.contains("diff") || lower_tool.contains("apply_patch") {
        options.target_tokens = options.target_tokens.min(512);
    }

    options
}

#[derive(Debug, Clone)]
pub(crate) struct InputSlimmingOutcome {
    pub(crate) request: TurnRequest,
    pub(crate) metrics: InputSlimmingMetrics,
    pub(crate) requires_retrieval_tool: bool,
    pub(crate) approx_tokens_saved: usize,
    pub(crate) persisted_entries: Vec<InputSlimmingPersistedEntry>,
    pub(crate) context_stats_candidates: Vec<InputSlimmingContextStatCandidate>,
}

impl InputSlimmingOutcome {
    fn new(
        request: TurnRequest,
        metrics: InputSlimmingMetrics,
        mode: InputSlimmingMode,
        persisted_entries: Vec<InputSlimmingPersistedEntry>,
        context_stats_candidates: Vec<InputSlimmingContextStatCandidate>,
    ) -> Self {
        let approx_tokens_saved = metrics.approx_tokens_saved;
        let requires_retrieval_tool = mode == InputSlimmingMode::Apply && metrics.slimmed > 0;
        Self {
            request,
            metrics,
            requires_retrieval_tool,
            approx_tokens_saved,
            persisted_entries,
            context_stats_candidates,
        }
    }

    pub(crate) fn token_stats(&self) -> InputSlimmingTokenStats {
        InputSlimmingTokenStats {
            tokens_before: usize_to_i64(self.metrics.approx_tokens_before),
            tokens_after: usize_to_i64(self.metrics.approx_tokens_after),
            tokens_saved: usize_to_i64(self.metrics.approx_tokens_saved),
            replacements: usize_to_i64(self.metrics.slimmed),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InputSlimmingOccurrenceKey {
    pub(crate) call_id: String,
    pub(crate) target: String,
    pub(crate) original_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSlimmingContextStatCandidate {
    pub(crate) occurrence_key: InputSlimmingOccurrenceKey,
    pub(crate) tokens_before: i64,
    pub(crate) tokens_after: i64,
    pub(crate) tokens_saved: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSlimmingPersistedEntry {
    pub(crate) hash: String,
    pub(crate) original: String,
    pub(crate) metadata: StoredInputMetadata,
}

impl From<InputSlimmingPersistedEntry> for InputSlimmingStoredInputItem {
    fn from(value: InputSlimmingPersistedEntry) -> Self {
        Self {
            hash: value.hash,
            original: value.original,
            metadata: value.metadata.into(),
        }
    }
}

impl From<StoredInputMetadata> for InputSlimmingStoredInputMetadata {
    fn from(value: StoredInputMetadata) -> Self {
        Self {
            scope: value.scope,
            strategy: value.strategy.as_str().to_string(),
            tool_name: value.tool_name,
            original_tokens: value.original_tokens,
            compressed_tokens: value.compressed_tokens,
            created_turn_id: value.created_turn_id,
        }
    }
}

impl TryFrom<InputSlimmingStoredInputMetadata> for StoredInputMetadata {
    type Error = String;

    fn try_from(value: InputSlimmingStoredInputMetadata) -> Result<Self, Self::Error> {
        let Some(strategy) = InputSlimmingStrategy::from_str(value.strategy.as_str()) else {
            return Err(format!(
                "unknown input slimming strategy `{}`",
                value.strategy
            ));
        };
        Ok(Self {
            scope: value.scope,
            strategy,
            tool_name: value.tool_name,
            original_tokens: value.original_tokens,
            compressed_tokens: value.compressed_tokens,
            created_turn_id: value.created_turn_id,
        })
    }
}

impl TryFrom<InputSlimmingStoredInputItem> for InputSlimmingPersistedEntry {
    type Error = String;

    fn try_from(value: InputSlimmingStoredInputItem) -> Result<Self, Self::Error> {
        Ok(Self {
            hash: value.hash,
            original: value.original,
            metadata: value.metadata.try_into()?,
        })
    }
}

fn usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct InputSlimmingMetrics {
    pub(crate) candidates: usize,
    pub(crate) slimmed: usize,
    pub(crate) measured_only: usize,
    pub(crate) cache_hits: usize,
    pub(crate) skipped: Vec<InputSlimmingSkip>,
    pub(crate) refs: Vec<InputSlimmingRef>,
    pub(crate) approx_tokens_before: usize,
    pub(crate) approx_tokens_after: usize,
    pub(crate) approx_tokens_saved: usize,
    pub(crate) token_gate_fallbacks: usize,
    pub(crate) strategy_metrics: Vec<InputSlimmingStrategyMetric>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSlimmingStrategyMetric {
    pub(crate) strategy: InputSlimmingStrategy,
    pub(crate) tool_name: String,
    pub(crate) zone: CandidateZone,
    pub(crate) gate_method: InputSlimmingTokenGateMethod,
    pub(crate) tokens_before: usize,
    pub(crate) tokens_after: usize,
    pub(crate) tokens_saved: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputSlimmingRef {
    pub(crate) hash: String,
    pub(crate) strategy: InputSlimmingStrategy,
    pub(crate) tool_name: String,
    pub(crate) original_tokens: usize,
    pub(crate) compressed_tokens: usize,
    pub(crate) zone: CandidateZone,
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

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "json_array_sample" => Some(Self::JsonArraySample),
            "search_result_compact" => Some(Self::SearchResultCompact),
            "log_compact" => Some(Self::LogCompact),
            "diff_compact" => Some(Self::DiffCompact),
            "plain_text_head_tail" => Some(Self::PlainTextHeadTail),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputSlimmingTokenGateMethod {
    ModelRequestEstimate,
    ApproxTextFallback,
}

impl InputSlimmingTokenGateMethod {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ModelRequestEstimate => "model_request_estimate",
            Self::ApproxTextFallback => "approx_text_fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputSlimmingTokenGateDecision {
    pub(crate) method: InputSlimmingTokenGateMethod,
    pub(crate) tokens_before: usize,
    pub(crate) tokens_after: usize,
    pub(crate) tokens_saved: usize,
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
    gate: InputSlimmingTokenGateDecision,
    reference: InputSlimmingRef,
    persisted_entry: Option<InputSlimmingPersistedEntry>,
    replacement_tokens_approx: usize,
    occurrence_key: InputSlimmingOccurrenceKey,
    from_cache: bool,
}

struct RejectedSlimming {
    skip: InputSlimmingSkip,
    gate_method: Option<InputSlimmingTokenGateMethod>,
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

async fn ensure_persisted_entry(
    context: &InputSlimmingContext<'_>,
    hash: &str,
    candidate: &Candidate,
    metadata: StoredInputMetadata,
) -> Result<Option<InputSlimmingPersistedEntry>, RejectedSlimming> {
    if context.mode != InputSlimmingMode::Apply {
        return Ok(None);
    }

    if context.store.get(hash).await.is_some() {
        return Ok(None);
    }

    context
        .store
        .put_with_hash(hash.to_string(), candidate.text.clone(), metadata.clone())
        .await
        .map_err(|_| candidate.reject(InputSlimmingSkipReason::RetrievalUnavailable))?;

    Ok(Some(InputSlimmingPersistedEntry {
        hash: hash.to_string(),
        original: candidate.text.clone(),
        metadata,
    }))
}

fn token_gate_decision(
    current_request: &TurnRequest,
    candidate: &Candidate,
    replacement: &str,
    estimate_request_tokens: Option<&RequestTokenEstimator<'_>>,
) -> InputSlimmingTokenGateDecision {
    if let Some(estimate_request_tokens) = estimate_request_tokens {
        let mut trial_request = current_request.clone();
        replace_candidate_text(
            &mut trial_request.conversation[candidate.index],
            candidate.target,
            replacement.to_string(),
        );
        if let (Some(tokens_before), Some(tokens_after)) = (
            estimate_request_tokens(current_request),
            estimate_request_tokens(&trial_request),
        ) {
            return InputSlimmingTokenGateDecision {
                method: InputSlimmingTokenGateMethod::ModelRequestEstimate,
                tokens_before,
                tokens_after,
                tokens_saved: tokens_before.saturating_sub(tokens_after),
            };
        }
    }

    let tokens_after = approx_token_count(replacement);
    InputSlimmingTokenGateDecision {
        method: InputSlimmingTokenGateMethod::ApproxTextFallback,
        tokens_before: candidate.original_tokens_approx,
        tokens_after,
        tokens_saved: candidate
            .original_tokens_approx
            .saturating_sub(tokens_after),
    }
}

fn replace_candidate_text(item: &mut TranscriptItem, target: CandidateTarget, replacement: String) {
    match (item, target) {
        (
            TranscriptItem::ToolResult {
                payload: ToolResultPayload::Text { output },
                ..
            },
            CandidateTarget::TextToolOutput,
        ) => {
            *output = replacement;
        }
        (
            TranscriptItem::ToolResult {
                payload:
                    ToolResultPayload::Structured {
                        content,
                        content_items: None,
                        ..
                    },
                ..
            },
            CandidateTarget::StructuredContent,
        ) => {
            *content = replacement;
        }
        (
            TranscriptItem::ToolResult {
                payload:
                    ToolResultPayload::Structured {
                        content,
                        content_items: Some(items),
                        ..
                    },
                ..
            },
            CandidateTarget::StructuredContentItem { item_index },
        ) => {
            replace_structured_content_item_text(content, items, item_index, replacement);
        }
        _ => {}
    }
}

fn replace_structured_content_item_text(
    content: &mut String,
    items: &mut [ToolResultContentItem],
    item_index: usize,
    replacement: String,
) {
    let old_text_join = join_text_items(items);
    let all_items_are_text = items
        .iter()
        .all(|item| matches!(item, ToolResultContentItem::InputText { .. }));

    if let Some(ToolResultContentItem::InputText { text }) = items.get_mut(item_index) {
        *text = replacement;
    }

    if all_items_are_text || *content == old_text_join {
        *content = join_text_items(items);
    }
}

fn join_text_items(items: &[ToolResultContentItem]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            ToolResultContentItem::InputText { text } => Some(text.as_str()),
            ToolResultContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lha_llm::ContentItem;
    use lha_llm::ToolCallPayload;
    use lha_llm::api::provider::Provider;
    use lha_llm::api::provider::RetryConfig;
    use lha_llm::api::provider::WireApi;
    use lha_llm::api::requests::chat::ChatRequestBuilder;
    use lha_llm::api::requests::messages::MessagesRequestBuilder;
    use lha_llm::api::requests::responses::ResponsesRequestBuilder;
    use lha_llm::types::ReasoningItemContent;
    use pretty_assertions::assert_eq;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

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

    fn reasoning(text: &str) -> TranscriptItem {
        TranscriptItem::Reasoning {
            id: "reasoning".to_string(),
            summary: Vec::new(),
            content: Some(vec![ReasoningItemContent::Text {
                text: text.to_string(),
            }]),
            encrypted_content: None,
        }
    }

    fn hosted_activity() -> TranscriptItem {
        TranscriptItem::HostedActivity {
            id: Some("hosted".to_string()),
            activity_type: "web_search".to_string(),
            status: Some("done".to_string()),
            payload: serde_json::json!({"text":"do not slim"}),
        }
    }

    fn tool_call(tool_name: &str) -> TranscriptItem {
        tool_call_with_id("call", tool_name)
    }

    fn tool_call_with_id(call_id: &str, tool_name: &str) -> TranscriptItem {
        TranscriptItem::ToolCall {
            id: None,
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolCallPayload::JsonArguments {
                arguments: "{}".to_string(),
            },
        }
    }

    fn tool_text(tool_name: &str, output: String) -> TranscriptItem {
        tool_text_with_call("call", tool_name, output)
    }

    fn tool_text_with_call(call_id: &str, tool_name: &str, output: String) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolResultPayload::Text { output },
        }
    }

    fn structured_tool(
        content: String,
        content_items: Option<Vec<ToolResultContentItem>>,
    ) -> TranscriptItem {
        structured_tool_with_call("call", content, content_items)
    }

    fn structured_tool_with_call(
        call_id: &str,
        content: String,
        content_items: Option<Vec<ToolResultContentItem>>,
    ) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: call_id.to_string(),
            tool_name: "shell".to_string(),
            payload: ToolResultPayload::Structured {
                content,
                content_items,
                success: Some(true),
            },
        }
    }

    fn provider(wire: WireApi) -> Provider {
        Provider {
            name: "test".to_string(),
            base_url: "https://example.invalid/v1".to_string(),
            query_params: None,
            wire,
            headers: Default::default(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(5),
        }
    }

    async fn slim_request_with_scope(
        request: &TurnRequest,
        scope: InputSlimmingScope,
    ) -> InputSlimmingOutcome {
        let store = InputSlimmingStore::default();
        InputSlimmer::default()
            .slim_request_with_context(
                request,
                InputSlimmingContext {
                    store: &store,
                    turn_id: "turn",
                    estimate_request_tokens: None,
                    mode: InputSlimmingMode::Apply,
                    scope,
                    wire_api: InputSlimmingWireApi::Responses,
                    context_window: None,
                    estimated_input_tokens: None,
                },
            )
            .await
            .expect("slimming succeeds")
    }

    #[test]
    fn resolve_input_slimming_scope_returns_disabled_when_both_disabled() {
        let features = Features::with_defaults();

        assert_eq!(
            resolve_input_slimming_scope(&features),
            InputSlimmingActivation::Disabled
        );
    }

    #[test]
    fn resolve_input_slimming_scope_returns_historical_when_only_historical_enabled() {
        let mut features = Features::with_defaults();
        features.enable(Feature::InputSlimming);

        assert_eq!(
            resolve_input_slimming_scope(&features),
            InputSlimmingActivation::Scope(InputSlimmingScope::HistoricalToolOutputs)
        );
    }

    #[test]
    fn resolve_input_slimming_scope_returns_live_zone_when_only_live_zone_enabled() {
        let mut features = Features::with_defaults();
        features.enable(Feature::InputSlimmingLiveZone);

        assert_eq!(
            resolve_input_slimming_scope(&features),
            InputSlimmingActivation::Scope(InputSlimmingScope::LiveZoneToolOutputs)
        );
    }

    #[test]
    fn resolve_input_slimming_scope_reports_conflict_when_both_enabled() {
        let mut features = Features::with_defaults();
        features
            .enable(Feature::InputSlimming)
            .enable(Feature::InputSlimmingLiveZone);

        assert_eq!(
            resolve_input_slimming_scope(&features),
            InputSlimmingActivation::Conflict
        );
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
        assert_eq!(outcome.persisted_entries.len(), 1);
        assert_eq!(outcome.persisted_entries[0].original, original_text);
        assert_eq!(
            outcome.persisted_entries[0].metadata.strategy,
            InputSlimmingStrategy::PlainTextHeadTail
        );
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
    async fn reuses_cached_replacement_for_same_candidate_and_options() {
        let request = request_with(vec![
            tool_text("shell", "alpha ".repeat(5_000)),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let first = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("first slimming succeeds");
        let second = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("second slimming succeeds");

        assert_eq!(first.request.conversation, second.request.conversation);
        assert_eq!(first.metrics.cache_hits, 0);
        assert_eq!(second.metrics.cache_hits, 1);
        assert_eq!(second.persisted_entries, Vec::new());
        assert_eq!(
            first.context_stats_candidates,
            second.context_stats_candidates
        );
    }

    #[tokio::test]
    async fn different_options_do_not_reuse_cached_replacement() {
        let request = request_with(vec![
            tool_text("shell", "alpha ".repeat(5_000)),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let default_outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("default slimming succeeds");
        let tighter_outcome = InputSlimmer::new(InputSlimmingOptions {
            min_candidate_tokens: 1,
            target_tokens: 256,
            min_saved_tokens: 1,
        })
        .slim_request(&request, &store)
        .await
        .expect("tighter slimming succeeds");

        assert_eq!(default_outcome.metrics.cache_hits, 0);
        assert_eq!(tighter_outcome.metrics.cache_hits, 0);
        assert_ne!(
            default_outcome.request.conversation,
            tighter_outcome.request.conversation
        );
    }

    #[tokio::test]
    async fn different_success_status_does_not_reuse_cached_replacement() {
        let output = "alpha ".repeat(5_000);
        let success_request =
            request_with(vec![structured_tool(output.clone(), None), user("next")]);
        let failed_request = request_with(vec![
            TranscriptItem::ToolResult {
                call_id: "call".to_string(),
                tool_name: "shell".to_string(),
                payload: ToolResultPayload::Structured {
                    content: output,
                    content_items: None,
                    success: Some(false),
                },
            },
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let success_outcome = InputSlimmer::default()
            .slim_request(&success_request, &store)
            .await
            .expect("success slimming succeeds");
        let failed_outcome = InputSlimmer::default()
            .slim_request(&failed_request, &store)
            .await
            .expect("failed-result slimming succeeds");

        assert_eq!(success_outcome.metrics.cache_hits, 0);
        assert_eq!(failed_outcome.metrics.cache_hits, 0);
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
        assert_eq!(outcome.persisted_entries, Vec::new());
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
    async fn structured_text_content_item_can_be_slimmed() {
        let request = request_with(vec![
            structured_tool(
                "visible".to_string(),
                Some(vec![ToolResultContentItem::InputText {
                    text: "structured item ".repeat(5_000),
                }]),
            ),
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
                    content_items: Some(items),
                    ..
                },
            ..
        } = &outcome.request.conversation[0]
        else {
            panic!("expected structured tool result");
        };
        assert!(content.contains("<<lha-input:"));
        assert!(matches!(
            &items[0],
            ToolResultContentItem::InputText { text } if text.contains("<<lha-input:")
        ));
    }

    #[tokio::test]
    async fn mixed_structured_content_items_keep_images_unchanged() {
        let image_url = "data:image/png;base64,abc".to_string();
        let request = request_with(vec![
            structured_tool(
                "Generated image".to_string(),
                Some(vec![
                    ToolResultContentItem::InputText {
                        text: "image metadata ".repeat(5_000),
                    },
                    ToolResultContentItem::InputImage {
                        image_url: image_url.clone(),
                    },
                ]),
            ),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        let TranscriptItem::ToolResult {
            payload:
                ToolResultPayload::Structured {
                    content,
                    content_items: Some(items),
                    ..
                },
            ..
        } = &outcome.request.conversation[0]
        else {
            panic!("expected structured tool result");
        };
        assert_eq!(content, "Generated image");
        assert!(matches!(
            &items[0],
            ToolResultContentItem::InputText { text } if text.contains("<<lha-input:")
        ));
        assert_eq!(items[1], ToolResultContentItem::InputImage { image_url });
    }

    #[tokio::test]
    async fn responses_serialization_sees_slimmed_structured_content_item() {
        let request = request_with(vec![
            structured_tool(
                "visible".to_string(),
                Some(vec![ToolResultContentItem::InputText {
                    text: "provider visible ".repeat(5_000),
                }]),
            ),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");
        let provider_request =
            ResponsesRequestBuilder::new("gpt-test", "inst", &outcome.request.conversation)
                .build(&provider(WireApi::Responses))
                .expect("provider request");

        assert!(
            provider_request.body["input"][0]["output"][0]["text"]
                .as_str()
                .expect("text item")
                .contains("<<lha-input:")
        );
    }

    #[tokio::test]
    async fn messages_serialization_sees_slimmed_structured_content_item() {
        let request = request_with(vec![
            structured_tool(
                "visible".to_string(),
                Some(vec![ToolResultContentItem::InputText {
                    text: "messages visible ".repeat(5_000),
                }]),
            ),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");
        let provider_request =
            MessagesRequestBuilder::new("claude-test", "inst", &outcome.request.conversation, &[])
                .build(&provider(WireApi::Messages))
                .expect("provider request");

        assert!(
            provider_request.body["messages"][0]["content"][0]["content"][0]["text"]
                .as_str()
                .expect("text item")
                .contains("<<lha-input:")
        );
    }

    #[tokio::test]
    async fn responses_live_zone_serialization_preserves_prefix_before_slimmed_output() {
        let request = request_with(vec![
            assistant("historical assistant"),
            user("current prompt"),
            tool_call("shell"),
            structured_tool("responses live output ".repeat(5_000), None),
        ]);
        let before = ResponsesRequestBuilder::new("gpt-test", "inst", &request.conversation)
            .build(&provider(WireApi::Responses))
            .expect("provider request")
            .body;

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;
        let after = ResponsesRequestBuilder::new("gpt-test", "inst", &outcome.request.conversation)
            .build(&provider(WireApi::Responses))
            .expect("provider request")
            .body;

        let before_input = before["input"].as_array().expect("before input");
        let after_input = after["input"].as_array().expect("after input");
        let output_index = after_input
            .iter()
            .position(|item| item["type"] == "function_call_output")
            .expect("function call output");
        assert_eq!(&after_input[..output_index], &before_input[..output_index]);
        assert!(
            after_input[output_index]["output"]
                .as_str()
                .expect("slimmed output")
                .contains("<<lha-input:")
        );
    }

    #[tokio::test]
    async fn chat_live_zone_serialization_preserves_prefix_and_slims_parallel_tool_outputs() {
        let request = request_with(vec![
            user("current prompt"),
            tool_call_with_id("call-1", "shell"),
            tool_call_with_id("call-2", "rg"),
            tool_text_with_call("call-1", "shell", "first chat output ".repeat(5_000)),
            tool_text_with_call("call-2", "rg", "second chat output ".repeat(5_000)),
        ]);
        let before = ChatRequestBuilder::new("gpt-test", "inst", &request.conversation, &[])
            .build(&provider(WireApi::Chat))
            .expect("provider request")
            .body;

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;
        let after = ChatRequestBuilder::new("gpt-test", "inst", &outcome.request.conversation, &[])
            .build(&provider(WireApi::Chat))
            .expect("provider request")
            .body;

        let before_messages = before["messages"].as_array().expect("before messages");
        let after_messages = after["messages"].as_array().expect("after messages");
        let first_tool_index = after_messages
            .iter()
            .position(|message| message["role"] == "tool")
            .expect("tool message");
        assert_eq!(
            &after_messages[..first_tool_index],
            &before_messages[..first_tool_index]
        );
        let tool_contents = after_messages[first_tool_index..]
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| message["content"].as_str().expect("tool content"))
            .collect::<Vec<_>>();
        assert_eq!(tool_contents.len(), 2);
        assert!(
            tool_contents
                .iter()
                .all(|content| content.contains("<<lha-input:"))
        );
    }

    #[tokio::test]
    async fn messages_live_zone_serialization_preserves_prefix_before_tool_result_block() {
        let request = request_with(vec![
            user("current prompt"),
            tool_call("shell"),
            structured_tool("messages live output ".repeat(5_000), None),
        ]);
        let before = MessagesRequestBuilder::new("claude-test", "inst", &request.conversation, &[])
            .build(&provider(WireApi::Messages))
            .expect("provider request")
            .body;

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;
        let after =
            MessagesRequestBuilder::new("claude-test", "inst", &outcome.request.conversation, &[])
                .build(&provider(WireApi::Messages))
                .expect("provider request")
                .body;

        let before_messages = before["messages"].as_array().expect("before messages");
        let after_messages = after["messages"].as_array().expect("after messages");
        let tool_result_index = after_messages
            .iter()
            .position(|message| {
                message["content"]
                    .as_array()
                    .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_result"))
            })
            .expect("tool result message");
        assert_eq!(
            &after_messages[..tool_result_index],
            &before_messages[..tool_result_index]
        );
        let tool_result = after_messages[tool_result_index]["content"]
            .as_array()
            .expect("content")
            .iter()
            .find(|block| block["type"] == "tool_result")
            .expect("tool result");
        assert!(
            tool_result["content"][0]["text"]
                .as_str()
                .expect("tool result text")
                .contains("<<lha-input:")
        );
    }

    #[tokio::test]
    async fn responses_live_zone_content_items_preserve_images_and_slim_text() {
        let image_url = "data:image/png;base64,abc".to_string();
        let request = request_with(vec![
            user("current prompt"),
            structured_tool(
                "Generated image".to_string(),
                Some(vec![
                    ToolResultContentItem::InputText {
                        text: "image metadata ".repeat(5_000),
                    },
                    ToolResultContentItem::InputImage {
                        image_url: image_url.clone(),
                    },
                ]),
            ),
        ]);

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;
        let body = ResponsesRequestBuilder::new("gpt-test", "inst", &outcome.request.conversation)
            .build(&provider(WireApi::Responses))
            .expect("provider request")
            .body;
        let output = body["input"][1]["output"].as_array().expect("output items");

        assert!(
            output[0]["text"]
                .as_str()
                .expect("text output")
                .contains("<<lha-input:")
        );
        assert_eq!(output[1]["image_url"], image_url);
    }

    #[tokio::test]
    async fn image_only_structured_content_items_are_skipped() {
        let request = request_with(vec![
            structured_tool(
                "image".to_string(),
                Some(vec![ToolResultContentItem::InputImage {
                    image_url: "data:image/png;base64,abc".to_string(),
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
            vec![
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::StructuredContentItems,
                    tool_name: Some("shell".to_string()),
                },
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::CurrentUserTurn,
                    tool_name: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn live_tool_results_after_latest_user_message_are_protected() {
        let text = "after user".repeat(5000);
        let request = request_with(vec![user("first"), tool_text("shell", text)]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.request.conversation, request.conversation);
        assert_eq!(outcome.metrics.candidates, 0);
        assert_eq!(outcome.metrics.slimmed, 0);
        assert_eq!(
            outcome.metrics.skipped,
            vec![
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::CurrentUserTurn,
                    tool_name: None,
                },
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::RecentAssistant,
                    tool_name: Some("shell".to_string()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn live_zone_scope_slims_same_turn_tool_output() {
        let request = request_with(vec![
            user("current prompt"),
            tool_text("shell", "live output ".repeat(5_000)),
        ]);

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;

        assert_eq!(outcome.metrics.slimmed, 1);
        let TranscriptItem::ToolResult {
            payload: ToolResultPayload::Text { output },
            ..
        } = &outcome.request.conversation[1]
        else {
            panic!("expected live tool result");
        };
        assert!(output.contains("<<lha-input:"));
        assert_eq!(outcome.metrics.refs[0].zone, CandidateZone::LiveToolOutput);
    }

    #[tokio::test]
    async fn live_zone_scope_skips_historical_tool_output_and_slims_current_tool_output() {
        let historical = "historical output ".repeat(5_000);
        let live = "live output ".repeat(5_000);
        let request = request_with(vec![
            tool_text("shell", historical.clone()),
            user("current prompt"),
            tool_text("shell", live),
        ]);

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;

        assert_eq!(
            outcome.request.conversation[0],
            tool_text("shell", historical)
        );
        let TranscriptItem::ToolResult {
            payload: ToolResultPayload::Text { output },
            ..
        } = &outcome.request.conversation[2]
        else {
            panic!("expected live tool result");
        };
        assert!(output.contains("<<lha-input:"));
        assert_eq!(outcome.metrics.slimmed, 1);
    }

    #[tokio::test]
    async fn live_zone_scope_slims_multiple_current_tool_results() {
        let request = request_with(vec![
            user("current prompt"),
            tool_text("shell", "first live output ".repeat(5_000)),
            tool_text("rg", "second live output ".repeat(5_000)),
        ]);

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;

        assert_eq!(outcome.metrics.slimmed, 2);
        assert_eq!(
            outcome
                .metrics
                .refs
                .iter()
                .map(|reference| reference.zone)
                .collect::<Vec<_>>(),
            vec![CandidateZone::LiveToolOutput, CandidateZone::LiveToolOutput]
        );
    }

    #[tokio::test]
    async fn live_zone_scope_skips_retrieve_output_and_existing_markers() {
        let request = request_with(vec![
            user("current prompt"),
            tool_text(
                INPUT_RETRIEVE_TOOL_NAME,
                "retrieved original ".repeat(5_000),
            ),
            tool_text("shell", "already <<lha-input:abcdef>> slimmed".to_string()),
        ]);

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;

        assert_eq!(outcome.request.conversation, request.conversation);
        assert_eq!(outcome.metrics.slimmed, 0);
        assert_eq!(
            outcome.metrics.skipped,
            vec![
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::CurrentUserTurn,
                    tool_name: None,
                },
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::AlreadySlimmed,
                    tool_name: Some(INPUT_RETRIEVE_TOOL_NAME.to_string()),
                },
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::AlreadySlimmed,
                    tool_name: Some("shell".to_string()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn live_zone_scope_skips_non_tool_items_and_user_text() {
        let request = request_with(vec![
            user("current prompt ".repeat(5_000).as_str()),
            reasoning("private reasoning"),
            hosted_activity(),
            tool_call("shell"),
            assistant("assistant note"),
            tool_text("shell", "live output ".repeat(5_000)),
        ]);

        let outcome =
            slim_request_with_scope(&request, InputSlimmingScope::LiveZoneToolOutputs).await;

        assert_eq!(outcome.metrics.slimmed, 1);
        assert_eq!(outcome.request.conversation[0], request.conversation[0]);
        assert!(outcome.metrics.skipped.contains(&InputSlimmingSkip {
            reason: InputSlimmingSkipReason::CurrentUserTurn,
            tool_name: None,
        }));
        assert!(outcome.metrics.skipped.contains(&InputSlimmingSkip {
            reason: InputSlimmingSkipReason::ProtectedRole,
            tool_name: None,
        }));
        assert!(outcome.metrics.skipped.contains(&InputSlimmingSkip {
            reason: InputSlimmingSkipReason::UnsupportedItem,
            tool_name: None,
        }));
        assert!(outcome.metrics.skipped.contains(&InputSlimmingSkip {
            reason: InputSlimmingSkipReason::RecentAssistant,
            tool_name: None,
        }));
    }

    #[tokio::test]
    async fn existing_marker_after_latest_user_keeps_already_slimmed_skip() {
        let request = request_with(vec![
            user("first"),
            tool_text("shell", "before <<lha-input:abcdef>> after".to_string()),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.request.conversation, request.conversation);
        assert_eq!(
            outcome.metrics.skipped,
            vec![
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::CurrentUserTurn,
                    tool_name: None,
                },
                InputSlimmingSkip {
                    reason: InputSlimmingSkipReason::AlreadySlimmed,
                    tool_name: Some("shell".to_string()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn live_zone_safety_preserves_recent_tool_result() {
        let request = request_with(vec![
            assistant("old assistant"),
            user("current prompt"),
            reasoning("private reasoning"),
            hosted_activity(),
            tool_call("shell"),
            tool_text("shell", "live output ".repeat(5_000)),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.request.conversation, request.conversation);
        assert!(outcome.metrics.skipped.contains(&InputSlimmingSkip {
            reason: InputSlimmingSkipReason::RecentAssistant,
            tool_name: Some("shell".to_string()),
        }));
    }

    #[tokio::test]
    async fn model_estimator_token_gate_is_preferred() {
        let request = request_with(vec![
            tool_text("shell", "alpha ".repeat(5_000)),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();
        let calls = AtomicUsize::new(0);
        let estimator = |request: &TurnRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            let text = serde_json::to_string(&request.conversation).ok()?;
            Some(approx_token_count(&text))
        };

        let outcome = InputSlimmer::default()
            .slim_request_with_context(
                &request,
                InputSlimmingContext {
                    store: &store,
                    turn_id: "turn",
                    estimate_request_tokens: Some(&estimator),
                    mode: InputSlimmingMode::Apply,
                    scope: InputSlimmingScope::HistoricalToolOutputs,
                    wire_api: InputSlimmingWireApi::Responses,
                    context_window: None,
                    estimated_input_tokens: None,
                },
            )
            .await
            .expect("slimming succeeds");

        assert!(calls.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            outcome.metrics.strategy_metrics[0].gate_method,
            InputSlimmingTokenGateMethod::ModelRequestEstimate
        );
        assert_eq!(outcome.metrics.token_gate_fallbacks, 0);
    }

    #[tokio::test]
    async fn token_gate_fallback_is_recorded_when_estimator_is_unavailable() {
        let request = request_with(vec![
            tool_text("shell", "alpha ".repeat(5_000)),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();
        let estimator = |_request: &TurnRequest| None;

        let outcome = InputSlimmer::default()
            .slim_request_with_context(
                &request,
                InputSlimmingContext {
                    store: &store,
                    turn_id: "turn",
                    estimate_request_tokens: Some(&estimator),
                    mode: InputSlimmingMode::Apply,
                    scope: InputSlimmingScope::HistoricalToolOutputs,
                    wire_api: InputSlimmingWireApi::Responses,
                    context_window: None,
                    estimated_input_tokens: None,
                },
            )
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.metrics.token_gate_fallbacks, 1);
        assert_eq!(
            outcome.metrics.strategy_metrics[0].gate_method,
            InputSlimmingTokenGateMethod::ApproxTextFallback
        );
    }

    #[tokio::test]
    async fn measure_only_does_not_modify_or_store() {
        let request = request_with(vec![
            tool_text("shell", "measure ".repeat(5_000)),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request_with_context(
                &request,
                InputSlimmingContext {
                    store: &store,
                    turn_id: "turn",
                    estimate_request_tokens: None,
                    mode: InputSlimmingMode::MeasureOnly,
                    scope: InputSlimmingScope::HistoricalToolOutputs,
                    wire_api: InputSlimmingWireApi::Responses,
                    context_window: None,
                    estimated_input_tokens: None,
                },
            )
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.request.conversation, request.conversation);
        assert!(!outcome.requires_retrieval_tool);
        assert_eq!(outcome.metrics.measured_only, 1);
        assert_eq!(outcome.metrics.slimmed, 0);
        assert_eq!(outcome.metrics.refs, Vec::new());
        assert_eq!(outcome.persisted_entries, Vec::new());
    }

    #[test]
    fn strategy_round_trips_through_stored_metadata() {
        for strategy in [
            InputSlimmingStrategy::JsonArraySample,
            InputSlimmingStrategy::SearchResultCompact,
            InputSlimmingStrategy::LogCompact,
            InputSlimmingStrategy::DiffCompact,
            InputSlimmingStrategy::PlainTextHeadTail,
        ] {
            let metadata = StoredInputMetadata {
                scope: InputSlimmingScope::HistoricalToolOutputs,
                strategy,
                tool_name: "shell".to_string(),
                original_tokens: 10,
                compressed_tokens: 3,
                created_turn_id: "turn-1".to_string(),
            };

            let serialized = InputSlimmingStoredInputMetadata::from(metadata.clone());
            let restored =
                StoredInputMetadata::try_from(serialized).expect("known strategy should restore");

            assert_eq!(restored, metadata);
        }
    }

    #[test]
    fn unknown_stored_strategy_is_rejected() {
        let serialized = InputSlimmingStoredInputMetadata {
            scope: InputSlimmingScope::HistoricalToolOutputs,
            strategy: "unknown".to_string(),
            tool_name: "shell".to_string(),
            original_tokens: 10,
            compressed_tokens: 3,
            created_turn_id: "turn-1".to_string(),
        };

        assert!(StoredInputMetadata::try_from(serialized).is_err());
        assert_eq!(InputSlimmingStrategy::from_str("unknown"), None);
    }

    #[test]
    fn stored_metadata_without_scope_defaults_to_historical() {
        let serialized: InputSlimmingStoredInputMetadata =
            serde_json::from_value(serde_json::json!({
                "strategy": "plain_text_head_tail",
                "tool_name": "shell",
                "original_tokens": 10,
                "compressed_tokens": 3,
                "created_turn_id": "turn-1",
            }))
            .expect("metadata without scope should deserialize");

        let restored =
            StoredInputMetadata::try_from(serialized).expect("default scope metadata restores");

        assert_eq!(
            restored,
            StoredInputMetadata {
                scope: InputSlimmingScope::HistoricalToolOutputs,
                strategy: InputSlimmingStrategy::PlainTextHeadTail,
                tool_name: "shell".to_string(),
                original_tokens: 10,
                compressed_tokens: 3,
                created_turn_id: "turn-1".to_string(),
            }
        );
    }

    #[test]
    fn live_zone_stored_metadata_round_trips() {
        let metadata = StoredInputMetadata {
            scope: InputSlimmingScope::LiveZoneToolOutputs,
            strategy: InputSlimmingStrategy::PlainTextHeadTail,
            tool_name: "shell".to_string(),
            original_tokens: 10,
            compressed_tokens: 3,
            created_turn_id: "turn-1".to_string(),
        };

        let serialized = InputSlimmingStoredInputMetadata::from(metadata.clone());
        let restored =
            StoredInputMetadata::try_from(serialized).expect("live scope metadata restores");

        assert_eq!(restored, metadata);
    }

    #[tokio::test]
    async fn token_gate_rejects_replacement_when_model_estimator_says_not_smaller() {
        let request = request_with(vec![
            tool_text("shell", "alpha ".repeat(5_000)),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();
        let estimator = |_request: &TurnRequest| Some(10_000);

        let outcome = InputSlimmer::default()
            .slim_request_with_context(
                &request,
                InputSlimmingContext {
                    store: &store,
                    turn_id: "turn",
                    estimate_request_tokens: Some(&estimator),
                    mode: InputSlimmingMode::Apply,
                    scope: InputSlimmingScope::HistoricalToolOutputs,
                    wire_api: InputSlimmingWireApi::Responses,
                    context_window: None,
                    estimated_input_tokens: None,
                },
            )
            .await
            .expect("slimming succeeds");

        assert_eq!(outcome.request.conversation, request.conversation);
        assert_eq!(outcome.metrics.slimmed, 0);
        assert!(outcome.metrics.skipped.iter().any(|skip| {
            skip.reason == InputSlimmingSkipReason::NotTokenSaving
                && skip.tool_name.as_deref() == Some("shell")
        }));
    }

    #[tokio::test]
    async fn rejected_fallback_gate_is_recorded() {
        let request = request_with(vec![
            tool_text(
                "rg",
                "src/main.rs:1:needle\nsrc/main.rs:2:needle\nsrc/main.rs:3:needle".to_string(),
            ),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();
        let estimator = |_request: &TurnRequest| None;

        let outcome = InputSlimmer::new(InputSlimmingOptions {
            min_candidate_tokens: 1,
            target_tokens: 512,
            min_saved_tokens: 128,
        })
        .slim_request_with_context(
            &request,
            InputSlimmingContext {
                store: &store,
                turn_id: "turn",
                estimate_request_tokens: Some(&estimator),
                mode: InputSlimmingMode::Apply,
                scope: InputSlimmingScope::HistoricalToolOutputs,
                wire_api: InputSlimmingWireApi::Responses,
                context_window: None,
                estimated_input_tokens: None,
            },
        )
        .await
        .expect("slimming succeeds");

        assert_eq!(outcome.metrics.slimmed, 0);
        assert_eq!(outcome.metrics.token_gate_fallbacks, 1);
        assert!(outcome.metrics.skipped.iter().any(|skip| {
            skip.reason == InputSlimmingSkipReason::NotTokenSaving
                && skip.tool_name.as_deref() == Some("rg")
        }));
    }

    #[tokio::test]
    async fn eval_retrieval_recovers_omitted_middle_needle() {
        let request = request_with(vec![
            tool_text(
                "shell",
                format!(
                    "{}\nNEEDLE_IN_OMITTED_MIDDLE\n{}",
                    "head ".repeat(5_000),
                    "tail ".repeat(5_000)
                ),
            ),
            user("next"),
        ]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");
        let hash = outcome.metrics.refs[0].hash.clone();
        let retrieval = store
            .retrieve(&hash, Some("NEEDLE_IN_OMITTED_MIDDLE"))
            .await;

        assert!(retrieval.success);
        assert_eq!(retrieval.query_matched, Some(true));
        assert!(retrieval.content.contains("NEEDLE_IN_OMITTED_MIDDLE"));
    }

    #[tokio::test]
    async fn eval_high_entropy_text_requires_marker_when_compressed() {
        let text = (0..8_000)
            .map(|idx| format!("{idx:04x}Qz9+/AaBbCcDdEeFfGgHhIiJjKkLlMmNn=="))
            .collect::<Vec<_>>()
            .join("\n");
        let request = request_with(vec![tool_text("shell", text), user("next")]);
        let store = InputSlimmingStore::default();

        let outcome = InputSlimmer::default()
            .slim_request(&request, &store)
            .await
            .expect("slimming succeeds");

        if outcome.metrics.slimmed == 0 {
            assert_eq!(outcome.request.conversation, request.conversation);
        } else if let TranscriptItem::ToolResult {
            payload: ToolResultPayload::Text { output },
            ..
        } = &outcome.request.conversation[0]
        {
            assert!(output.contains("<<lha-input:"));
            assert!(outcome.requires_retrieval_tool);
        } else {
            panic!("expected text tool result");
        }
    }

    #[test]
    fn adaptive_policy_changes_with_context_pressure() {
        let low = input_slimming_options_for_context(
            Some(100_000),
            Some(10_000),
            "shell",
            CandidateZone::HistoricalToolOutput,
        );
        let high = input_slimming_options_for_context(
            Some(100_000),
            Some(90_000),
            "shell",
            CandidateZone::HistoricalToolOutput,
        );
        let live = input_slimming_options_for_context(
            Some(100_000),
            Some(90_000),
            "shell",
            CandidateZone::LiveToolOutput,
        );

        assert!(high.min_candidate_tokens < low.min_candidate_tokens);
        assert!(high.min_saved_tokens < low.min_saved_tokens);
        assert!(live.min_candidate_tokens >= high.min_candidate_tokens);
    }
}
