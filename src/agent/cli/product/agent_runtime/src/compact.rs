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
use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::input_slimming::INPUT_RETRIEVE_TOOL_NAME;
use crate::product::agent::input_slimming::INPUT_SLIMMING_MARKER_PREFIX;
use crate::product::agent::input_slimming::InputRetrieveHandler;
use crate::product::agent::input_slimming::InputSlimmer;
use crate::product::agent::input_slimming::InputSlimmingContext;
use crate::product::agent::input_slimming::InputSlimmingMode;
use crate::product::agent::input_slimming::InputSlimmingWireApi;
use crate::product::agent::input_slimming::create_lha_input_retrieve_tool;
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
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::handlers::UPDATE_PLAN_SUCCESS_OUTPUT;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::truncate::TruncationPolicy;
use crate::product::agent::truncate::approx_token_count;
use crate::product::agent::truncate::truncate_text;
use crate::product::agent::turn_diff_tracker::TurnDiffTracker;
use crate::product::protocol::items::ContextCompactionItem;
use crate::product::protocol::items::TurnItem;
use crate::product::protocol::models::ContentItem;
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
use tokio::sync::Mutex;
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
const RETRIEVAL_AWARE_COMPACT_ADDENDUM: &str = r#"Retrieval-aware compact addendum:

You are compacting an input-slimmed transcript. Markers like <<lha-input:hash>> refer to original tool outputs available through the lha_input_retrieve(hash, query?) tool.

Retrieve marker originals when the surrounding compressed snippet is insufficient for correctness, especially for errors, stack traces, diffs, command output, test failures, or details the user referenced. Do not invent details from compressed snippets.

If you do not retrieve a marker, preserve the marker in the final summary and explain what it likely contains. The final summary must include these sections:

- Current task and decisions
- Evidence retrieved
- Unresolved retrievable markers
- Next steps"#;

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
}

struct RetrievalAwareCompactPrompt {
    request: TurnRequest,
    marker_hashes: HashSet<String>,
    durable_marker_hashes: HashSet<String>,
    slimmed_count: usize,
}

struct RetrievalAwareCompactOutput {
    summary: String,
    retrieved_hashes: HashSet<String>,
}

struct GuardedRetrievalSummary {
    summary: String,
    durable_unresolved_count: usize,
    non_durable_marker_count: usize,
}

#[derive(Deserialize)]
struct RetrievalAwareRetrieveArgs {
    hash: String,
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
        "{}\n\n{}",
        turn_context.compact_prompt(),
        RETRIEVAL_AWARE_COMPACT_ADDENDUM
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
                )
                .await
                {
                    Ok(retrieval_prompt) => {
                        retrieval_marker_hashes = retrieval_prompt.marker_hashes.clone();
                        retrieval_durable_marker_hashes =
                            retrieval_prompt.durable_marker_hashes.clone();
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

    let history_snapshot = sess.clone_history().await;
    let history_items = history_snapshot.raw_items();
    let mut summary_suffix = retrieval_summary_suffix
        .unwrap_or_else(|| get_last_assistant_message_from_turn(history_items).unwrap_or_default());
    if mode == CompactInputMode::RetrievalAwareSlimmed {
        let guarded_summary = guard_unresolved_retrieval_markers(
            summary_suffix,
            &retrieval_marker_hashes,
            &retrieval_durable_marker_hashes,
            &retrieval_retrieved_hashes,
        );
        summary_suffix = guarded_summary.summary;
        debug!(
            retrieved_marker_count = retrieval_retrieved_hashes.len(),
            durable_unresolved_marker_count = guarded_summary.durable_unresolved_count,
            non_durable_marker_count = guarded_summary.non_durable_marker_count,
            auto_compact_strategy = "retrieval_aware",
            decision = "retrieval_aware_compact_summary_guard",
        );
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
    let mut history = sess.clone_history().await;
    let compact_input = transcript_item_from_user_input(vec![UserInput::Text {
        text: format!(
            "{}\n\n{}",
            turn_context.compact_prompt(),
            RETRIEVAL_AWARE_COMPACT_ADDENDUM
        ),
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
            warn!("failed to estimate retrieval-aware compact prompt: {err}");
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
    if !marker_hashes.is_empty() {
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
    let tracker = Arc::new(Mutex::new(TurnDiffTracker::new()));

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
                handle_retrieval_aware_tool_call(
                    Arc::clone(&sess),
                    Arc::clone(&turn_context),
                    Arc::clone(&tracker),
                    request.clone(),
                )
                .await
            } else if request.tool_name == INPUT_RETRIEVE_TOOL_NAME {
                retrieval_aware_tool_error(
                    &request,
                    format!(
                        "Retrieval-aware compact already used {RETRIEVAL_AWARE_COMPACT_RETRIEVE_LIMIT} retrieval calls. Preserve any unresolved <<lha-input:...>> markers in the final summary."
                    ),
                )
            } else {
                retrieval_aware_tool_error(
                    &request,
                    format!(
                        "Retrieval-aware compact only supports the {INPUT_RETRIEVE_TOOL_NAME} tool. Preserve unresolved markers in the final summary."
                    ),
                )
            };
            let success = tool_result_success(&result);
            if success && let Some(hash) = retrieve_hash {
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
            conversation.push(result.to_transcript_item());
        }
    }

    Err(CodexErr::Stream(
        "retrieval-aware compact exceeded its internal follow-up limit".into(),
        None,
    ))
}

async fn handle_retrieval_aware_tool_call(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    tracker: Arc<Mutex<TurnDiffTracker>>,
    request: ToolCallRequest,
) -> ToolResultItem {
    let payload_outputs_custom = matches!(request.payload, ToolCallPayload::TextInput { .. });
    let payload = match request.payload.clone() {
        ToolCallPayload::JsonArguments { arguments } => ToolPayload::Function { arguments },
        ToolCallPayload::TextInput { input } => ToolPayload::Custom { input },
    };
    let invocation = ToolInvocation {
        session: sess,
        turn: turn_context,
        tracker,
        call_id: request.call_id.clone(),
        tool_name: request.tool_name.clone(),
        payload: payload.clone(),
    };
    match InputRetrieveHandler.handle(invocation).await {
        Ok(output) => output.into_tool_result(&request.call_id, &request.tool_name, &payload),
        Err(FunctionCallError::Fatal(message))
        | Err(FunctionCallError::RespondToModel(message)) => {
            retrieval_aware_tool_error_with_payload(
                &request.call_id,
                &request.tool_name,
                payload_outputs_custom,
                message,
            )
        }
        Err(FunctionCallError::MissingLocalShellCallId) => retrieval_aware_tool_error_with_payload(
            &request.call_id,
            &request.tool_name,
            payload_outputs_custom,
            FunctionCallError::MissingLocalShellCallId.to_string(),
        ),
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

fn guard_unresolved_retrieval_markers(
    mut summary: String,
    marker_hashes: &HashSet<String>,
    durable_marker_hashes: &HashSet<String>,
    retrieved_hashes: &HashSet<String>,
) -> GuardedRetrievalSummary {
    let mut non_durable_marker_count = 0usize;
    let mut candidate_hashes = marker_hashes.clone();
    candidate_hashes.extend(input_slimming_hashes_from_text(&summary));
    let mut non_durable_hashes = candidate_hashes
        .iter()
        .filter(|hash| !durable_marker_hashes.contains(*hash))
        .collect::<Vec<_>>();
    non_durable_hashes.sort();
    for hash in non_durable_hashes {
        let marker = input_slimming_marker(hash);
        if summary.contains(&marker) {
            non_durable_marker_count = non_durable_marker_count.saturating_add(1);
            summary = summary.replace(
                &marker,
                &format!(
                    "input slimming marker {hash} omitted because its original is not durably retrievable"
                ),
            );
        }
    }

    let mut missing = durable_marker_hashes
        .iter()
        .filter(|hash| !retrieved_hashes.contains(*hash))
        .filter(|hash| !summary.contains(&input_slimming_marker(hash)))
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();

    if !missing.is_empty() {
        summary.push_str("\n\nUnresolved retrievable markers:\n");
        for hash in missing {
            summary.push_str("- ");
            summary.push_str(&input_slimming_marker(&hash));
            summary.push_str(" (original tool output was not retrieved during compaction)\n");
        }
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
    fn guard_appends_only_durable_unretrieved_markers() {
        let marker_hashes = HashSet::from([
            "111111111111111111111111".to_string(),
            "222222222222222222222222".to_string(),
        ]);
        let durable_marker_hashes = HashSet::from(["111111111111111111111111".to_string()]);
        let retrieved_hashes = HashSet::new();

        let guarded = guard_unresolved_retrieval_markers(
            "Current task and decisions\n- Keep going".to_string(),
            &marker_hashes,
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
        let marker_hashes = HashSet::from(["111111111111111111111111".to_string()]);
        let durable_marker_hashes = marker_hashes.clone();
        let retrieved_hashes = marker_hashes.clone();

        let guarded = guard_unresolved_retrieval_markers(
            "Current task and decisions\n- Keep going".to_string(),
            &marker_hashes,
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
        let marker_hashes = HashSet::from(["222222222222222222222222".to_string()]);
        let durable_marker_hashes = HashSet::new();
        let retrieved_hashes = HashSet::new();

        let guarded = guard_unresolved_retrieval_markers(
            "Unresolved retrievable markers:\n- <<lha-input:222222222222222222222222>>".to_string(),
            &marker_hashes,
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
        let marker_hashes = HashSet::from(["111111111111111111111111".to_string()]);
        let durable_marker_hashes = marker_hashes.clone();
        let retrieved_hashes = HashSet::new();

        let guarded = guard_unresolved_retrieval_markers(
            "Unresolved retrievable markers:\n- <<lha-input:111111111111111111111111>>".to_string(),
            &marker_hashes,
            &durable_marker_hashes,
            &retrieved_hashes,
        );

        assert_eq!(guarded.summary.matches("<<lha-input:").count(), 1);
        assert_eq!(guarded.durable_unresolved_count, 1);
        assert_eq!(guarded.non_durable_marker_count, 0);
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
