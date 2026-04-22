use std::collections::HashSet;
use std::sync::Arc;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::codex::get_last_assistant_message_from_turn;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::features::Feature;
use crate::instructions::SkillInstructionSource;
use crate::instructions::SkillInstructions;
use crate::proposed_plan_parser::extract_proposed_plan_text;
use crate::protocol::CompactedItem;
use crate::protocol::EventMsg;
use crate::protocol::TurnContextItem;
use crate::protocol::TurnStartedEvent;
use crate::protocol::WarningEvent;
use crate::session_prefix::TURN_ABORTED_OPEN_TAG;
use crate::tools::handlers::UPDATE_PLAN_SUCCESS_OUTPUT;
use crate::truncate::TruncationPolicy;
use crate::truncate::approx_token_count;
use crate::truncate::truncate_text;
use codex_llm::RuntimeCapabilities;
use codex_llm::TurnEvent;
use codex_llm::TurnRequest;
use codex_protocol::items::ContextCompactionItem;
use codex_protocol::items::TurnItem;
use codex_protocol::legacy_transcript::ConversationItem;
use codex_protocol::legacy_transcript::FunctionCallOutputPayload;
use codex_protocol::legacy_transcript::ResponseInputItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::TranscriptItem;
use codex_protocol::models::transcript_item_from_user_input;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::user_input::UserInput;
use futures::prelude::*;
use tracing::error;

pub const SUMMARIZATION_PROMPT: &str = include_str!("../templates/compact/prompt.md");
pub const SUMMARY_PREFIX: &str = include_str!("../templates/compact/summary_prefix.md");
const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;
const BACKFILLED_SKILL_MAX_TOKENS_PER_SKILL: usize = 5_000;
const BACKFILLED_SKILL_TOTAL_MAX_TOKENS: usize = 20_000;
const PROPOSED_PLAN_OPEN_TAG: &str = "<proposed_plan>\n";
const PROPOSED_PLAN_CLOSE_TAG: &str = "</proposed_plan>";
const BACKFILLED_UPDATE_PLAN_CALL_ID: &str = "compact_backfill_update_plan";

fn legacy_response_input_from_user_input(input: Vec<UserInput>) -> ResponseInputItem {
    match transcript_item_from_user_input(input) {
        TranscriptItem::Message { role, content, .. } => {
            ResponseInputItem::Message { role, content }
        }
        _ => unreachable!("user input always maps to a message transcript item"),
    }
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

    run_compact_task_inner(sess, turn_context, input).await;
}

pub(crate) async fn run_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
) {
    let start_event = EventMsg::TurnStarted(TurnStartedEvent {
        model_context_window: turn_context.runtime.get_model_context_window(),
        collaboration_mode_kind: turn_context.collaboration_mode.mode,
    });
    sess.send_event(&turn_context, start_event).await;
    run_compact_task_inner(sess.clone(), turn_context, input).await;
}

async fn run_compact_task_inner(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
) {
    let compaction_item = TurnItem::ContextCompaction(ContextCompactionItem::new());
    sess.emit_turn_item_started(&turn_context, &compaction_item)
        .await;
    let initial_input_for_turn: ResponseInputItem = legacy_response_input_from_user_input(input);

    let mut history = sess.clone_history().await;
    let (backfilled_plan_text, backfilled_update_plan, backfilled_skills) =
        if sess.enabled(Feature::BackfillCompactPlanContext) {
            (
                last_completed_plan_from_history(history.raw_items()),
                last_backfillable_update_plan_from_history(history.raw_items()),
                recent_backfillable_skills_from_history(history.raw_items()),
            )
        } else {
            (None, None, Vec::new())
        };
    history.record_items(
        &[initial_input_for_turn.into()],
        turn_context.truncation_policy,
    );

    let mut truncated_count = 0usize;

    // TODO: If we need to guarantee the persisted mode always matches the prompt used for this
    // turn, capture it in TurnContext at creation time. Using SessionConfiguration here avoids
    // duplicating model settings on TurnContext, but an Op after turn start could update the
    // session config before this write occurs.
    let collaboration_mode = sess.current_collaboration_mode().await;
    let rollout_item = RolloutItem::TurnContext(TurnContextItem {
        cwd: turn_context.cwd.clone(),
        approval_policy: turn_context.approval_policy,
        sandbox_policy: turn_context.sandbox_policy.clone(),
        model: turn_context.runtime.get_model(),
        personality: turn_context.personality,
        collaboration_mode: Some(collaboration_mode),
        effort: turn_context.runtime.get_reasoning_effort(),
        summary: turn_context.runtime.get_reasoning_summary(),
        user_instructions: turn_context.user_instructions.clone(),
        developer_instructions: turn_context.developer_instructions.clone(),
        final_output_json_schema: turn_context.final_output_json_schema.clone(),
        truncation_policy: Some(turn_context.truncation_policy.into()),
    });
    sess.persist_rollout_items(&[rollout_item]).await;

    loop {
        // Clone is required because of the loop
        let turn_input = history.clone().for_compaction_prompt();
        let turn_input_len = turn_input.len();
        let prompt = TurnRequest {
            conversation: turn_input.into_iter().map(Into::into).collect(),
            base_instructions: sess.get_base_instructions().await,
            personality: turn_context.personality,
            ..Default::default()
        };
        let attempt_result = drain_to_completed(&sess, turn_context.as_ref(), &prompt).await;

        match attempt_result {
            Ok(()) => {
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
    let summary_suffix = get_last_assistant_message_from_turn(history_items).unwrap_or_default();
    let summary_text = format!("{SUMMARY_PREFIX}\n{summary_suffix}");
    let user_messages = collect_user_messages(history_items);
    let initial_context = sess.build_initial_context(turn_context.as_ref()).await;
    let initial_context_len = initial_context.len();
    let mut new_history = build_compacted_history(
        initial_context,
        &user_messages,
        backfilled_plan_text.as_deref(),
        backfilled_update_plan.as_ref(),
        &backfilled_skills,
        &summary_text,
    );
    let ghost_snapshots: Vec<ConversationItem> = history_items
        .iter()
        .filter(|item| matches!(item, ConversationItem::GhostSnapshot { .. }))
        .cloned()
        .collect();
    new_history.extend(ghost_snapshots);
    let replacement_history =
        replacement_history_without_initial_context(&new_history, initial_context_len);
    sess.replace_history(new_history).await;
    sess.recompute_token_usage(&turn_context).await;

    let rollout_item = RolloutItem::Compacted(CompactedItem {
        message: summary_text.clone(),
        replacement_history: Some(replacement_history.into_iter().map(Into::into).collect()),
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

pub(crate) fn collect_user_messages(items: &[ConversationItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match crate::event_mapping::parse_turn_item(item) {
            Some(TurnItem::UserMessage(user)) => {
                if is_summary_message(&user.message()) {
                    None
                } else {
                    Some(user.message())
                }
            }
            _ => collect_turn_aborted_marker(item),
        })
        .collect()
}

fn collect_turn_aborted_marker(item: &ConversationItem) -> Option<String> {
    let ConversationItem::Message { role, content, .. } = item else {
        return None;
    };
    if role != "user" {
        return None;
    }

    let text = content_items_to_text(content)?;
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

pub(crate) fn build_compacted_history(
    initial_context: Vec<ConversationItem>,
    user_messages: &[String],
    backfilled_plan_text: Option<&str>,
    backfilled_update_plan: Option<&UpdatePlanArgs>,
    backfilled_skills: &[SkillInstructions],
    summary_text: &str,
) -> Vec<ConversationItem> {
    build_compacted_history_with_limit(
        initial_context,
        user_messages,
        backfilled_plan_text,
        backfilled_update_plan,
        backfilled_skills,
        summary_text,
        COMPACT_USER_MESSAGE_MAX_TOKENS,
    )
}

pub(crate) fn replacement_history_without_initial_context(
    compacted_history: &[ConversationItem],
    initial_context_len: usize,
) -> Vec<ConversationItem> {
    compacted_history[initial_context_len..].to_vec()
}

fn build_compacted_history_with_limit(
    mut history: Vec<ConversationItem>,
    user_messages: &[String],
    backfilled_plan_text: Option<&str>,
    backfilled_update_plan: Option<&UpdatePlanArgs>,
    backfilled_skills: &[SkillInstructions],
    summary_text: &str,
    max_tokens: usize,
) -> Vec<ConversationItem> {
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
        history.push(ConversationItem::Message {
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

    history.push(ConversationItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text: summary_text }],
        end_turn: None,
    });

    if let Some(plan_text) = backfilled_plan_text {
        history.push(proposed_plan_message(plan_text));
    }

    if let Some(update_plan) = backfilled_update_plan {
        history.extend(backfilled_update_plan_items(update_plan));
    }

    if !backfilled_skills.is_empty() {
        history.extend(backfilled_skill_items(backfilled_skills));
    }

    history
}

pub(crate) fn last_completed_plan_from_history(items: &[ConversationItem]) -> Option<String> {
    items.iter().rev().find_map(|item| {
        let ConversationItem::Message { role, content, .. } = item else {
            return None;
        };
        if role != "assistant" {
            return None;
        }

        let mut text = String::new();
        for entry in content {
            if let ContentItem::OutputText { text: chunk } = entry {
                text.push_str(chunk);
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
    items: &[ConversationItem],
) -> Option<UpdatePlanArgs> {
    for (idx, item) in items.iter().enumerate().rev() {
        let ConversationItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } = item
        else {
            continue;
        };
        if name != "update_plan" {
            continue;
        }

        let Some(output) = items[idx + 1..]
            .iter()
            .find_map(|candidate| match candidate {
                ConversationItem::FunctionCallOutput {
                    call_id: existing,
                    output,
                } if existing == call_id => Some(output),
                _ => None,
            })
        else {
            continue;
        };
        if output.content != UPDATE_PLAN_SUCCESS_OUTPUT {
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

pub(crate) fn backfilled_update_plan_items(args: &UpdatePlanArgs) -> Vec<ConversationItem> {
    let arguments = match serde_json::to_string(args) {
        Ok(arguments) => arguments,
        Err(err) => {
            error!("failed to serialize backfilled update_plan args: {err}");
            return Vec::new();
        }
    };
    vec![
        ConversationItem::FunctionCall {
            id: None,
            name: "update_plan".to_string(),
            arguments,
            call_id: BACKFILLED_UPDATE_PLAN_CALL_ID.to_string(),
        },
        ConversationItem::FunctionCallOutput {
            call_id: BACKFILLED_UPDATE_PLAN_CALL_ID.to_string(),
            output: FunctionCallOutputPayload {
                content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                success: Some(true),
                ..Default::default()
            },
        },
    ]
}

pub(crate) fn recent_backfillable_skills_from_history(
    items: &[ConversationItem],
) -> Vec<SkillInstructions> {
    let compact_backfilled =
        collect_backfillable_skills_from_history(items, SkillInstructionSource::CompactBackfill);
    let direct = collect_backfillable_skills_from_history(items, SkillInstructionSource::Direct);
    let merged = merge_backfillable_skills(compact_backfilled, direct);

    apply_backfilled_skill_budget(merged)
}

pub(crate) fn backfilled_skill_items(skills: &[SkillInstructions]) -> Vec<ConversationItem> {
    skills
        .iter()
        .cloned()
        .map(SkillInstructions::into_backfilled_response_item)
        .collect()
}

fn rendered_skill_message_text(skill: &SkillInstructions) -> String {
    let item = skill.clone().into_backfilled_response_item();
    let ConversationItem::Message { content, .. } = item else {
        return String::new();
    };
    content_items_to_text(&content).unwrap_or_default()
}

fn collect_backfillable_skills_from_history(
    items: &[ConversationItem],
    expected_source: SkillInstructionSource,
) -> Vec<SkillInstructions> {
    let mut seen_paths = HashSet::new();
    let mut collected = Vec::new();

    for item in items.iter().rev() {
        let ConversationItem::Message { role, content, .. } = item else {
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

pub(crate) fn proposed_plan_message(plan_text: &str) -> ConversationItem {
    let text = format!("{PROPOSED_PLAN_OPEN_TAG}{plan_text}{PROPOSED_PLAN_CLOSE_TAG}");
    ConversationItem::Message {
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
                let warning = EventMsg::Warning(WarningEvent {
                    message: notice.message,
                });
                sess.send_event(turn_context, warning).await;
            }
            Ok(TurnEvent::ItemCompleted { item, .. }) => {
                let item: ConversationItem = item.into_item().into();
                sess.record_into_history(std::slice::from_ref(&item), turn_context)
                    .await;
            }
            Ok(TurnEvent::ToolCall(request)) => {
                let item: ConversationItem = request.to_transcript_item().into();
                sess.record_into_history(std::slice::from_ref(&item), turn_context)
                    .await;
            }
            Ok(TurnEvent::ServerReasoningIncluded(included)) => {
                sess.set_server_reasoning_included(included).await;
            }
            Ok(TurnEvent::RateLimits(snapshot)) => {
                sess.update_rate_limits(turn_context, snapshot).await;
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
    use crate::instructions::SkillInstructions;
    use crate::session_prefix::TURN_ABORTED_OPEN_TAG;
    use codex_protocol::legacy_transcript::ConversationItem;
    use pretty_assertions::assert_eq;

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
            ConversationItem::Message {
                id: Some("assistant".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "ignored".to_string(),
                }],
                end_turn: None,
            },
            ConversationItem::Message {
                id: Some("user".to_string()),
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "first".to_string(),
                }],
                end_turn: None,
            },
            ConversationItem::Other,
        ];

        let collected = collect_user_messages(&items);

        assert_eq!(vec!["first".to_string()], collected);
    }

    #[test]
    fn collect_user_messages_filters_session_prefix_entries() {
        let items = vec![
            ConversationItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "# AGENTS.md instructions for project\n\n<INSTRUCTIONS>\ndo things\n</INSTRUCTIONS>"
                        .to_string(),
                }],
                end_turn: None,
            },
            ConversationItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "<ENVIRONMENT_CONTEXT>cwd=/tmp</ENVIRONMENT_CONTEXT>".to_string(),
                }],
                end_turn: None,
            },
            ConversationItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "real user message".to_string(),
                }],
                end_turn: None,
            },
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
            None,
            None,
            &[],
            "SUMMARY",
            max_tokens,
        );
        assert_eq!(history.len(), 2);

        let truncated_message = &history[0];
        let summary_message = &history[1];

        let truncated_text = match truncated_message {
            ConversationItem::Message { role, content, .. } if role == "user" => {
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
            ConversationItem::Message { role, content, .. } if role == "user" => {
                content_items_to_text(content).unwrap_or_default()
            }
            other => panic!("unexpected item in history: {other:?}"),
        };
        assert_eq!(summary_text, "SUMMARY");
    }

    #[test]
    fn replacement_history_without_initial_context_omits_prefix_items() {
        let initial_context = vec![
            ConversationItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText {
                    text: "developer instructions".to_string(),
                }],
                end_turn: None,
            },
            ConversationItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "<environment_context>\n  <cwd>/source</cwd>\n</environment_context>"
                        .to_string(),
                }],
                end_turn: None,
            },
        ];
        let history = build_compacted_history(
            initial_context.clone(),
            &["user".to_string()],
            None,
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
        let initial_context: Vec<ConversationItem> = Vec::new();
        let user_messages = vec!["first user message".to_string()];
        let summary_text = "summary text";

        let history = build_compacted_history(
            initial_context,
            &user_messages,
            None,
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
            ConversationItem::Message { role, content, .. } if role == "user" => {
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
        let items = vec![
            ConversationItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: marker.clone(),
                }],
                end_turn: None,
            },
            ConversationItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "real user message".to_string(),
                }],
                end_turn: None,
            },
        ];

        let user_messages = collect_user_messages(&items);
        let history =
            build_compacted_history(Vec::new(), &user_messages, None, None, &[], "SUMMARY");

        let found_marker = history.iter().any(|item| match item {
            ConversationItem::Message { role, content, .. } if role == "user" => {
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
            ConversationItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Intro\n<proposed_plan>\n- Step 2\n</proposed_plan>\nOutro".to_string(),
                }],
                end_turn: None,
            },
        ];

        let plan = last_completed_plan_from_history(&items);

        assert_eq!(Some("- Step 2\n".to_string()), plan);
    }

    #[test]
    fn last_completed_plan_from_history_returns_none_when_missing() {
        let items = vec![ConversationItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "No plan here".to_string(),
            }],
            end_turn: None,
        }];

        let plan = last_completed_plan_from_history(&items);

        assert_eq!(None, plan);
    }

    #[test]
    fn build_compacted_history_appends_backfilled_plan_as_assistant_message() {
        let history = build_compacted_history(
            Vec::new(),
            &["user".to_string()],
            Some("- Step 1\n"),
            None,
            &[],
            "SUMMARY",
        );

        assert_eq!(history.len(), 3);
        assert_eq!(history[2], proposed_plan_message("- Step 1\n"));
    }

    #[test]
    fn last_backfillable_update_plan_from_history_returns_none_for_latest_completed_plan() {
        let older_args = UpdatePlanArgs {
            explanation: Some("Keep going".to_string()),
            plan: vec![
                codex_protocol::plan_tool::PlanItemArg {
                    step: "Inspect workspace".to_string(),
                    status: StepStatus::Completed,
                },
                codex_protocol::plan_tool::PlanItemArg {
                    step: "Patch compact".to_string(),
                    status: StepStatus::InProgress,
                },
            ],
        };
        let latest_completed_args = UpdatePlanArgs {
            explanation: None,
            plan: vec![codex_protocol::plan_tool::PlanItemArg {
                step: "Wrap up".to_string(),
                status: StepStatus::Completed,
            }],
        };
        let older_args_json = serde_json::to_string(&older_args).expect("serialize args");
        let newer_args_json =
            serde_json::to_string(&latest_completed_args).expect("serialize args");
        let items = vec![
            ConversationItem::FunctionCall {
                id: None,
                name: "update_plan".to_string(),
                arguments: older_args_json,
                call_id: "call-1".to_string(),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload {
                    content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                    success: Some(true),
                    ..Default::default()
                },
            },
            ConversationItem::FunctionCall {
                id: None,
                name: "update_plan".to_string(),
                arguments: newer_args_json,
                call_id: "call-2".to_string(),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-2".to_string(),
                output: FunctionCallOutputPayload {
                    content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                    success: Some(true),
                    ..Default::default()
                },
            },
        ];

        assert!(last_backfillable_update_plan_from_history(&items).is_none());
    }

    #[test]
    fn last_backfillable_update_plan_from_history_extracts_latest_unfinished_success() {
        let older_args = UpdatePlanArgs {
            explanation: Some("Keep going".to_string()),
            plan: vec![codex_protocol::plan_tool::PlanItemArg {
                step: "Inspect workspace".to_string(),
                status: StepStatus::Pending,
            }],
        };
        let latest_args = UpdatePlanArgs {
            explanation: Some("Patch compact".to_string()),
            plan: vec![codex_protocol::plan_tool::PlanItemArg {
                step: "Patch compact".to_string(),
                status: StepStatus::InProgress,
            }],
        };
        let older_args_json = serde_json::to_string(&older_args).expect("serialize args");
        let latest_args_json = serde_json::to_string(&latest_args).expect("serialize args");
        let items = vec![
            ConversationItem::FunctionCall {
                id: None,
                name: "update_plan".to_string(),
                arguments: older_args_json,
                call_id: "call-1".to_string(),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload {
                    content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                    success: Some(true),
                    ..Default::default()
                },
            },
            ConversationItem::FunctionCall {
                id: None,
                name: "update_plan".to_string(),
                arguments: latest_args_json,
                call_id: "call-2".to_string(),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-2".to_string(),
                output: FunctionCallOutputPayload {
                    content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                    success: Some(true),
                    ..Default::default()
                },
            },
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
            plan: vec![codex_protocol::plan_tool::PlanItemArg {
                step: "Retry compact".to_string(),
                status: StepStatus::Pending,
            }],
        };
        let valid_args_json = serde_json::to_string(&valid_args).expect("serialize args");
        let items = vec![
            ConversationItem::FunctionCall {
                id: None,
                name: "update_plan".to_string(),
                arguments: valid_args_json,
                call_id: "call-1".to_string(),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload {
                    content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                    success: Some(true),
                    ..Default::default()
                },
            },
            ConversationItem::FunctionCall {
                id: None,
                name: "update_plan".to_string(),
                arguments: "{".to_string(),
                call_id: "call-2".to_string(),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-2".to_string(),
                output: FunctionCallOutputPayload {
                    content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                    success: Some(true),
                    ..Default::default()
                },
            },
            ConversationItem::FunctionCall {
                id: None,
                name: "update_plan".to_string(),
                arguments: serde_json::json!({
                    "explanation": "Bad latest",
                    "plan": [
                        {
                            "step": "Broken",
                            "status": "in_progress"
                        }
                    ]
                })
                .to_string(),
                call_id: "call-3".to_string(),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-3".to_string(),
                output: FunctionCallOutputPayload {
                    content: "update_plan is a TODO/checklist tool and is not allowed in Plan mode"
                        .to_string(),
                    ..Default::default()
                },
            },
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
            plan: vec![codex_protocol::plan_tool::PlanItemArg {
                step: "Do work".to_string(),
                status: StepStatus::InProgress,
            }],
        };
        let args_json = serde_json::to_string(&args).expect("serialize args");
        let items = vec![
            ConversationItem::FunctionCall {
                id: None,
                name: "update_plan".to_string(),
                arguments: args_json,
                call_id: "call-1".to_string(),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload {
                    content: UPDATE_PLAN_SUCCESS_OUTPUT.to_string(),
                    success: None,
                    ..Default::default()
                },
            },
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
            plan: vec![codex_protocol::plan_tool::PlanItemArg {
                step: "Do work".to_string(),
                status: StepStatus::InProgress,
            }],
        };

        let history = build_compacted_history(
            Vec::new(),
            &["user".to_string()],
            None,
            Some(&args),
            &[],
            "SUMMARY",
        );

        assert_eq!(history.len(), 4);
        assert_eq!(history[2..], backfilled_update_plan_items(&args),);
    }

    #[test]
    fn recent_backfillable_skills_from_history_returns_latest_unique_skills() {
        let older = ConversationItem::from(SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "older".to_string(),
        });
        let newer = ConversationItem::from(SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "newer".to_string(),
        });
        let second = ConversationItem::from(SkillInstructions {
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
            ConversationItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "<skill>\n<name>demo</name>\nmissing path\n</skill>".to_string(),
                }],
                end_turn: None,
            },
            ConversationItem::from(SkillInstructions {
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
        let backfilled = SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "backfilled".to_string(),
        }
        .into_backfilled_response_item();

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
        let backfilled = SkillInstructions {
            name: "demo".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "backfilled".to_string(),
        }
        .into_backfilled_response_item();
        let direct = ConversationItem::from(SkillInstructions {
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
        let persisted_alpha = SkillInstructions {
            name: "alpha".to_string(),
            path: "skills/alpha/SKILL.md".to_string(),
            contents: "persisted alpha".to_string(),
        }
        .into_backfilled_response_item();
        let persisted_beta = SkillInstructions {
            name: "beta".to_string(),
            path: "skills/beta/SKILL.md".to_string(),
            contents: "persisted beta".to_string(),
        }
        .into_backfilled_response_item();
        let direct_gamma = ConversationItem::from(SkillInstructions {
            name: "gamma".to_string(),
            path: "skills/gamma/SKILL.md".to_string(),
            contents: "direct gamma".to_string(),
        });
        let direct_delta = ConversationItem::from(SkillInstructions {
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
            recent_backfillable_skills_from_history(&[ConversationItem::from(SkillInstructions {
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
            ConversationItem::from(SkillInstructions {
                name: name.to_string(),
                path: path.to_string(),
                contents: "word ".repeat(6_000),
            })
        };
        let items = vec![
            SkillInstructions {
                name: "alpha".to_string(),
                path: "skills/alpha/SKILL.md".to_string(),
                contents: "persisted alpha".to_string(),
            }
            .into_backfilled_response_item(),
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
            plan: vec![codex_protocol::plan_tool::PlanItemArg {
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
            Some("- Step 1\n"),
            Some(&args),
            &skills,
            "SUMMARY",
        );

        assert_eq!(history[2], proposed_plan_message("- Step 1\n"));
        assert_eq!(history[3..5], backfilled_update_plan_items(&args));
        assert_eq!(
            history[5],
            skills[0].clone().into_backfilled_response_item()
        );
    }
}
