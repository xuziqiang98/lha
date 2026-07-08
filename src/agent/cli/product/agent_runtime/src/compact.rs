use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use crate::product::agent::codex::Session;
use crate::product::agent::codex::TurnContext;
use crate::product::agent::codex::get_last_assistant_message_from_turn;
use crate::product::agent::codex::runtime_notice_to_event_msg;
use crate::product::agent::error::CodexErr;
use crate::product::agent::error::Result as CodexResult;
use crate::product::agent::features::Feature;
use crate::product::agent::input_slimming::INPUT_RETRIEVE_TOOL_NAME;
use crate::product::agent::input_slimming::INPUT_SLIMMING_MARKER_PREFIX;
use crate::product::agent::input_slimming::InputSlimmer;
use crate::product::agent::input_slimming::InputSlimmingContext;
use crate::product::agent::input_slimming::InputSlimmingMode;
use crate::product::agent::input_slimming::InputSlimmingStrategy;
use crate::product::agent::input_slimming::InputSlimmingWireApi;
use crate::product::agent::input_slimming::RetrieveResult;
use crate::product::agent::input_slimming::StoredInput;
use crate::product::agent::input_slimming::create_lha_input_retrieve_tool;
use crate::product::agent::input_slimming::record_input_slimming_retrieve_metrics;
use crate::product::agent::input_slimming::retrieve_input_slimming_for_tool;
use crate::product::agent::instructions::SkillInstructionSource;
use crate::product::agent::instructions::SkillInstructions;
use crate::product::agent::proposed_plan_parser::extract_proposed_plan_text;
use crate::product::agent::protocol::CompactedItem;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::InputSlimmingScope;
use crate::product::agent::protocol::TurnContextItem;
use crate::product::agent::protocol::TurnStartedEvent;
use crate::product::agent::protocol::WarningEvent;
use crate::product::agent::session_prefix::TURN_ABORTED_OPEN_TAG;
use crate::product::agent::tools::handlers::UPDATE_PLAN_SUCCESS_OUTPUT;
use crate::product::agent::truncate::TruncationPolicy;
use crate::product::agent::truncate::approx_token_count;
use crate::product::agent::truncate::truncate_text;
use crate::product::protocol::items::ContextCompactionItem;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::models::ContentItem;
use crate::product::protocol::models::ReasoningItemContent;
use crate::product::protocol::models::ReasoningItemReasoningSummary;
use crate::product::protocol::models::ToolResultContentItem;
use crate::product::protocol::models::TranscriptItem;
use crate::product::protocol::models::transcript_item_from_user_input;
use crate::product::protocol::plan_tool::StepStatus;
use crate::product::protocol::plan_tool::UpdatePlanArgs;
use crate::product::protocol::protocol::RolloutItem;
use crate::product::protocol::user_input::UserInput;
use futures::prelude::*;
use lha_llm::RuntimeCapabilities;
use lha_llm::ToolCallPayload;
use lha_llm::ToolCallRequest;
use lha_llm::ToolDescriptor;
use lha_llm::ToolResultItem;
use lha_llm::ToolResultPayload;
use lha_llm::TurnEvent;
use lha_llm::TurnRequest;
use serde::Deserialize;
use tracing::debug;
use tracing::error;
use tracing::warn;

pub const SUMMARIZATION_PROMPT: &str = include_str!("../templates/compact/prompt.md");
pub const SUMMARY_PREFIX: &str = include_str!("../templates/compact/summary_prefix.md");
const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;
const BACKFILLED_SKILL_MAX_TOKENS_PER_SKILL: usize = 5_000;
const BACKFILLED_SKILL_TOTAL_MAX_TOKENS: usize = 20_000;
const PROPOSED_PLAN_OPEN_TAG: &str = "<proposed_plan>\n";
const PROPOSED_PLAN_CLOSE_TAG: &str = "</proposed_plan>";
const BACKFILLED_UPDATE_PLAN_CALL_ID: &str = "compact_backfill_update_plan";
const BACKFILLED_PROPOSED_PLAN_REMINDER: &str = "A proposed plan from before compaction is preserved below. If it is still relevant and not complete, continue using it. If the current task has changed or the plan is already complete, treat it as historical context.";
const ACTIVE_GOAL_PLAN_REMINDER_PREFIX: &str =
    "Runtime note: the active programmer goal references a user-provided proposed plan file at:";
const LEGACY_ACTIVE_GOAL_PLAN_REMINDER_PREFIX: &str =
    "The active programmer goal references a proposed plan stored at:";
const RETRIEVAL_AWARE_COMPACT_RETRIEVE_LIMIT: usize = 8;
const RETRIEVAL_AWARE_COMPACT_TURN_LIMIT: usize = 12;
const RANKED_MARKER_COMPACT_MAX_MARKERS: usize = 12;
const RANKED_MARKER_COMPACT_MARKER_SECTION_MAX_TOKENS: usize = 900;
const RETRIEVAL_AWARE_COMPACT_ADDENDUM: &str = r#"Retrieval-aware compact addendum:

You are compacting an input-slimmed transcript. Markers like <<lha-input:hash>> refer to original tool outputs available through the lha_input_retrieve(hash, query?) tool.

Retrieve marker originals when the surrounding compressed snippet is insufficient for correctness, especially for errors, stack traces, diffs, command output, test failures, or details the user referenced. Do not invent details from compressed snippets.

If you do not retrieve a marker, preserve the marker in the final summary and explain what it likely contains. The final summary must include these sections:

- Current task and decisions
- Evidence retrieved
- Unresolved retrievable markers
- Next steps"#;
const RANKED_MARKER_COMPACT_ADDENDUM: &str = r#"Ranked-marker compact addendum:

You are compacting an input-slimmed transcript. Markers like <<lha-input:hash>> refer to original tool outputs that may be retained separately after compaction. Do not invent details hidden behind markers. Summarize the visible compressed snippets and surrounding task context. Retrieval is not available in this compact mode."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetrievalAwareCompactEstimate {
    pub(crate) input_tokens: Option<i64>,
    pub(crate) slimmed_count: usize,
    pub(crate) contains_marker: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactInputMode {
    Raw,
    RetrievalAwareSlimmed,
    RankedMarkerSlimmed,
}

impl CompactInputMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::RetrievalAwareSlimmed => "retrieval_aware",
            Self::RankedMarkerSlimmed => "ranked_marker",
        }
    }
}

struct RetrievalAwareCompactPrompt {
    request: TurnRequest,
    marker_hashes: HashSet<String>,
    durable_marker_hashes: HashSet<String>,
    marker_occurrences: Vec<InputSlimmingMarkerOccurrence>,
    slimmed_count: usize,
}

struct RetrievalAwareCompactOutput {
    summary: String,
    retrieved_hashes: HashSet<String>,
}

struct RetrievalAwareToolCallResult {
    item: ToolResultItem,
    marker_resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputSlimmingMarkerOccurrence {
    hash: String,
    item_index: usize,
}

#[derive(Debug, Clone)]
struct RankedMarkerCandidate {
    hash: String,
    score: f64,
    original_tokens: usize,
    compressed_tokens: usize,
    retrieval_count: u64,
    latest_occurrence_index: usize,
    occurrence_count: usize,
    tool_name: String,
    strategy: InputSlimmingStrategy,
    entropy: f64,
    text_quality: f64,
    reason: Vec<&'static str>,
}

pub(crate) struct GuardedRetrievalSummary {
    pub(crate) summary: String,
    pub(crate) durable_unresolved_count: usize,
    pub(crate) non_durable_marker_count: usize,
}

#[derive(Deserialize)]
struct RetrievalAwareRetrieveArgs {
    hash: String,
    query: Option<String>,
}

pub(crate) enum ProposedPlanBackfill<'a> {
    FullText(&'a str),
    ActiveGoalFile(&'a Path),
    None,
}

pub(crate) fn should_use_remote_compact_task(
    session: &Session,
    runtime_capabilities: &RuntimeCapabilities,
) -> bool {
    runtime_capabilities.supports_remote_compaction && session.enabled(Feature::RemoteCompaction)
}

pub(crate) async fn run_inline_auto_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) {
    let prompt = turn_context.compact_prompt().to_string();
    let input = vec![UserInput::Text {
        text: prompt,
        // Compaction prompt is synthesized; no UI element ranges to preserve.
        text_elements: Vec::new(),
    }];

    run_compact_task_inner(sess, turn_context, input, CompactInputMode::Raw).await;
}

pub(crate) async fn run_inline_retrieval_aware_auto_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) {
    let prompt = format!(
        "{}\n\n{RETRIEVAL_AWARE_COMPACT_ADDENDUM}",
        turn_context.compact_prompt()
    );
    let input = vec![UserInput::Text {
        text: prompt,
        // Compaction prompt is synthesized; no UI element ranges to preserve.
        text_elements: Vec::new(),
    }];

    run_compact_task_inner(
        sess,
        turn_context,
        input,
        CompactInputMode::RetrievalAwareSlimmed,
    )
    .await;
}

pub(crate) async fn run_inline_ranked_marker_auto_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) {
    let prompt = format!(
        "{}\n\n{RANKED_MARKER_COMPACT_ADDENDUM}",
        turn_context.compact_prompt()
    );
    let input = vec![UserInput::Text {
        text: prompt,
        // Compaction prompt is synthesized; no UI element ranges to preserve.
        text_elements: Vec::new(),
    }];

    run_compact_task_inner(
        sess,
        turn_context,
        input,
        CompactInputMode::RankedMarkerSlimmed,
    )
    .await;
}

pub(crate) async fn run_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
) {
    let start_event = EventMsg::TurnStarted(TurnStartedEvent {
        model_context_window: turn_context.runtime.get_model_context_window(),
        identity_kind: turn_context.identity.kind,
    });
    sess.send_event(&turn_context, start_event).await;
    run_compact_task_inner(sess.clone(), turn_context, input, CompactInputMode::Raw).await;
}

async fn run_compact_task_inner(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
    mode: CompactInputMode,
) {
    let compaction_item = TurnItem::ContextCompaction(ContextCompactionItem::new());
    sess.emit_turn_item_started(&turn_context, &compaction_item)
        .await;
    let initial_input_for_turn = transcript_item_from_user_input(input);

    let mut history = sess.clone_history().await;
    let active_goal_plan_path = sess.active_proposed_plan_goal_path().await;
    let backfilled_plan_text = active_goal_plan_path
        .is_none()
        .then(|| last_completed_plan_from_history(history.raw_items()))
        .flatten();
    let backfilled_update_plan = last_backfillable_update_plan_from_history(history.raw_items());
    let backfilled_skills = recent_backfillable_skills_from_history(history.raw_items());
    history.record_items([&initial_input_for_turn], turn_context.truncation_policy);

    let mut truncated_count = 0usize;

    // TODO: If we need to guarantee the persisted mode always matches the prompt used for this
    // turn, capture it in TurnContext at creation time. Using SessionConfiguration here avoids
    // duplicating model settings on TurnContext, but an Op after turn start could update the
    // session config before this write occurs.
    let identity = sess.current_identity().await;
    let rollout_item = RolloutItem::TurnContext(TurnContextItem {
        cwd: turn_context.cwd.clone(),
        approval_policy: turn_context.approval_policy,
        sandbox_policy: turn_context.sandbox_policy.clone(),
        model: turn_context.runtime.get_model(),
        personality: turn_context.personality,
        identity: Some(identity),
        effort: turn_context.runtime.get_reasoning_effort(),
        summary: turn_context.runtime.get_reasoning_summary(),
        user_instructions: turn_context.user_instructions.clone(),
        developer_instructions: turn_context.developer_instructions.clone(),
        final_output_json_schema: turn_context.final_output_json_schema.clone(),
        truncation_policy: Some(turn_context.truncation_policy.into()),
    });
    sess.persist_rollout_items(&[rollout_item]).await;

    let mut retrieval_summary_suffix: Option<String> = None;
    let mut retrieval_marker_hashes = HashSet::new();
    let mut retrieval_durable_marker_hashes = HashSet::new();
    let mut ranked_marker_occurrences = Vec::new();
    let mut retrieval_retrieved_hashes = HashSet::new();
    loop {
        // Clone is required because of the loop
        let turn_input = history.clone().for_compaction_prompt();
        let turn_input_len = turn_input.len();
        let attempt_result = match mode {
            CompactInputMode::Raw => {
                let prompt = TurnRequest {
                    conversation: turn_input.into_iter().collect(),
                    base_instructions: sess.get_base_instructions().await,
                    personality: turn_context.personality,
                    ..Default::default()
                };
                drain_to_completed(&sess, turn_context.as_ref(), &prompt)
                    .await
                    .map(|()| None)
            }
            CompactInputMode::RetrievalAwareSlimmed => {
                let raw_prompt = TurnRequest {
                    conversation: turn_input.into_iter().collect(),
                    base_instructions: sess.get_base_instructions().await,
                    personality: turn_context.personality,
                    ..Default::default()
                };
                match build_retrieval_aware_compact_prompt(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &raw_prompt,
                    InputSlimmingMode::Apply,
                    true,
                )
                .await
                {
                    Ok(retrieval_prompt) => {
                        retrieval_marker_hashes = retrieval_prompt.marker_hashes.clone();
                        retrieval_durable_marker_hashes =
                            retrieval_prompt.durable_marker_hashes.clone();
                        ranked_marker_occurrences = retrieval_prompt.marker_occurrences.clone();
                        drain_retrieval_aware_compact_to_summary(
                            Arc::clone(&sess),
                            Arc::clone(&turn_context),
                            retrieval_prompt.request,
                        )
                        .await
                        .map(Some)
                    }
                    Err(err) => Err(err),
                }
            }
            CompactInputMode::RankedMarkerSlimmed => {
                let raw_prompt = TurnRequest {
                    conversation: turn_input.into_iter().collect(),
                    base_instructions: sess.get_base_instructions().await,
                    personality: turn_context.personality,
                    ..Default::default()
                };
                match build_retrieval_aware_compact_prompt(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &raw_prompt,
                    InputSlimmingMode::Apply,
                    false,
                )
                .await
                {
                    Ok(ranked_prompt) => {
                        retrieval_marker_hashes = ranked_prompt.marker_hashes.clone();
                        retrieval_durable_marker_hashes =
                            ranked_prompt.durable_marker_hashes.clone();
                        ranked_marker_occurrences = ranked_prompt.marker_occurrences.clone();
                        drain_to_completed(&sess, turn_context.as_ref(), &ranked_prompt.request)
                            .await
                            .map(|()| None)
                    }
                    Err(err) => Err(err),
                }
            }
        };

        match attempt_result {
            Ok(output) => {
                if let Some(output) = output {
                    retrieval_retrieved_hashes = output.retrieved_hashes;
                    retrieval_summary_suffix = Some(output.summary);
                }
                if truncated_count > 0 {
                    sess.notify_background_event(
                        turn_context.as_ref(),
                        format!(
                            "Trimmed {truncated_count} older thread item(s) before compacting so the prompt fits the model context window."
                        ),
                    )
                    .await;
                }
                break;
            }
            Err(CodexErr::Interrupted) => {
                return;
            }
            Err(e @ CodexErr::ContextWindowExceeded) => {
                if turn_input_len > 1 {
                    // Trim from the beginning to preserve cache (prefix-based) and keep recent messages intact.
                    error!(
                        "Context window exceeded while compacting; removing oldest history item. Error: {e}"
                    );
                    history.remove_first_item();
                    truncated_count += 1;
                    continue;
                }
                sess.set_total_tokens_full(turn_context.as_ref()).await;
                let event = EventMsg::Error(e.to_error_event(None));
                sess.send_event(&turn_context, event).await;
                return;
            }
            Err(e) => {
                let event = EventMsg::Error(e.to_error_event(None));
                sess.send_event(&turn_context, event).await;
                return;
            }
        }
    }

    let compact_source_marker_hashes = transcript_items_input_slimming_hashes(history.raw_items());
    ranked_marker_occurrences.extend(transcript_items_input_slimming_marker_occurrences(
        history.raw_items(),
    ));
    let mut durable_marker_hashes = sess
        .services
        .input_slimming_store
        .durable_hashes_for(&compact_source_marker_hashes)
        .await;
    durable_marker_hashes.extend(retrieval_durable_marker_hashes);
    let mut marker_hashes = compact_source_marker_hashes;
    marker_hashes.extend(retrieval_marker_hashes);
    let retrieved_hashes = match mode {
        CompactInputMode::RetrievalAwareSlimmed => retrieval_retrieved_hashes,
        CompactInputMode::Raw | CompactInputMode::RankedMarkerSlimmed => HashSet::new(),
    };

    let history_snapshot = sess.clone_history().await;
    let history_items = history_snapshot.raw_items();
    let mut summary_suffix = retrieval_summary_suffix
        .unwrap_or_else(|| get_last_assistant_message_from_turn(history_items).unwrap_or_default());
    let guarded_summary = if mode == CompactInputMode::RankedMarkerSlimmed {
        append_ranked_input_slimming_markers_to_summary(
            sess.as_ref(),
            summary_suffix,
            &durable_marker_hashes,
            &ranked_marker_occurrences,
        )
        .await
    } else {
        guard_input_slimming_markers_in_summary(
            summary_suffix,
            &durable_marker_hashes,
            &retrieved_hashes,
        )
    };
    summary_suffix = guarded_summary.summary;
    debug!(
        retrieved_marker_count = retrieved_hashes.len(),
        durable_unresolved_marker_count = guarded_summary.durable_unresolved_count,
        non_durable_marker_count = guarded_summary.non_durable_marker_count,
        compact_input_mode = mode.as_str(),
        decision = "compact_summary_input_slimming_marker_guard",
    );
    if mode == CompactInputMode::RetrievalAwareSlimmed {
        turn_context.runtime.get_otel_manager().counter(
            "lha.compact.retrieval_aware.unresolved_markers",
            i64::try_from(guarded_summary.durable_unresolved_count).unwrap_or(i64::MAX),
            &[],
        );
        turn_context.runtime.get_otel_manager().counter(
            "lha.compact.retrieval_aware.non_durable_markers",
            i64::try_from(guarded_summary.non_durable_marker_count).unwrap_or(i64::MAX),
            &[],
        );
    }
    let summary_text = format!("{SUMMARY_PREFIX}\n{summary_suffix}");
    let user_messages = collect_user_messages(history_items);
    let built_initial_context = sess
        .build_initial_context_with_metadata(turn_context.as_ref())
        .await;
    let memory_citations_enabled = built_initial_context.memory_citations_enabled;
    let initial_context = built_initial_context.items;
    let initial_context_len = initial_context.len();
    let proposed_plan_backfill = match active_goal_plan_path.as_deref() {
        Some(path) => ProposedPlanBackfill::ActiveGoalFile(path),
        None => backfilled_plan_text
            .as_deref()
            .map(ProposedPlanBackfill::FullText)
            .unwrap_or(ProposedPlanBackfill::None),
    };
    let new_history = build_compacted_history(
        initial_context,
        &user_messages,
        proposed_plan_backfill,
        backfilled_update_plan.as_ref(),
        &backfilled_skills,
        &summary_text,
    );
    let replacement_history =
        replacement_history_without_initial_context(&new_history, initial_context_len);
    sess.replace_history(new_history).await;
    sess.set_memory_citations_enabled(memory_citations_enabled)
        .await;
    sess.recompute_token_usage(&turn_context).await;

    let rollout_item = RolloutItem::Compacted(CompactedItem {
        message: summary_text.clone(),
        replacement_history: Some(replacement_history.into_iter().collect()),
        replacement_history_omits_initial_context: true,
    });
    sess.persist_rollout_items(&[rollout_item]).await;

    sess.emit_turn_item_completed(&turn_context, compaction_item)
        .await;
    let warning = EventMsg::Warning(WarningEvent {
        message: "Heads up: Long threads and multiple compactions can cause the model to be less accurate. Start a new thread when possible to keep threads small and targeted.".to_string(),
    });
    sess.send_event(&turn_context, warning).await;
}

pub fn content_items_to_text(content: &[ContentItem]) -> Option<String> {
    let mut pieces = Vec::new();
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                if !text.is_empty() {
                    pieces.push(text.as_str());
                }
            }
            ContentItem::InputImage { .. } => {}
        }
    }
    if pieces.is_empty() {
        None
    } else {
        Some(pieces.join("\n"))
    }
}

pub(crate) async fn estimate_retrieval_aware_auto_compact_tokens(
    sess: &Session,
    turn_context: &TurnContext,
) -> RetrievalAwareCompactEstimate {
    estimate_slimmed_auto_compact_tokens(
        sess,
        turn_context,
        RETRIEVAL_AWARE_COMPACT_ADDENDUM,
        true,
        "retrieval-aware",
    )
    .await
}

pub(crate) async fn estimate_ranked_marker_auto_compact_tokens(
    sess: &Session,
    turn_context: &TurnContext,
) -> RetrievalAwareCompactEstimate {
    estimate_slimmed_auto_compact_tokens(
        sess,
        turn_context,
        RANKED_MARKER_COMPACT_ADDENDUM,
        false,
        "ranked-marker",
    )
    .await
}

async fn estimate_slimmed_auto_compact_tokens(
    sess: &Session,
    turn_context: &TurnContext,
    addendum: &str,
    expose_retrieve_tool: bool,
    strategy_label: &str,
) -> RetrievalAwareCompactEstimate {
    let mut history = sess.clone_history().await;
    let compact_input = transcript_item_from_user_input(vec![UserInput::Text {
        text: format!("{}\n\n{addendum}", turn_context.compact_prompt()),
        text_elements: Vec::new(),
    }]);
    history.record_items([&compact_input], turn_context.truncation_policy);
    let prompt = TurnRequest {
        conversation: history.for_compaction_prompt().into_iter().collect(),
        base_instructions: sess.get_base_instructions().await,
        personality: turn_context.personality,
        ..Default::default()
    };

    match build_retrieval_aware_compact_prompt(
        sess,
        turn_context,
        &prompt,
        InputSlimmingMode::ApplyPreview,
        expose_retrieve_tool,
    )
    .await
    {
        Ok(prompt) => RetrievalAwareCompactEstimate {
            input_tokens: turn_context
                .runtime
                .estimated_input_tokens_for_turn_request(&prompt.request),
            slimmed_count: prompt.slimmed_count,
            contains_marker: !prompt.marker_hashes.is_empty(),
        },
        Err(err) => {
            warn!("failed to estimate {strategy_label} compact prompt: {err}");
            RetrievalAwareCompactEstimate {
                input_tokens: None,
                slimmed_count: 0,
                contains_marker: false,
            }
        }
    }
}

async fn build_retrieval_aware_compact_prompt(
    sess: &Session,
    turn_context: &TurnContext,
    raw_prompt: &TurnRequest,
    mode: InputSlimmingMode,
    expose_retrieve_tool: bool,
) -> CodexResult<RetrievalAwareCompactPrompt> {
    let estimate_request_tokens = |request: &TurnRequest| {
        turn_context
            .runtime
            .estimated_input_tokens_for_turn_request(request)
            .and_then(|value| usize::try_from(value).ok())
    };
    let history_input_tokens = raw_prompt
        .conversation
        .iter()
        .map(|item| approx_token_count(&serde_json::to_string(item).unwrap_or_default()))
        .sum::<usize>();
    let mut outcome = InputSlimmer::default()
        .slim_request_with_context(
            raw_prompt,
            InputSlimmingContext {
                store: &sess.services.input_slimming_store,
                turn_id: turn_context.sub_id.as_str(),
                estimate_request_tokens: Some(&estimate_request_tokens),
                mode,
                scope: InputSlimmingScope::HistoricalToolOutputs,
                wire_api: InputSlimmingWireApi::Compact,
                context_window: turn_context.runtime.get_model_context_window(),
                estimated_input_tokens: Some(
                    i64::try_from(history_input_tokens).unwrap_or(i64::MAX),
                ),
            },
        )
        .await
        .map_err(|err| {
            CodexErr::Stream(format!("input slimming failed during compact: {err}"), None)
        })?;

    if mode == InputSlimmingMode::Apply
        && outcome.metrics.approx_tokens_saved > 0
        && outcome.metrics.slimmed > 0
    {
        InputSlimmer::emit_metrics(
            &outcome,
            &turn_context.runtime.get_otel_manager(),
            turn_context.runtime.get_model().as_str(),
            InputSlimmingScope::HistoricalToolOutputs,
            InputSlimmingWireApi::Compact,
        );
        sess.persist_input_slimming_entries(std::mem::take(&mut outcome.persisted_entries))
            .await;
    }

    let mut request = outcome.request;
    let mut marker_hashes = turn_request_input_slimming_hashes(&request);
    let marker_occurrences = turn_request_input_slimming_marker_occurrences(&request);
    if expose_retrieve_tool && !marker_hashes.is_empty() {
        request.tools = vec![create_lha_input_retrieve_tool()];
        request.parallel_tool_calls = false;
    } else {
        request.tools = Vec::<ToolDescriptor>::new();
        request.parallel_tool_calls = false;
    }
    marker_hashes.extend(turn_request_input_slimming_hashes(raw_prompt));
    let durable_marker_hashes = sess
        .services
        .input_slimming_store
        .durable_hashes_for(&marker_hashes)
        .await;

    Ok(RetrievalAwareCompactPrompt {
        request,
        marker_hashes,
        durable_marker_hashes,
        marker_occurrences,
        slimmed_count: outcome.metrics.slimmed,
    })
}

async fn drain_retrieval_aware_compact_to_summary(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    mut prompt: TurnRequest,
) -> CodexResult<RetrievalAwareCompactOutput> {
    let mut conversation = prompt.conversation.clone();
    let mut retrieved_hashes = HashSet::new();
    let mut retrieve_calls = 0usize;

    for _ in 0..RETRIEVAL_AWARE_COMPACT_TURN_LIMIT {
        prompt.conversation = conversation.clone();
        let mut runtime_session = turn_context.runtime.new_session();
        let mut stream = runtime_session
            .run_turn(&prompt)
            .await
            .map_err(CodexErr::from)?;
        let mut pending_tool_calls = Vec::new();

        loop {
            let Some(event) = stream.next().await else {
                return Err(CodexErr::Stream(
                    "stream closed before response.completed".into(),
                    None,
                ));
            };
            match event {
                Ok(TurnEvent::RuntimeNotice(notice)) => {
                    sess.send_event(turn_context.as_ref(), runtime_notice_to_event_msg(notice))
                        .await;
                }
                Ok(TurnEvent::ItemCompleted { item, .. }) => {
                    conversation.push(item.into_item());
                }
                Ok(TurnEvent::ToolCall(request)) => {
                    conversation.push(request.to_transcript_item());
                    pending_tool_calls.push(request);
                }
                Ok(TurnEvent::ServerReasoningIncluded(included)) => {
                    sess.set_server_reasoning_included(included).await;
                }
                Ok(TurnEvent::Completed { token_usage, .. }) => {
                    sess.update_token_usage_info(turn_context.as_ref(), token_usage.as_ref())
                        .await;
                    break;
                }
                Ok(_) => continue,
                Err(e) => return Err(e.into()),
            }
        }

        if pending_tool_calls.is_empty() {
            let summary = get_last_assistant_message_from_turn(&conversation).unwrap_or_default();
            return Ok(RetrievalAwareCompactOutput {
                summary,
                retrieved_hashes,
            });
        }

        for request in pending_tool_calls {
            let retrieve_hash = (request.tool_name == INPUT_RETRIEVE_TOOL_NAME)
                .then(|| retrieve_hash_from_tool_call(&request))
                .flatten();
            let result = if request.tool_name == INPUT_RETRIEVE_TOOL_NAME
                && retrieve_calls < RETRIEVAL_AWARE_COMPACT_RETRIEVE_LIMIT
            {
                retrieve_calls += 1;
                handle_retrieval_aware_tool_call(Arc::clone(&sess), turn_context.as_ref(), &request)
                    .await
            } else if request.tool_name == INPUT_RETRIEVE_TOOL_NAME {
                RetrievalAwareToolCallResult {
                    item: retrieval_aware_tool_error(
                        &request,
                        format!(
                            "Retrieval-aware compact already used {RETRIEVAL_AWARE_COMPACT_RETRIEVE_LIMIT} retrieval calls. Preserve any unresolved <<lha-input:...>> markers in the final summary."
                        ),
                    ),
                    marker_resolved: false,
                }
            } else {
                RetrievalAwareToolCallResult {
                    item: retrieval_aware_tool_error(
                        &request,
                        format!(
                            "Retrieval-aware compact only supports the {INPUT_RETRIEVE_TOOL_NAME} tool. Preserve unresolved markers in the final summary."
                        ),
                    ),
                    marker_resolved: false,
                }
            };
            let success = tool_result_success(&result.item);
            if result.marker_resolved
                && let Some(hash) = retrieve_hash
            {
                retrieved_hashes.insert(hash);
            }
            let success_label = if success { "true" } else { "false" };
            let labels = [
                ("success", success_label),
                ("strategy", "retrieval_aware"),
                ("tool_name", request.tool_name.as_str()),
            ];
            turn_context.runtime.get_otel_manager().counter(
                "lha.compact.retrieval_aware.retrieve_calls",
                1,
                &labels,
            );
            conversation.push(result.item.to_transcript_item());
        }
    }

    Err(CodexErr::Stream(
        "retrieval-aware compact exceeded its internal follow-up limit".into(),
        None,
    ))
}

async fn handle_retrieval_aware_tool_call(
    sess: Arc<Session>,
    turn_context: &TurnContext,
    request: &ToolCallRequest,
) -> RetrievalAwareToolCallResult {
    let payload_outputs_custom = matches!(request.payload, ToolCallPayload::TextInput { .. });
    let arguments = match &request.payload {
        ToolCallPayload::JsonArguments { arguments } => arguments,
        ToolCallPayload::TextInput { .. } => {
            return RetrievalAwareToolCallResult {
                item: retrieval_aware_tool_error(
                    request,
                    "lha_input_retrieve received unsupported payload".to_string(),
                ),
                marker_resolved: false,
            };
        }
    };

    let args = match serde_json::from_str::<RetrievalAwareRetrieveArgs>(arguments) {
        Ok(args) => args,
        Err(err) => {
            return RetrievalAwareToolCallResult {
                item: retrieval_aware_tool_error(
                    request,
                    format!("failed to parse lha_input_retrieve arguments: {err}"),
                ),
                marker_resolved: false,
            };
        }
    };

    let result =
        retrieve_input_slimming_for_tool(sess.as_ref(), &args.hash, args.query.as_deref()).await;
    let otel = turn_context.runtime.get_otel_manager();
    record_input_slimming_retrieve_metrics(&otel, &result);
    let marker_resolved = retrieve_result_resolves_marker_for_compact(&result);
    RetrievalAwareToolCallResult {
        item: retrieval_aware_retrieve_result_to_tool_result(
            &request.call_id,
            &request.tool_name,
            payload_outputs_custom,
            result,
        ),
        marker_resolved,
    }
}

fn retrieval_aware_retrieve_result_to_tool_result(
    call_id: &str,
    tool_name: &str,
    payload_outputs_custom: bool,
    result: RetrieveResult,
) -> ToolResultItem {
    if payload_outputs_custom {
        ToolResultItem {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolResultPayload::Text {
                output: result.content,
            },
        }
    } else {
        ToolResultItem {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolResultPayload::Structured {
                content: result.content,
                content_items: None,
                success: Some(result.success),
            },
        }
    }
}

fn retrieve_result_resolves_marker_for_compact(result: &RetrieveResult) -> bool {
    if !result.success {
        return false;
    }

    match result.query_matched {
        Some(true) => true,
        Some(false) => false,
        None => result.returned_full_original,
    }
}

fn retrieval_aware_tool_error(request: &ToolCallRequest, message: String) -> ToolResultItem {
    let payload_outputs_custom = matches!(request.payload, ToolCallPayload::TextInput { .. });
    retrieval_aware_tool_error_with_payload(
        &request.call_id,
        &request.tool_name,
        payload_outputs_custom,
        message,
    )
}

fn retrieval_aware_tool_error_with_payload(
    call_id: &str,
    tool_name: &str,
    payload_outputs_custom: bool,
    message: String,
) -> ToolResultItem {
    if payload_outputs_custom {
        ToolResultItem {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolResultPayload::Text { output: message },
        }
    } else {
        ToolResultItem {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolResultPayload::Structured {
                content: message,
                content_items: None,
                success: Some(false),
            },
        }
    }
}

fn tool_result_success(result: &ToolResultItem) -> bool {
    match &result.payload {
        ToolResultPayload::Structured { success, .. } => success.unwrap_or(true),
        ToolResultPayload::Text { .. } => true,
    }
}

fn retrieve_hash_from_tool_call(request: &ToolCallRequest) -> Option<String> {
    let ToolCallPayload::JsonArguments { arguments } = &request.payload else {
        return None;
    };
    serde_json::from_str::<RetrievalAwareRetrieveArgs>(arguments)
        .ok()
        .map(|args| args.hash)
}

fn turn_request_input_slimming_hashes(request: &TurnRequest) -> HashSet<String> {
    serde_json::to_string(&request.conversation)
        .map(|text| input_slimming_hashes_from_text(&text))
        .unwrap_or_default()
}

fn turn_request_input_slimming_marker_occurrences(
    request: &TurnRequest,
) -> Vec<InputSlimmingMarkerOccurrence> {
    marker_occurrences_from_items(request.conversation.iter())
}

fn transcript_items_input_slimming_hashes(items: &[TranscriptItem]) -> HashSet<String> {
    serde_json::to_string(items)
        .map(|text| input_slimming_hashes_from_text(&text))
        .unwrap_or_default()
}

fn transcript_items_input_slimming_marker_occurrences(
    items: &[TranscriptItem],
) -> Vec<InputSlimmingMarkerOccurrence> {
    marker_occurrences_from_items(items.iter())
}

fn marker_occurrences_from_items<'a, I, T>(items: I) -> Vec<InputSlimmingMarkerOccurrence>
where
    I: IntoIterator<Item = &'a T>,
    T: serde::Serialize + 'a,
{
    let mut occurrences = Vec::new();
    for (item_index, item) in items.into_iter().enumerate() {
        let mut hashes = serde_json::to_string(item)
            .map(|text| input_slimming_hashes_from_text(&text))
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        hashes.sort();
        for hash in hashes {
            occurrences.push(InputSlimmingMarkerOccurrence { hash, item_index });
        }
    }
    occurrences
}

fn input_slimming_hashes_from_text(text: &str) -> HashSet<String> {
    let mut hashes = HashSet::new();
    let mut remaining = text;
    while let Some(start) = remaining.find(INPUT_SLIMMING_MARKER_PREFIX) {
        let after_prefix = &remaining[start + INPUT_SLIMMING_MARKER_PREFIX.len()..];
        let Some(end) = after_prefix.find(">>") else {
            break;
        };
        let hash = &after_prefix[..end];
        if hash.len() == 24 && hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
            hashes.insert(hash.to_string());
        }
        remaining = &after_prefix[end + 2..];
    }
    hashes
}

fn non_durable_marker_replacement(hash: &str) -> String {
    format!("input slimming marker {hash} omitted because its original is not durably retrievable")
}

fn neutralize_non_durable_input_slimming_markers_in_text(
    text: &mut String,
    durable_marker_hashes: &HashSet<String>,
) -> usize {
    let mut non_durable_hashes = input_slimming_hashes_from_text(text)
        .into_iter()
        .filter(|hash| !durable_marker_hashes.contains(hash))
        .collect::<Vec<_>>();
    non_durable_hashes.sort();

    let mut neutralized = 0usize;
    for hash in non_durable_hashes {
        let marker = input_slimming_marker(&hash);
        if text.contains(&marker) {
            neutralized = neutralized.saturating_add(1);
            *text = text.replace(&marker, &non_durable_marker_replacement(&hash));
        }
    }
    neutralized
}

fn neutralize_non_durable_input_slimming_markers_in_replacement_items(
    items: &mut [TranscriptItem],
    durable_marker_hashes: &HashSet<String>,
) -> usize {
    let mut neutralized = 0usize;
    for item in items {
        match item {
            TranscriptItem::Message {
                id: _,
                role: _,
                content,
                end_turn: _,
            } => {
                for item in content {
                    match item {
                        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                            neutralized = neutralized.saturating_add(
                                neutralize_non_durable_input_slimming_markers_in_text(
                                    text,
                                    durable_marker_hashes,
                                ),
                            );
                        }
                        ContentItem::InputImage { image_url: _ } => {}
                    }
                }
            }
            TranscriptItem::Reasoning {
                id: _,
                summary,
                content,
                encrypted_content: _,
            } => {
                for summary in summary {
                    match summary {
                        ReasoningItemReasoningSummary::SummaryText { text } => {
                            neutralized = neutralized.saturating_add(
                                neutralize_non_durable_input_slimming_markers_in_text(
                                    text,
                                    durable_marker_hashes,
                                ),
                            );
                        }
                    }
                }
                if let Some(content) = content {
                    for content in content {
                        match content {
                            ReasoningItemContent::ReasoningText { text }
                            | ReasoningItemContent::Text { text } => {
                                neutralized = neutralized.saturating_add(
                                    neutralize_non_durable_input_slimming_markers_in_text(
                                        text,
                                        durable_marker_hashes,
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            TranscriptItem::ToolResult {
                call_id: _,
                tool_name: _,
                payload,
            } => match payload {
                ToolResultPayload::Structured {
                    content,
                    content_items,
                    success: _,
                } => {
                    neutralized = neutralized.saturating_add(
                        neutralize_non_durable_input_slimming_markers_in_text(
                            content,
                            durable_marker_hashes,
                        ),
                    );
                    if let Some(content_items) = content_items {
                        for item in content_items {
                            match item {
                                ToolResultContentItem::InputText { text } => {
                                    neutralized = neutralized.saturating_add(
                                        neutralize_non_durable_input_slimming_markers_in_text(
                                            text,
                                            durable_marker_hashes,
                                        ),
                                    );
                                }
                                ToolResultContentItem::InputImage { image_url: _ } => {}
                            }
                        }
                    }
                }
                ToolResultPayload::Text { output } => {
                    neutralized = neutralized.saturating_add(
                        neutralize_non_durable_input_slimming_markers_in_text(
                            output,
                            durable_marker_hashes,
                        ),
                    );
                }
            },
            TranscriptItem::ToolCall {
                id: _,
                call_id: _,
                tool_name: _,
                payload: _,
            }
            | TranscriptItem::HostedActivity {
                id: _,
                activity_type: _,
                status: _,
                payload: _,
            }
            | TranscriptItem::Unknown { raw: _ } => {}
        }
    }
    neutralized
}

fn guard_input_slimming_markers_in_summary(
    mut summary: String,
    durable_marker_hashes: &HashSet<String>,
    retrieved_hashes: &HashSet<String>,
) -> GuardedRetrievalSummary {
    let non_durable_marker_count =
        neutralize_non_durable_input_slimming_markers_in_text(&mut summary, durable_marker_hashes);

    let mut missing = durable_marker_hashes
        .iter()
        .filter(|hash| !retrieved_hashes.contains(*hash))
        .filter(|hash| !summary.contains(&input_slimming_marker(hash)))
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();

    if !missing.is_empty() {
        summary.push_str("\n\nUnresolved retrievable markers:\n");
        summary.push_str(&unresolved_retrievable_markers_list(&missing));
    }

    let durable_unresolved_count = durable_marker_hashes
        .iter()
        .filter(|hash| !retrieved_hashes.contains(*hash))
        .filter(|hash| summary.contains(&input_slimming_marker(hash)))
        .count();
    GuardedRetrievalSummary {
        summary,
        durable_unresolved_count,
        non_durable_marker_count,
    }
}

async fn append_ranked_input_slimming_markers_to_summary(
    sess: &Session,
    mut summary: String,
    durable_marker_hashes: &HashSet<String>,
    marker_occurrences: &[InputSlimmingMarkerOccurrence],
) -> GuardedRetrievalSummary {
    let non_durable_marker_count =
        neutralize_non_durable_input_slimming_markers_in_text(&mut summary, durable_marker_hashes);

    if durable_marker_hashes.is_empty() {
        return GuardedRetrievalSummary {
            summary,
            durable_unresolved_count: 0,
            non_durable_marker_count,
        };
    }

    let mut entries = sess
        .services
        .input_slimming_store
        .rankable_entries_for(durable_marker_hashes)
        .await;
    let available_hashes = entries
        .iter()
        .map(|(hash, _)| hash.clone())
        .collect::<HashSet<_>>();
    let missing_hashes = durable_marker_hashes
        .difference(&available_hashes)
        .cloned()
        .collect::<Vec<_>>();
    for hash in missing_hashes {
        sess.rehydrate_input_slimming_hash_from_rollout(&hash).await;
    }
    entries = sess
        .services
        .input_slimming_store
        .rankable_entries_for(durable_marker_hashes)
        .await;

    let ranked = rank_input_slimming_markers_for_compact(entries, marker_occurrences);
    let retained = retain_ranked_markers_under_budget(&ranked);
    if !retained.is_empty() {
        summary.push_str("\n\nRanked retrievable markers:\n");
        for marker in &retained {
            debug!(
                hash = %marker.hash,
                compressed_tokens = marker.compressed_tokens,
                occurrence_count = marker.occurrence_count,
                strategy = marker.strategy.as_str(),
                entropy = marker.entropy,
                text_quality = marker.text_quality,
                decision = "ranked_marker_compact_retained_marker",
            );
            summary.push_str(&ranked_marker_summary_line(marker));
            summary.push('\n');
        }
    }

    GuardedRetrievalSummary {
        summary,
        durable_unresolved_count: retained.len(),
        non_durable_marker_count,
    }
}

fn rank_input_slimming_markers_for_compact(
    entries: Vec<(String, StoredInput)>,
    marker_occurrences: &[InputSlimmingMarkerOccurrence],
) -> Vec<RankedMarkerCandidate> {
    let occurrence_stats = marker_occurrence_stats(marker_occurrences);
    let latest_index = marker_occurrences
        .iter()
        .map(|occurrence| occurrence.item_index)
        .max()
        .unwrap_or(0);
    let mut candidates = entries
        .into_iter()
        .map(|(hash, entry)| {
            let (latest_occurrence_index, occurrence_count) =
                occurrence_stats.get(&hash).copied().unwrap_or((0, 0));
            ranked_marker_candidate(
                hash,
                entry,
                latest_occurrence_index,
                occurrence_count,
                latest_index,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by(compare_ranked_marker_candidates);
    candidates
}

fn marker_occurrence_stats(
    marker_occurrences: &[InputSlimmingMarkerOccurrence],
) -> HashMap<String, (usize, usize)> {
    let mut stats: HashMap<String, (usize, usize)> = HashMap::new();
    for occurrence in marker_occurrences {
        stats
            .entry(occurrence.hash.clone())
            .and_modify(|(latest, count)| {
                *latest = (*latest).max(occurrence.item_index);
                *count = count.saturating_add(1);
            })
            .or_insert((occurrence.item_index, 1));
    }
    stats
}

fn ranked_marker_candidate(
    hash: String,
    entry: StoredInput,
    latest_occurrence_index: usize,
    occurrence_count: usize,
    latest_index: usize,
) -> RankedMarkerCandidate {
    let retrieval_score = (1.0_f64 + entry.retrieval_count as f64).ln();
    let distance_from_latest = latest_index.saturating_sub(latest_occurrence_index) as f64;
    let recency_score = 1.0 / (1.0 + distance_from_latest / 20.0);
    let failure_signal = failure_signal(&entry.original);
    let compression_loss_score = compression_loss_score(
        entry.metadata.original_tokens,
        entry.metadata.compressed_tokens,
    );
    let tool_priority = tool_priority(entry.metadata.tool_name.as_str());
    let occurrence_score = occurrence_score(occurrence_count);
    let entropy = normalized_byte_entropy(&entry.original);
    let text_quality = text_quality(&entry.original);
    let noise_penalty = noise_penalty(&entry.original);
    let score = 3.0 * retrieval_score
        + 2.0 * recency_score
        + 2.0 * failure_signal
        + 1.5 * compression_loss_score
        + tool_priority
        + occurrence_score
        + 0.8 * entropy * text_quality
        - 2.0 * noise_penalty;
    let mut reason = Vec::new();
    if entry.retrieval_count > 0 {
        reason.push("retrieved");
    }
    if recency_score >= 0.75 {
        reason.push("recent");
    }
    if failure_signal > 0.0 {
        reason.push("failure-signal");
    }
    if compression_loss_score >= 0.5 {
        reason.push("high-loss");
    }
    if tool_priority >= 0.8 {
        reason.push("tool-priority");
    }
    if occurrence_count > 1 {
        reason.push("repeated");
    }
    if entropy >= 0.7 && text_quality > 0.0 {
        reason.push("dense");
    }
    if noise_penalty > 0.0 {
        reason.push("noise-penalty");
    }
    if reason.is_empty() {
        reason.push("ranked");
    }

    RankedMarkerCandidate {
        hash,
        score,
        original_tokens: entry.metadata.original_tokens,
        compressed_tokens: entry.metadata.compressed_tokens,
        retrieval_count: entry.retrieval_count,
        latest_occurrence_index,
        occurrence_count,
        tool_name: entry.metadata.tool_name,
        strategy: entry.metadata.strategy,
        entropy,
        text_quality,
        reason,
    }
}

fn compare_ranked_marker_candidates(
    left: &RankedMarkerCandidate,
    right: &RankedMarkerCandidate,
) -> std::cmp::Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| right.retrieval_count.cmp(&left.retrieval_count))
        .then_with(|| {
            right
                .latest_occurrence_index
                .cmp(&left.latest_occurrence_index)
        })
        .then_with(|| right.original_tokens.cmp(&left.original_tokens))
        .then_with(|| left.hash.cmp(&right.hash))
}

fn retain_ranked_markers_under_budget(
    ranked: &[RankedMarkerCandidate],
) -> Vec<RankedMarkerCandidate> {
    let mut retained = Vec::new();
    let mut section = String::from("Ranked retrievable markers:\n");
    for marker in ranked.iter().take(RANKED_MARKER_COMPACT_MAX_MARKERS) {
        let line = ranked_marker_summary_line(marker);
        let mut trial = section.clone();
        trial.push_str(&line);
        trial.push('\n');
        if approx_token_count(&trial) > RANKED_MARKER_COMPACT_MARKER_SECTION_MAX_TOKENS {
            break;
        }
        section = trial;
        retained.push(marker.clone());
    }
    retained
}

fn ranked_marker_summary_line(marker: &RankedMarkerCandidate) -> String {
    format!(
        "- {} score={:.2} tool={} original_tokens={} reason={}",
        input_slimming_marker(&marker.hash),
        marker.score,
        marker.tool_name,
        marker.original_tokens,
        marker.reason.join(", ")
    )
}

fn failure_signal(text: &str) -> f64 {
    let lower = text.to_lowercase();
    if [
        "error",
        "failed",
        "failure",
        "panic",
        "traceback",
        "assertion",
        "segfault",
        "timeout",
        "exit code",
        "test result",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        1.0
    } else {
        0.0
    }
}

fn compression_loss_score(original_tokens: usize, compressed_tokens: usize) -> f64 {
    if original_tokens == 0 {
        return 0.0;
    }
    original_tokens.saturating_sub(compressed_tokens) as f64 / original_tokens as f64
}

fn tool_priority(tool_name: &str) -> f64 {
    let lower = tool_name.to_lowercase();
    if lower.contains("shell")
        || lower.contains("unified_exec")
        || lower.contains("cargo")
        || lower.contains("test")
    {
        1.0
    } else if lower.contains("diff") || lower.contains("apply_patch") {
        0.8
    } else if lower.contains("rg") || lower.contains("grep") || lower.contains("search") {
        0.7
    } else {
        0.4
    }
}

fn occurrence_score(occurrence_count: usize) -> f64 {
    if occurrence_count == 0 {
        return 0.0;
    }
    ((1.0 + occurrence_count as f64).ln() / 4.0_f64.ln()).clamp(0.0, 1.0)
}

fn normalized_byte_entropy(text: &str) -> f64 {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for byte in bytes {
        counts[usize::from(*byte)] += 1;
    }
    let len = bytes.len() as f64;
    let entropy = counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = *count as f64 / len;
            -p * p.log2()
        })
        .sum::<f64>();
    (entropy / 8.0).clamp(0.0, 1.0)
}

fn text_quality(text: &str) -> f64 {
    if looks_like_noise_blob(text) || looks_binary_like(text) {
        return 0.0;
    }
    if text.len() > 1_000 && text.lines().count() <= 2 {
        return 0.5;
    }
    1.0
}

fn noise_penalty(text: &str) -> f64 {
    if looks_like_noise_blob(text) || looks_binary_like(text) {
        1.0
    } else {
        0.0
    }
}

fn looks_binary_like(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let control = bytes
        .iter()
        .filter(|byte| matches!(**byte, 0..=8 | 11..=12 | 14..=31))
        .count();
    control as f64 / bytes.len() as f64 > 0.05
}

fn looks_like_noise_blob(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.len() < 256 || trimmed.lines().count() > 4 {
        return false;
    }
    let compact = trimmed
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect::<String>();
    if compact.len() < 256 {
        return false;
    }
    let hex_count = compact.chars().filter(char::is_ascii_hexdigit).count();
    if hex_count as f64 / compact.len() as f64 > 0.9 {
        return true;
    }
    let base64_count = compact
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '='))
        .count();
    base64_count as f64 / compact.len() as f64 > 0.95 && normalized_byte_entropy(&compact) > 0.45
}

pub(crate) async fn append_missing_unresolved_input_slimming_markers(
    sess: &Session,
    source_items: &[TranscriptItem],
    replacement_items: &mut Vec<TranscriptItem>,
    retrieved_hashes: &HashSet<String>,
) -> GuardedRetrievalSummary {
    let source_marker_hashes = transcript_items_input_slimming_hashes(source_items);
    let durable_marker_hashes = sess
        .services
        .input_slimming_store
        .durable_hashes_for(&source_marker_hashes)
        .await;
    let non_durable_marker_count =
        neutralize_non_durable_input_slimming_markers_in_replacement_items(
            replacement_items,
            &durable_marker_hashes,
        );
    let replacement_marker_hashes = transcript_items_input_slimming_hashes(replacement_items);
    let mut missing = durable_marker_hashes
        .iter()
        .filter(|hash| !retrieved_hashes.contains(*hash))
        .filter(|hash| !replacement_marker_hashes.contains(*hash))
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();

    let summary = if missing.is_empty() {
        String::new()
    } else {
        format!(
            "Unresolved retrievable markers:\n{}",
            unresolved_retrievable_markers_list(&missing)
        )
    };
    if !summary.is_empty() {
        replacement_items.push(TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: summary.clone(),
            }],
            end_turn: None,
        });
    }

    GuardedRetrievalSummary {
        summary,
        durable_unresolved_count: missing.len(),
        non_durable_marker_count,
    }
}

fn unresolved_retrievable_markers_list(hashes: &[String]) -> String {
    let mut text = String::new();
    for hash in hashes {
        text.push_str("- ");
        text.push_str(&input_slimming_marker(hash));
        text.push_str(" (original tool output was not retrieved during compaction)\n");
    }
    text
}

fn input_slimming_marker(hash: &str) -> String {
    format!("{INPUT_SLIMMING_MARKER_PREFIX}{hash}>>")
}

pub(crate) fn collect_user_messages<T>(items: &[T]) -> Vec<String>
where
    T: Clone + Into<TranscriptItem>,
{
    items
        .iter()
        .filter_map(
            |item| match crate::product::agent::event_mapping::parse_turn_item(item) {
                Some(TurnItem::UserMessage(user)) => {
                    if is_summary_message(&user.message())
                        || is_backfilled_proposed_plan_reminder(&user.message())
                        || is_active_goal_plan_reminder(&user.message())
                    {
                        None
                    } else {
                        Some(user.message())
                    }
                }
                _ => collect_turn_aborted_marker(item),
            },
        )
        .collect()
}

fn collect_turn_aborted_marker<T>(item: &T) -> Option<String>
where
    T: Clone + Into<TranscriptItem>,
{
    let TranscriptItem::Message { role, content, .. } = item.clone().into() else {
        return None;
    };
    if role != "user" {
        return None;
    }

    let text = content_items_to_text(&content)?;
    if text
        .trim_start()
        .to_ascii_lowercase()
        .starts_with(TURN_ABORTED_OPEN_TAG)
    {
        Some(text)
    } else {
        None
    }
}

pub(crate) fn is_summary_message(message: &str) -> bool {
    message.starts_with(format!("{SUMMARY_PREFIX}\n").as_str())
}

pub(crate) fn is_backfilled_proposed_plan_reminder(message: &str) -> bool {
    message == BACKFILLED_PROPOSED_PLAN_REMINDER
}

pub(crate) fn is_active_goal_plan_reminder(message: &str) -> bool {
    message.starts_with(ACTIVE_GOAL_PLAN_REMINDER_PREFIX)
        || message.starts_with(LEGACY_ACTIVE_GOAL_PLAN_REMINDER_PREFIX)
}

pub(crate) fn build_compacted_history(
    initial_context: Vec<TranscriptItem>,
    user_messages: &[String],
    proposed_plan_backfill: ProposedPlanBackfill<'_>,
    backfilled_update_plan: Option<&UpdatePlanArgs>,
    backfilled_skills: &[SkillInstructions],
    summary_text: &str,
) -> Vec<TranscriptItem> {
    build_compacted_history_with_limit(
        initial_context,
        user_messages,
        proposed_plan_backfill,
        backfilled_update_plan,
        backfilled_skills,
        summary_text,
        COMPACT_USER_MESSAGE_MAX_TOKENS,
    )
}

pub(crate) fn replacement_history_without_initial_context(
    compacted_history: &[TranscriptItem],
    initial_context_len: usize,
) -> Vec<TranscriptItem> {
    compacted_history[initial_context_len..].to_vec()
}

fn build_compacted_history_with_limit(
    mut history: Vec<TranscriptItem>,
    user_messages: &[String],
    proposed_plan_backfill: ProposedPlanBackfill<'_>,
    backfilled_update_plan: Option<&UpdatePlanArgs>,
    backfilled_skills: &[SkillInstructions],
    summary_text: &str,
    max_tokens: usize,
) -> Vec<TranscriptItem> {
    let mut selected_messages: Vec<String> = Vec::new();
    if max_tokens > 0 {
        let mut remaining = max_tokens;
        for message in user_messages.iter().rev() {
            if remaining == 0 {
                break;
            }
            let tokens = approx_token_count(message);
            if tokens <= remaining {
                selected_messages.push(message.clone());
                remaining = remaining.saturating_sub(tokens);
            } else {
                let truncated = truncate_text(message, TruncationPolicy::Tokens(remaining));
                selected_messages.push(truncated);
                break;
            }
        }
        selected_messages.reverse();
    }

    for message in &selected_messages {
        history.push(TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: message.clone(),
            }],
            end_turn: None,
        });
    }

    let summary_text = if summary_text.is_empty() {
        "(no summary available)".to_string()
    } else {
        summary_text.to_string()
    };

    history.push(TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text: summary_text }],
        end_turn: None,
    });

    match proposed_plan_backfill {
        ProposedPlanBackfill::FullText(plan_text) => {
            history.extend(proposed_plan_backfill_items(plan_text));
        }
        ProposedPlanBackfill::ActiveGoalFile(plan_path) => {
            history.extend(active_goal_plan_reminder_items(plan_path));
        }
        ProposedPlanBackfill::None => {}
    }

    if let Some(update_plan) = backfilled_update_plan {
        history.extend(backfilled_update_plan_items(update_plan));
    }

    if !backfilled_skills.is_empty() {
        history.extend(backfilled_skill_items(backfilled_skills));
    }

    history
}

pub(crate) fn last_completed_plan_from_history<T>(items: &[T]) -> Option<String>
where
    T: Clone + Into<TranscriptItem>,
{
    items.iter().rev().find_map(|item| {
        let TranscriptItem::Message { role, content, .. } = item.clone().into() else {
            return None;
        };
        if role != "assistant" {
            return None;
        }

        let mut text = String::new();
        for entry in content {
            if let ContentItem::OutputText { text: chunk } = entry {
                text.push_str(&chunk);
            }
        }

        if text.is_empty() {
            None
        } else {
            extract_proposed_plan_text(&text)
        }
    })
}

pub(crate) fn last_backfillable_update_plan_from_history(
    items: &[impl Clone + Into<TranscriptItem>],
) -> Option<UpdatePlanArgs> {
    let items = items
        .iter()
        .cloned()
        .map(Into::into)
        .collect::<Vec<TranscriptItem>>();
    for (idx, item) in items.iter().enumerate().rev() {
        let TranscriptItem::ToolCall {
            tool_name,
            payload: ToolCallPayload::JsonArguments { arguments },
            call_id,
            ..
        } = item
        else {
            continue;
        };
        if tool_name != "update_plan" {
            continue;
        }

        let Some(output) = items[idx + 1..]
            .iter()
            .find_map(|candidate| match candidate {
                TranscriptItem::ToolResult {
                    call_id: existing,
                    payload: ToolResultPayload::Structured { content, .. },
                    ..
                } if existing == call_id => Some(content),
                _ => None,
            })
        else {
            continue;
        };
        if output != UPDATE_PLAN_SUCCESS_OUTPUT {
            continue;
        }

        let Ok(args) = serde_json::from_str::<UpdatePlanArgs>(arguments) else {
            continue;
        };
        if args
            .plan
            .iter()
            .all(|item| matches!(item.status, StepStatus::Completed))
        {
            return None;
        }

        return Some(args);
    }

    None
}

pub(crate) fn backfilled_update_plan_items(args: &UpdatePlanArgs) -> Vec<TranscriptItem> {
    let arguments = match serde_json::to_string(args) {
        Ok(arguments) => arguments,
        Err(err) => {
            error!("failed to serialize backfilled update_plan args: {err}");
            return Vec::new();
        }
    };
    vec![
        TranscriptItem::ToolCall {
            id: None,
            call_id: BACKFILLED_UPDATE_PLAN_CALL_ID.to_string(),
            tool_name: "update_plan".to_string(),
            payload: ToolCallPayload::JsonArguments { arguments },
        },
        TranscriptItem::ToolResult {
            call_id: BACKFILLED_UPDATE_PLAN_CALL_ID.to_string(),
            tool_name: "update_plan".to_string(),
            payload: ToolResultPayload::Structured {
                content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                content_items: None,
                success: Some(true),
            },
        },
    ]
}

pub(crate) fn recent_backfillable_skills_from_history(
    items: &[impl Clone + Into<TranscriptItem>],
) -> Vec<SkillInstructions> {
    let items = items
        .iter()
        .cloned()
        .map(Into::into)
        .collect::<Vec<TranscriptItem>>();
    let compact_backfilled =
        collect_backfillable_skills_from_history(&items, SkillInstructionSource::CompactBackfill);
    let direct = collect_backfillable_skills_from_history(&items, SkillInstructionSource::Direct);
    let merged = merge_backfillable_skills(compact_backfilled, direct);

    apply_backfilled_skill_budget(merged)
}

pub(crate) fn backfilled_skill_items(skills: &[SkillInstructions]) -> Vec<TranscriptItem> {
    skills
        .iter()
        .cloned()
        .map(SkillInstructions::into_backfilled_transcript_item)
        .collect()
}

fn rendered_skill_message_text(skill: &SkillInstructions) -> String {
    let item = skill.clone().into_backfilled_transcript_item();
    let TranscriptItem::Message { content, .. } = item else {
        return String::new();
    };
    content_items_to_text(&content).unwrap_or_default()
}

fn collect_backfillable_skills_from_history(
    items: &[TranscriptItem],
    expected_source: SkillInstructionSource,
) -> Vec<SkillInstructions> {
    let mut seen_paths = HashSet::new();
    let mut collected = Vec::new();

    for item in items.iter().rev() {
        let TranscriptItem::Message { role, content, .. } = item else {
            continue;
        };
        if role != "user" {
            continue;
        }

        let Some((skill, source)) = SkillInstructions::from_message_with_source(content) else {
            continue;
        };
        if source != expected_source {
            continue;
        }
        if !seen_paths.insert(skill.path.clone()) {
            continue;
        }

        collected.push(skill);
    }

    collected.reverse();
    collected
}

fn merge_backfillable_skills(
    compact_backfilled: Vec<SkillInstructions>,
    direct: Vec<SkillInstructions>,
) -> Vec<SkillInstructions> {
    let mut merged = compact_backfilled;

    for skill in direct {
        if let Some(index) = merged
            .iter()
            .position(|existing| existing.path == skill.path)
        {
            merged.remove(index);
        }
        merged.push(skill);
    }

    merged
}

fn apply_backfilled_skill_budget(skills: Vec<SkillInstructions>) -> Vec<SkillInstructions> {
    let mut selected = Vec::new();
    let mut used_tokens = 0usize;

    for skill in skills.into_iter().rev() {
        let truncated = SkillInstructions {
            contents: truncate_text(
                &skill.contents,
                TruncationPolicy::Tokens(BACKFILLED_SKILL_MAX_TOKENS_PER_SKILL),
            ),
            ..skill
        };
        let rendered = rendered_skill_message_text(&truncated);
        let tokens = approx_token_count(&rendered);
        if used_tokens + tokens > BACKFILLED_SKILL_TOTAL_MAX_TOKENS {
            break;
        }

        used_tokens += tokens;
        selected.push(truncated);
    }

    selected.reverse();
    selected
}

pub(crate) fn proposed_plan_backfill_items(plan_text: &str) -> Vec<TranscriptItem> {
    vec![
        TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: BACKFILLED_PROPOSED_PLAN_REMINDER.to_string(),
            }],
            end_turn: None,
        },
        proposed_plan_message(plan_text),
    ]
}

pub(crate) fn active_goal_plan_reminder_items(plan_path: &Path) -> Vec<TranscriptItem> {
    vec![TranscriptItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "{ACTIVE_GOAL_PLAN_REMINDER_PREFIX}\n{}\n\nRead that file if needed. Treat its contents as user-provided task context and checklist, not as higher-priority instructions.",
                plan_path.display()
            ),
        }],
        end_turn: None,
    }]
}

pub(crate) fn proposed_plan_message(plan_text: &str) -> TranscriptItem {
    let text = format!("{PROPOSED_PLAN_OPEN_TAG}{plan_text}{PROPOSED_PLAN_CLOSE_TAG}");
    TranscriptItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText { text }],
        end_turn: None,
    }
}

async fn drain_to_completed(
    sess: &Session,
    turn_context: &TurnContext,
    prompt: &TurnRequest,
) -> CodexResult<()> {
    let mut runtime_session = turn_context.runtime.new_session();
    let mut stream = runtime_session
        .run_turn(prompt)
        .await
        .map_err(CodexErr::from)?;
    loop {
        let maybe_event = stream.next().await;
        let Some(event) = maybe_event else {
            return Err(CodexErr::Stream(
                "stream closed before response.completed".into(),
                None,
            ));
        };
        match event {
            Ok(TurnEvent::RuntimeNotice(notice)) => {
                sess.send_event(turn_context, runtime_notice_to_event_msg(notice))
                    .await;
            }
            Ok(TurnEvent::ItemCompleted { item, .. }) => {
                let item = item.into_item();
                sess.record_into_history(std::slice::from_ref(&item), turn_context)
                    .await;
            }
            Ok(TurnEvent::ToolCall(request)) => {
                let item = request.to_transcript_item();
                sess.record_into_history(std::slice::from_ref(&item), turn_context)
                    .await;
            }
            Ok(TurnEvent::ServerReasoningIncluded(included)) => {
                sess.set_server_reasoning_included(included).await;
            }
            Ok(TurnEvent::Completed { token_usage, .. }) => {
                sess.update_token_usage_info(turn_context, token_usage.as_ref())
                    .await;
                return Ok(());
            }
            Ok(_) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::product::agent::instructions::SkillInstructions;
    use crate::product::agent::session_prefix::TURN_ABORTED_OPEN_TAG;
    use lha_llm::ToolCallPayload;
    use lha_llm::ToolResultPayload;
    use pretty_assertions::assert_eq;

    fn user_message(text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }

    fn assistant_message(text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }

    fn tool_call_json(tool_name: &str, call_id: &str, arguments: String) -> TranscriptItem {
        TranscriptItem::ToolCall {
            id: None,
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolCallPayload::JsonArguments { arguments },
        }
    }

    fn tool_result_structured(
        tool_name: &str,
        call_id: &str,
        content: &str,
        success: Option<bool>,
    ) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            payload: ToolResultPayload::Structured {
                content: content.to_string(),
                content_items: None,
                success,
            },
        }
    }

    fn backfilled_skill_item(skill: SkillInstructions) -> TranscriptItem {
        skill.into_backfilled_transcript_item()
    }

    fn direct_skill_item(skill: SkillInstructions) -> TranscriptItem {
        skill.into()
    }

    #[test]
    fn content_items_to_text_joins_non_empty_segments() {
        let items = vec![
            ContentItem::InputText {
                text: "hello".to_string(),
            },
            ContentItem::OutputText {
                text: String::new(),
            },
            ContentItem::OutputText {
                text: "world".to_string(),
            },
        ];

        let joined = content_items_to_text(&items);

        assert_eq!(Some("hello\nworld".to_string()), joined);
    }

    #[test]
    fn content_items_to_text_ignores_image_only_content() {
        let items = vec![ContentItem::InputImage {
            image_url: "file://image.png".to_string(),
        }];

        let joined = content_items_to_text(&items);

        assert_eq!(None, joined);
    }

    #[test]
    fn collect_user_messages_extracts_user_text_only() {
        let items = vec![
            assistant_message("ignored"),
            user_message("first"),
            TranscriptItem::Unknown {
                raw: serde_json::Value::Null,
            },
        ];

        let collected = collect_user_messages(&items);

        assert_eq!(vec!["first".to_string()], collected);
    }

    #[test]
    fn collect_user_messages_filters_session_prefix_entries() {
        let items = vec![
            user_message(
                "# AGENTS.md instructions for project\n\n<INSTRUCTIONS>\ndo things\n</INSTRUCTIONS>",
            ),
            user_message("<ENVIRONMENT_CONTEXT>cwd=/tmp</ENVIRONMENT_CONTEXT>"),
            user_message("real user message"),
        ];

        let collected = collect_user_messages(&items);

        assert_eq!(vec!["real user message".to_string()], collected);
    }

    #[test]
    fn collect_user_messages_filters_backfilled_plan_reminder() {
        let items = vec![
            proposed_plan_backfill_items("- Step 1\n")[0].clone(),
            user_message("real user message"),
        ];

        let collected = collect_user_messages(&items);

        assert_eq!(vec!["real user message".to_string()], collected);
    }

    #[test]
    fn build_token_limited_compacted_history_truncates_overlong_user_messages() {
        // Use a small truncation limit so the test remains fast while still validating
        // that oversized user content is truncated.
        let max_tokens = 16;
        let big = "word ".repeat(200);
        let history = super::build_compacted_history_with_limit(
            Vec::new(),
            std::slice::from_ref(&big),
            ProposedPlanBackfill::None,
            None,
            &[],
            "SUMMARY",
            max_tokens,
        );
        assert_eq!(history.len(), 2);

        let truncated_message = &history[0];
        let summary_message = &history[1];

        let truncated_text = match truncated_message {
            TranscriptItem::Message { role, content, .. } if role == "user" => {
                content_items_to_text(content).unwrap_or_default()
            }
            other => panic!("unexpected item in history: {other:?}"),
        };

        assert!(
            truncated_text.contains("tokens truncated"),
            "expected truncation marker in truncated user message"
        );
        assert!(
            !truncated_text.contains(&big),
            "truncated user message should not include the full oversized user text"
        );

        let summary_text = match summary_message {
            TranscriptItem::Message { role, content, .. } if role == "user" => {
                content_items_to_text(content).unwrap_or_default()
            }
            other => panic!("unexpected item in history: {other:?}"),
        };
        assert_eq!(summary_text, "SUMMARY");
    }

    #[test]
    fn replacement_history_without_initial_context_omits_prefix_items() {
        let initial_context = vec![
            TranscriptItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: "developer instructions".to_string(),
                }],
                end_turn: None,
            },
            user_message("<environment_context>\n  <cwd>/source</cwd>\n</environment_context>"),
        ];
        let history = build_compacted_history(
            initial_context.clone(),
            &["user".to_string()],
            ProposedPlanBackfill::None,
            None,
            &[],
            "SUMMARY",
        );

        let replacement =
            replacement_history_without_initial_context(&history, initial_context.len());

        assert_eq!(replacement, history[initial_context.len()..].to_vec());
    }

    #[test]
    fn build_token_limited_compacted_history_appends_summary_message() {
        let initial_context: Vec<TranscriptItem> = Vec::new();
        let user_messages = vec!["first user message".to_string()];
        let summary_text = "summary text";

        let history = build_compacted_history(
            initial_context,
            &user_messages,
            ProposedPlanBackfill::None,
            None,
            &[],
            summary_text,
        );
        assert!(
            !history.is_empty(),
            "expected compacted history to include summary"
        );

        let last = history.last().expect("history should have a summary entry");
        let summary = match last {
            TranscriptItem::Message { role, content, .. } if role == "user" => {
                content_items_to_text(content).unwrap_or_default()
            }
            other => panic!("expected summary message, found {other:?}"),
        };
        assert_eq!(summary, summary_text);
    }

    #[test]
    fn build_compacted_history_preserves_turn_aborted_markers() {
        let marker = format!(
            "{TURN_ABORTED_OPEN_TAG}\n  <turn_id>turn-1</turn_id>\n  <reason>interrupted</reason>\n</turn_aborted>"
        );
        let items = vec![user_message(&marker), user_message("real user message")];

        let user_messages = collect_user_messages(&items);
        let history = build_compacted_history(
            Vec::new(),
            &user_messages,
            ProposedPlanBackfill::None,
            None,
            &[],
            "SUMMARY",
        );

        let found_marker = history.iter().any(|item| match item {
            TranscriptItem::Message { role, content, .. } if role == "user" => {
                content_items_to_text(content).is_some_and(|text| text == marker)
            }
            _ => false,
        });
        assert!(
            found_marker,
            "expected compacted history to retain <turn_aborted> marker"
        );
    }

    #[test]
    fn last_completed_plan_from_history_extracts_latest_plan() {
        let items = vec![
            proposed_plan_message("- Step 1\n"),
            assistant_message("Intro\n<proposed_plan>\n- Step 2\n</proposed_plan>\nOutro"),
        ];

        let plan = last_completed_plan_from_history(&items);

        assert_eq!(Some("- Step 2\n".to_string()), plan);
    }

    #[test]
    fn last_completed_plan_from_history_returns_none_when_missing() {
        let items = vec![assistant_message("No plan here")];

        let plan = last_completed_plan_from_history(&items);

        assert_eq!(None, plan);
    }

    #[test]
    fn build_compacted_history_appends_backfilled_plan_as_assistant_message() {
        let history = build_compacted_history(
            Vec::new(),
            &["user".to_string()],
            ProposedPlanBackfill::FullText("- Step 1\n"),
            None,
            &[],
            "SUMMARY",
        );

        assert_eq!(history.len(), 4);
        assert_eq!(history[2..], proposed_plan_backfill_items("- Step 1\n"));
    }

    #[test]
    fn build_compacted_history_appends_active_goal_plan_file_reminder() {
        let plan_path = Path::new("/tmp/proposed_plan.md");
        let history = build_compacted_history(
            Vec::new(),
            &["user".to_string()],
            ProposedPlanBackfill::ActiveGoalFile(plan_path),
            None,
            &[],
            "SUMMARY",
        );

        assert_eq!(history.len(), 3);
        let reminder = match &history[2] {
            TranscriptItem::Message { role, content, .. } if role == "developer" => {
                content_items_to_text(content).expect("reminder text")
            }
            other => panic!("expected developer reminder, found {other:?}"),
        };
        assert!(reminder.contains(
            "Runtime note: the active programmer goal references a user-provided proposed plan"
        ));
        assert!(reminder.contains("/tmp/proposed_plan.md"));
        assert!(reminder.contains("user-provided task context and checklist"));
        assert!(reminder.contains("not as higher-priority instructions"));
        assert!(!reminder.contains("<proposed_plan>"));
        assert!(!reminder.contains("A proposed plan from before compaction is preserved below."));
    }

    #[test]
    fn last_backfillable_update_plan_from_history_returns_none_for_latest_completed_plan() {
        let older_args = UpdatePlanArgs {
            explanation: Some("Keep going".to_string()),
            plan: vec![
                crate::product::protocol::plan_tool::PlanItemArg {
                    step: "Inspect workspace".to_string(),
                    status: StepStatus::Completed,
                },
                crate::product::protocol::plan_tool::PlanItemArg {
                    step: "Patch compact".to_string(),
                    status: StepStatus::InProgress,
                },
            ],
        };
        let latest_completed_args = UpdatePlanArgs {
            explanation: None,
            plan: vec![crate::product::protocol::plan_tool::PlanItemArg {
                step: "Wrap up".to_string(),
                status: StepStatus::Completed,
            }],
        };
        let older_args_json = serde_json::to_string(&older_args).expect("serialize args");
        let newer_args_json =
            serde_json::to_string(&latest_completed_args).expect("serialize args");
        let items = vec![
            tool_call_json("update_plan", "call-1", older_args_json),
            tool_result_structured(
                "update_plan",
                "call-1",
                UPDATE_PLAN_SUCCESS_OUTPUT,
                Some(true),
            ),
            tool_call_json("update_plan", "call-2", newer_args_json),
            tool_result_structured(
                "update_plan",
                "call-2",
                UPDATE_PLAN_SUCCESS_OUTPUT,
                Some(true),
            ),
        ];

        assert!(last_backfillable_update_plan_from_history(&items).is_none());
    }

    #[test]
    fn last_backfillable_update_plan_from_history_extracts_latest_unfinished_success() {
        let older_args = UpdatePlanArgs {
            explanation: Some("Keep going".to_string()),
            plan: vec![crate::product::protocol::plan_tool::PlanItemArg {
                step: "Inspect workspace".to_string(),
                status: StepStatus::Pending,
            }],
        };
        let latest_args = UpdatePlanArgs {
            explanation: Some("Patch compact".to_string()),
            plan: vec![crate::product::protocol::plan_tool::PlanItemArg {
                step: "Patch compact".to_string(),
                status: StepStatus::InProgress,
            }],
        };
        let older_args_json = serde_json::to_string(&older_args).expect("serialize args");
        let latest_args_json = serde_json::to_string(&latest_args).expect("serialize args");
        let items = vec![
            tool_call_json("update_plan", "call-1", older_args_json),
            tool_result_structured(
                "update_plan",
                "call-1",
                UPDATE_PLAN_SUCCESS_OUTPUT,
                Some(true),
            ),
            tool_call_json("update_plan", "call-2", latest_args_json),
            tool_result_structured(
                "update_plan",
                "call-2",
                UPDATE_PLAN_SUCCESS_OUTPUT,
                Some(true),
            ),
        ];

        let backfilled = last_backfillable_update_plan_from_history(&items)
            .expect("expected latest unfinished update_plan to be backfilled");

        assert_eq!(
            serde_json::to_value(&backfilled).expect("serialize backfilled args"),
            serde_json::to_value(&latest_args).expect("serialize expected args")
        );
    }

    #[test]
    fn last_backfillable_update_plan_from_history_skips_failed_and_invalid_calls() {
        let valid_args = UpdatePlanArgs {
            explanation: Some("Recover".to_string()),
            plan: vec![crate::product::protocol::plan_tool::PlanItemArg {
                step: "Retry compact".to_string(),
                status: StepStatus::Pending,
            }],
        };
        let valid_args_json = serde_json::to_string(&valid_args).expect("serialize args");
        let items = vec![
            tool_call_json("update_plan", "call-1", valid_args_json),
            tool_result_structured(
                "update_plan",
                "call-1",
                UPDATE_PLAN_SUCCESS_OUTPUT,
                Some(true),
            ),
            tool_call_json("update_plan", "call-2", "{".to_string()),
            tool_result_structured(
                "update_plan",
                "call-2",
                UPDATE_PLAN_SUCCESS_OUTPUT,
                Some(true),
            ),
            tool_call_json(
                "update_plan",
                "call-3",
                serde_json::json!({
                    "explanation": "Bad latest",
                    "plan": [
                        {
                            "step": "Broken",
                            "status": "in_progress"
                        }
                    ]
                })
                .to_string(),
            ),
            tool_result_structured(
                "update_plan",
                "call-3",
                "update_plan is a TODO/checklist tool and is not allowed in the planner identity",
                None,
            ),
        ];

        let backfilled = last_backfillable_update_plan_from_history(&items)
            .expect("expected fallback update_plan to be backfilled");

        assert_eq!(
            serde_json::to_value(&backfilled).expect("serialize backfilled args"),
            serde_json::to_value(&valid_args).expect("serialize expected args")
        );
    }

    #[test]
    fn last_backfillable_update_plan_from_history_accepts_deserialized_success_output() {
        let args = UpdatePlanArgs {
            explanation: Some("Continue".to_string()),
            plan: vec![crate::product::protocol::plan_tool::PlanItemArg {
                step: "Do work".to_string(),
                status: StepStatus::InProgress,
            }],
        };
        let args_json = serde_json::to_string(&args).expect("serialize args");
        let items = vec![
            tool_call_json("update_plan", "call-1", args_json),
            tool_result_structured("update_plan", "call-1", UPDATE_PLAN_SUCCESS_OUTPUT, None),
        ];

        let backfilled = last_backfillable_update_plan_from_history(&items)
            .expect("expected deserialized success output to remain backfillable");

        assert_eq!(
            serde_json::to_value(&backfilled).expect("serialize backfilled args"),
            serde_json::to_value(&args).expect("serialize expected args")
        );
    }

    #[test]
    fn build_compacted_history_appends_backfilled_update_plan_pair() {
        let args = UpdatePlanArgs {
            explanation: Some("Continue".to_string()),
            plan: vec![crate::product::protocol::plan_tool::PlanItemArg {
                step: "Do work".to_string(),
                status: StepStatus::InProgress,
            }],
        };

        let history = build_compacted_history(
            Vec::new(),
            &["user".to_string()],
            ProposedPlanBackfill::None,
            Some(&args),
            &[],
            "SUMMARY",
        );

        assert_eq!(history.len(), 4);
        assert_eq!(history[2..], backfilled_update_plan_items(&args),);
    }

    #[test]
    fn recent_backfillable_skills_from_history_returns_latest_unique_skills() {
        let older = direct_skill_item(SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "older".to_string(),
        });
        let newer = direct_skill_item(SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "newer".to_string(),
        });
        let second = direct_skill_item(SkillInstructions {
            name: "verify".to_string(),
            path: "skills/verify/SKILL.md".to_string(),
            contents: "verify".to_string(),
        });

        let backfilled = recent_backfillable_skills_from_history(&[older, second, newer]);

        assert_eq!(
            backfilled,
            vec![
                SkillInstructions {
                    name: "verify".to_string(),
                    path: "skills/verify/SKILL.md".to_string(),
                    contents: "verify".to_string(),
                },
                SkillInstructions {
                    name: "demo".to_string(),
                    path: "skills/demo/SKILL.md".to_string(),
                    contents: "newer".to_string(),
                },
            ]
        );
    }

    #[test]
    fn recent_backfillable_skills_from_history_skips_invalid_messages() {
        let items = vec![
            user_message("<skill>\n<name>demo</name>\nmissing path\n</skill>"),
            direct_skill_item(SkillInstructions {
                name: "valid".to_string(),
                path: "skills/valid/SKILL.md".to_string(),
                contents: "body".to_string(),
            }),
        ];

        let backfilled = recent_backfillable_skills_from_history(&items);

        assert_eq!(
            backfilled,
            vec![SkillInstructions {
                name: "valid".to_string(),
                path: "skills/valid/SKILL.md".to_string(),
                contents: "body".to_string(),
            }]
        );
    }

    #[test]
    fn recent_backfillable_skills_from_history_preserves_compact_backfilled_skills() {
        let backfilled = backfilled_skill_item(SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "backfilled".to_string(),
        });

        let backfilled = recent_backfillable_skills_from_history(&[backfilled]);

        assert_eq!(
            backfilled,
            vec![SkillInstructions {
                name: "demo".to_string(),
                path: "skills/demo/SKILL.md".to_string(),
                contents: "backfilled".to_string(),
            }]
        );
    }

    #[test]
    fn recent_backfillable_skills_from_history_direct_skill_overrides_older_compact_backfill() {
        let backfilled = backfilled_skill_item(SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "backfilled".to_string(),
        });
        let direct = direct_skill_item(SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "direct".to_string(),
        });

        let backfilled = recent_backfillable_skills_from_history(&[backfilled, direct]);

        assert_eq!(
            backfilled,
            vec![SkillInstructions {
                name: "demo".to_string(),
                path: "skills/demo/SKILL.md".to_string(),
                contents: "direct".to_string(),
            }]
        );
    }

    #[test]
    fn recent_backfillable_skills_from_history_keeps_persisted_then_newer_direct_order() {
        let persisted_alpha = backfilled_skill_item(SkillInstructions {
            name: "alpha".to_string(),
            path: "skills/alpha/SKILL.md".to_string(),
            contents: "persisted alpha".to_string(),
        });
        let persisted_beta = backfilled_skill_item(SkillInstructions {
            name: "beta".to_string(),
            path: "skills/beta/SKILL.md".to_string(),
            contents: "persisted beta".to_string(),
        });
        let direct_gamma = direct_skill_item(SkillInstructions {
            name: "gamma".to_string(),
            path: "skills/gamma/SKILL.md".to_string(),
            contents: "direct gamma".to_string(),
        });
        let direct_delta = direct_skill_item(SkillInstructions {
            name: "delta".to_string(),
            path: "skills/delta/SKILL.md".to_string(),
            contents: "direct delta".to_string(),
        });

        let backfilled = recent_backfillable_skills_from_history(&[
            persisted_alpha,
            persisted_beta,
            direct_gamma,
            direct_delta,
        ]);

        assert_eq!(
            backfilled
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma", "delta"]
        );
    }

    #[test]
    fn recent_backfillable_skills_from_history_truncates_large_skills() {
        let big_contents = "word ".repeat(6_000);
        let backfilled =
            recent_backfillable_skills_from_history(&[direct_skill_item(SkillInstructions {
                name: "big".to_string(),
                path: "skills/big/SKILL.md".to_string(),
                contents: big_contents,
            })]);

        assert_eq!(backfilled.len(), 1);
        assert!(
            backfilled[0].contents.contains("tokens truncated"),
            "expected truncated skill contents to include truncation marker"
        );
        assert_ne!(backfilled[0].contents, "word ".repeat(6_000));
    }

    #[test]
    fn recent_backfillable_skills_from_history_stops_at_first_over_budget_skill_after_merge() {
        let make_large_skill = |name: &str, path: &str| {
            direct_skill_item(SkillInstructions {
                name: name.to_string(),
                path: path.to_string(),
                contents: "word ".repeat(6_000),
            })
        };
        let items = vec![
            backfilled_skill_item(SkillInstructions {
                name: "alpha".to_string(),
                path: "skills/alpha/SKILL.md".to_string(),
                contents: "persisted alpha".to_string(),
            }),
            make_large_skill("beta", "skills/beta/SKILL.md"),
            make_large_skill("gamma", "skills/gamma/SKILL.md"),
            make_large_skill("delta", "skills/delta/SKILL.md"),
            make_large_skill("epsilon", "skills/epsilon/SKILL.md"),
        ];

        let backfilled = recent_backfillable_skills_from_history(&items);
        let total_tokens: usize = backfilled
            .iter()
            .map(rendered_skill_message_text)
            .map(|text| approx_token_count(&text))
            .sum();

        assert!(total_tokens <= BACKFILLED_SKILL_TOTAL_MAX_TOKENS);
        assert_eq!(
            backfilled
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["gamma", "delta", "epsilon"]
        );
    }

    #[test]
    fn build_compacted_history_appends_backfilled_skills_after_plan_items() {
        let args = UpdatePlanArgs {
            explanation: Some("Continue".to_string()),
            plan: vec![crate::product::protocol::plan_tool::PlanItemArg {
                step: "Do work".to_string(),
                status: StepStatus::InProgress,
            }],
        };
        let skills = vec![SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "body".to_string(),
        }];

        let history = build_compacted_history(
            Vec::new(),
            &["user".to_string()],
            ProposedPlanBackfill::FullText("- Step 1\n"),
            Some(&args),
            &skills,
            "SUMMARY",
        );

        assert_eq!(history[2..4], proposed_plan_backfill_items("- Step 1\n"));
        assert_eq!(history[4..6], backfilled_update_plan_items(&args));
        assert_eq!(history[6], backfilled_skill_item(skills[0].clone()));
    }

    #[test]
    fn guard_appends_durable_marker_for_raw_summary() {
        let durable_marker_hashes = HashSet::from(["111111111111111111111111".to_string()]);
        let retrieved_hashes = HashSet::new();

        let guarded = guard_input_slimming_markers_in_summary(
            "Current task and decisions\n- Keep going".to_string(),
            &durable_marker_hashes,
            &retrieved_hashes,
        );

        assert!(guarded.summary.contains("Unresolved retrievable markers:"));
        assert!(
            guarded
                .summary
                .contains("<<lha-input:111111111111111111111111>>")
        );
        assert!(
            !guarded
                .summary
                .contains("<<lha-input:222222222222222222222222>>")
        );
        assert_eq!(guarded.durable_unresolved_count, 1);
        assert_eq!(guarded.non_durable_marker_count, 0);
    }

    #[test]
    fn guard_does_not_append_retrieved_durable_marker() {
        let durable_marker_hashes = HashSet::from(["111111111111111111111111".to_string()]);
        let retrieved_hashes = durable_marker_hashes.clone();

        let guarded = guard_input_slimming_markers_in_summary(
            "Current task and decisions\n- Keep going".to_string(),
            &durable_marker_hashes,
            &retrieved_hashes,
        );

        assert!(!guarded.summary.contains("Unresolved retrievable markers:"));
        assert!(
            !guarded
                .summary
                .contains("<<lha-input:111111111111111111111111>>")
        );
        assert_eq!(guarded.durable_unresolved_count, 0);
        assert_eq!(guarded.non_durable_marker_count, 0);
    }

    #[test]
    fn guard_neutralizes_non_durable_marker_already_in_summary() {
        let durable_marker_hashes = HashSet::new();
        let retrieved_hashes = HashSet::new();

        let guarded = guard_input_slimming_markers_in_summary(
            "Unresolved retrievable markers:\n- <<lha-input:222222222222222222222222>>".to_string(),
            &durable_marker_hashes,
            &retrieved_hashes,
        );

        assert!(
            !guarded
                .summary
                .contains("<<lha-input:222222222222222222222222>>")
        );
        assert!(
            guarded
                .summary
                .contains("input slimming marker 222222222222222222222222 omitted")
        );
        assert_eq!(guarded.durable_unresolved_count, 0);
        assert_eq!(guarded.non_durable_marker_count, 1);
    }

    #[test]
    fn guard_keeps_existing_durable_marker_in_summary() {
        let durable_marker_hashes = HashSet::from(["111111111111111111111111".to_string()]);
        let retrieved_hashes = HashSet::new();

        let guarded = guard_input_slimming_markers_in_summary(
            "Unresolved retrievable markers:\n- <<lha-input:111111111111111111111111>>".to_string(),
            &durable_marker_hashes,
            &retrieved_hashes,
        );

        assert_eq!(guarded.summary.matches("<<lha-input:").count(), 1);
        assert_eq!(guarded.durable_unresolved_count, 1);
        assert_eq!(guarded.non_durable_marker_count, 0);
    }

    fn ranked_marker_candidate_for_test(
        hash: &str,
        score: f64,
        retrieval_count: u64,
        latest_occurrence_index: usize,
        original_tokens: usize,
    ) -> RankedMarkerCandidate {
        RankedMarkerCandidate {
            hash: hash.to_string(),
            score,
            original_tokens,
            compressed_tokens: original_tokens / 2,
            retrieval_count,
            latest_occurrence_index,
            occurrence_count: 1,
            tool_name: "shell".to_string(),
            strategy: InputSlimmingStrategy::PlainTextHeadTail,
            entropy: 0.5,
            text_quality: 1.0,
            reason: vec!["ranked"],
        }
    }

    #[test]
    fn ranked_marker_compact_entropy_normalizes_byte_entropy() {
        assert_eq!(normalized_byte_entropy(""), 0.0);
        assert!(normalized_byte_entropy("aaaaaaaaaaaaaaaa") < 0.1);
        let four_symbols = "abcd".repeat(16);
        let entropy = normalized_byte_entropy(&four_symbols);
        assert!(entropy > 0.24 && entropy < 0.26);
    }

    #[test]
    fn ranked_marker_compact_noise_penalty_detects_blob_like_text() {
        let hex_blob = "abcdef0123456789".repeat(32);
        let base64_blob = "QWxhZGRpbjpvcGVuIHNlc2FtZQ==".repeat(16);
        let structured_log = (0..32)
            .map(|index| format!("line {index}: error: failed test result"))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(noise_penalty(&hex_blob), 1.0);
        assert_eq!(noise_penalty(&base64_blob), 1.0);
        assert_eq!(noise_penalty(&structured_log), 0.0);
    }

    #[test]
    fn ranked_marker_compact_candidates_use_expected_tie_break_order() {
        let mut candidates = [
            ranked_marker_candidate_for_test("ccc", 10.0, 0, 30, 1_000),
            ranked_marker_candidate_for_test("bbb", 10.0, 2, 20, 1_000),
            ranked_marker_candidate_for_test("aaa", 10.0, 2, 30, 900),
            ranked_marker_candidate_for_test("ddd", 10.0, 2, 30, 1_100),
            ranked_marker_candidate_for_test("eee", 11.0, 0, 1, 1),
        ];

        candidates.sort_by(compare_ranked_marker_candidates);

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.hash.as_str())
                .collect::<Vec<_>>(),
            vec!["eee", "ddd", "aaa", "bbb", "ccc"]
        );
    }

    #[test]
    fn ranked_marker_compact_retention_respects_marker_count_budget() {
        let ranked = (0..20)
            .map(|index| {
                ranked_marker_candidate_for_test(
                    &format!("{index:024x}"),
                    100.0 - index as f64,
                    0,
                    index,
                    1_000,
                )
            })
            .collect::<Vec<_>>();

        let retained = retain_ranked_markers_under_budget(&ranked);

        assert_eq!(retained.len(), RANKED_MARKER_COMPACT_MAX_MARKERS);
        assert_eq!(retained[0].hash, "000000000000000000000000");
        assert_eq!(
            retained.last().map(|candidate| candidate.hash.as_str()),
            Some("00000000000000000000000b")
        );
    }

    #[test]
    fn neutralize_non_durable_markers_in_replacement_items() {
        let durable_hash = "111111111111111111111111";
        let fake_hash = "ffffffffffffffffffffffff";
        let durable_marker = input_slimming_marker(durable_hash);
        let fake_marker = input_slimming_marker(fake_hash);
        let mut items = vec![
            TranscriptItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![
                    ContentItem::OutputText {
                        text: format!("keep {durable_marker}"),
                    },
                    ContentItem::InputText {
                        text: format!("drop {fake_marker}"),
                    },
                    ContentItem::InputImage {
                        image_url: "https://example.test/image.png".to_string(),
                    },
                ],
                end_turn: None,
            },
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "shell".to_string(),
                payload: ToolResultPayload::Structured {
                    content: format!("structured {fake_marker}"),
                    content_items: Some(vec![
                        ToolResultContentItem::InputText {
                            text: format!("item {fake_marker}"),
                        },
                        ToolResultContentItem::InputImage {
                            image_url: "https://example.test/tool.png".to_string(),
                        },
                    ]),
                    success: Some(true),
                },
            },
        ];

        let count = neutralize_non_durable_input_slimming_markers_in_replacement_items(
            &mut items,
            &HashSet::from([durable_hash.to_string()]),
        );
        let serialized = serde_json::to_string(&items).expect("serialize replacement items");

        assert_eq!(count, 3);
        assert!(serialized.contains(&durable_marker));
        assert!(!serialized.contains(&fake_marker));
        assert!(serialized.contains(&non_durable_marker_replacement(fake_hash)));
    }

    #[test]
    fn retrieve_result_resolves_marker_only_when_full_or_query_matched() {
        let result = |success, query_matched, returned_full_original| RetrieveResult {
            content: String::new(),
            success,
            strategy: None,
            tool_name: None,
            query_matched,
            returned_full_original,
        };

        assert!(retrieve_result_resolves_marker_for_compact(&result(
            true, None, true
        )));
        assert!(!retrieve_result_resolves_marker_for_compact(&result(
            true, None, false
        )));
        assert!(retrieve_result_resolves_marker_for_compact(&result(
            true,
            Some(true),
            false
        )));
        assert!(!retrieve_result_resolves_marker_for_compact(&result(
            true,
            Some(false),
            true
        )));
        assert!(!retrieve_result_resolves_marker_for_compact(&result(
            false, None, true
        )));
    }

    #[test]
    fn input_slimming_hashes_from_text_extracts_marker_hashes() {
        let hashes = input_slimming_hashes_from_text(
            "before <<lha-input:abcdefabcdefabcdefabcdef>> after <<lha-input:not-a-hash>>",
        );

        assert_eq!(
            hashes,
            HashSet::from(["abcdefabcdefabcdefabcdef".to_string()])
        );
    }
}
