use std::sync::Arc;

use anyhow::Context;
use futures::StreamExt;
use lha_llm::TurnEvent;
use lha_llm::TurnRequest;
use lha_protocol::models::BaseInstructions;
use lha_protocol::models::ContentItem;
use lha_protocol::models::TranscriptItem;
use lha_protocol::protocol::RolloutItem;
use lha_protocol::protocol::SessionSource;
use lha_protocol::protocol::USER_MESSAGE_BEGIN;
use lha_secrets::redact_secrets;
use lha_state::Stage1JobClaim;
use lha_state::Stage1StartupClaimParams;
use tracing::debug;
use tracing::info;
use tracing::warn;

use crate::config::Config;
use crate::memories::metrics;
use crate::memories::runtime::MemoryStartupContext;
use crate::rollout::RolloutRecorder;
use crate::truncate::approx_token_count;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobOutcome {
    SucceededWithOutput,
    SucceededNoOutput,
    Failed,
}

pub(crate) async fn run(context: Arc<MemoryStartupContext>) {
    let claims = match claim_startup_jobs(context.as_ref()).await {
        Some(claims) => claims,
        None => return,
    };
    if claims.is_empty() {
        metrics::counter(metrics::PHASE1_JOBS, 1, &[("status", "skipped_no_claims")]);
        return;
    }
    metrics::counter(
        metrics::PHASE1_JOBS,
        u64::try_from(claims.len()).unwrap_or(u64::MAX),
        &[("status", "claimed")],
    );

    let outcomes = futures::stream::iter(claims)
        .map(|claim| {
            let context = Arc::clone(&context);
            async move { run_job(context.as_ref(), claim).await }
        })
        .buffer_unordered(lha_memories_write::STAGE_ONE_CONCURRENCY_LIMIT)
        .collect::<Vec<_>>()
        .await;

    let succeeded = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome,
                JobOutcome::SucceededWithOutput | JobOutcome::SucceededNoOutput
            )
        })
        .count();
    let failed = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, JobOutcome::Failed))
        .count();
    info!(
        "memory phase-1 extraction complete: {} succeeded, {failed} failed",
        succeeded
    );
}

async fn claim_startup_jobs(context: &MemoryStartupContext) -> Option<Vec<Stage1JobClaim>> {
    let Some(state_db) = context.state_db() else {
        warn!("state db unavailable while claiming memory phase-1 jobs");
        return None;
    };
    let allowed_sources = [
        SessionSource::Cli.to_string(),
        SessionSource::VSCode.to_string(),
    ];
    match state_db
        .memories()
        .claim_stage1_jobs_for_startup(
            context.thread_id(),
            Stage1StartupClaimParams {
                scan_limit: lha_memories_write::STAGE_ONE_THREAD_SCAN_LIMIT,
                max_claimed: context.config().memories.max_rollouts_per_startup,
                max_age_days: context.config().memories.max_rollout_age_days,
                min_rollout_idle_hours: context.config().memories.min_rollout_idle_hours,
                allowed_sources: &allowed_sources,
                lease_seconds: lha_memories_write::STAGE_ONE_JOB_LEASE_SECONDS,
            },
        )
        .await
    {
        Ok(claims) => Some(claims),
        Err(err) => {
            warn!("failed to claim memory phase-1 jobs: {err}");
            metrics::counter(metrics::PHASE1_JOBS, 1, &[("status", "failed")]);
            None
        }
    }
}

async fn run_job(context: &MemoryStartupContext, claim: Stage1JobClaim) -> JobOutcome {
    let thread = claim.thread;
    let source_updated_at = thread.updated_at.timestamp();
    let sampled = sample(context, context.config(), &thread.rollout_path, &thread.cwd).await;
    let output = match sampled {
        Ok(output) => output,
        Err(err) => {
            let reason = err.to_string();
            let status = if reason.contains("unsupported_output_schema") {
                "unsupported_output_schema"
            } else {
                "failed"
            };
            metrics::counter(metrics::PHASE1_JOBS, 1, &[("status", status)]);
            mark_failed(context, thread.id, &claim.ownership_token, reason.as_str()).await;
            return JobOutcome::Failed;
        }
    };

    let raw_memory = redact_secrets(output.raw_memory);
    let rollout_summary = redact_secrets(output.rollout_summary);
    let rollout_slug = output.rollout_slug.map(redact_secrets);
    if raw_memory.trim().is_empty() || rollout_summary.trim().is_empty() {
        return mark_no_output(context, thread.id, &claim.ownership_token).await;
    }

    let Some(state_db) = context.state_db() else {
        return JobOutcome::Failed;
    };
    match state_db
        .memories()
        .mark_stage1_job_succeeded(
            thread.id,
            &claim.ownership_token,
            source_updated_at,
            raw_memory.as_str(),
            rollout_summary.as_str(),
            rollout_slug.as_deref(),
        )
        .await
    {
        Ok(true) => {
            metrics::counter(
                metrics::PHASE1_JOBS,
                1,
                &[("status", "succeeded_with_output")],
            );
            JobOutcome::SucceededWithOutput
        }
        Ok(false) => {
            metrics::counter(metrics::PHASE1_JOBS, 1, &[("status", "failed")]);
            JobOutcome::Failed
        }
        Err(err) => {
            warn!("failed to mark memory phase-1 job succeeded: {err}");
            metrics::counter(metrics::PHASE1_JOBS, 1, &[("status", "failed")]);
            JobOutcome::Failed
        }
    }
}

async fn sample(
    context: &MemoryStartupContext,
    _config: &Config,
    rollout_path: &std::path::Path,
    rollout_cwd: &std::path::Path,
) -> anyhow::Result<lha_memories_write::StageOneOutput> {
    let runtime = context.stage_one_runtime().await;
    if !runtime.runtime_capabilities().supports_output_schema {
        anyhow::bail!("unsupported_output_schema");
    }
    let token_budget = stage_one_rollout_token_budget(runtime.get_model_info().context_window);

    let (rollout_items, _, _) = RolloutRecorder::load_rollout_items(rollout_path)
        .await
        .with_context(|| format!("load rollout {}", rollout_path.display()))?;
    let serialized_rollout =
        serialize_filtered_rollout_items_with_budget(&rollout_items, token_budget)?;
    if serialized_rollout.truncated {
        metrics::counter(metrics::PHASE1_JOBS, 1, &[("status", "truncated_rollout")]);
    }
    debug!(
        original_item_count = serialized_rollout.original_item_count,
        retained_item_count = serialized_rollout.retained_item_count,
        original_tokens = serialized_rollout.original_tokens,
        retained_tokens = serialized_rollout.retained_tokens,
        token_budget,
        "prepared memory phase-1 rollout sample"
    );
    let input = lha_memories_write::build_stage_one_input_message(
        rollout_path,
        rollout_cwd,
        serialized_rollout.contents.as_str(),
    );
    let request = TurnRequest {
        conversation: vec![TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: input }],
            end_turn: None,
        }],
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: lha_memories_write::STAGE_ONE_SYSTEM_PROMPT.to_string(),
        },
        personality: None,
        output_schema: Some(lha_memories_write::stage_one_output_schema()),
    };

    let mut session = runtime.new_session();
    let mut stream = session.run_turn(&request).await?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            TurnEvent::OutputTextDelta { delta, .. } => text.push_str(&delta),
            TurnEvent::ItemCompleted { item, .. } if text.trim().is_empty() => {
                if let TranscriptItem::Message { content, .. } = item.into_item() {
                    for content_item in content {
                        if let ContentItem::OutputText { text: content_text } = content_item {
                            text.push_str(&content_text);
                        }
                    }
                }
            }
            TurnEvent::Completed { token_usage, .. } => {
                if let Some(token_usage) = token_usage {
                    metrics::counter(
                        metrics::PHASE1_TOKEN_USAGE,
                        u64::try_from(token_usage.total_tokens.max(0)).unwrap_or(u64::MAX),
                        &[("kind", "total")],
                    );
                }
                break;
            }
            _ => {}
        }
    }

    serde_json::from_str(text.trim()).context("parse memory phase-1 JSON output")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SerializedRolloutItems {
    contents: String,
    original_item_count: usize,
    retained_item_count: usize,
    original_tokens: usize,
    retained_tokens: usize,
    truncated: bool,
}

fn serialize_filtered_rollout_items_with_budget(
    items: &[RolloutItem],
    token_budget: usize,
) -> anyhow::Result<SerializedRolloutItems> {
    let filtered = items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::TranscriptItem(item) => sanitize_transcript_item_for_memories(item),
            RolloutItem::SessionMeta(_)
            | RolloutItem::GhostSnapshot(_)
            | RolloutItem::Compacted(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::Workflow(_)
            | RolloutItem::EventMsg(_) => None,
        })
        .collect::<Vec<_>>();
    let serialized = serde_json::to_string(&filtered)?;
    let original_tokens = approx_token_count(&serialized);
    if original_tokens <= token_budget || filtered.len() <= 1 {
        return Ok(SerializedRolloutItems {
            contents: redact_secrets(serialized),
            original_item_count: filtered.len(),
            retained_item_count: filtered.len(),
            original_tokens,
            retained_tokens: original_tokens,
            truncated: false,
        });
    }

    for start in 1..filtered.len() {
        let candidate = &filtered[start..];
        let candidate_serialized = serde_json::to_string(candidate)?;
        let candidate_tokens = approx_token_count(&candidate_serialized);
        if candidate_tokens <= token_budget || candidate.len() == 1 {
            return Ok(SerializedRolloutItems {
                contents: redact_secrets(candidate_serialized),
                original_item_count: filtered.len(),
                retained_item_count: candidate.len(),
                original_tokens,
                retained_tokens: candidate_tokens,
                truncated: true,
            });
        }
    }

    Ok(SerializedRolloutItems {
        contents: redact_secrets(serialized),
        original_item_count: filtered.len(),
        retained_item_count: filtered.len(),
        original_tokens,
        retained_tokens: original_tokens,
        truncated: false,
    })
}

fn stage_one_rollout_token_budget(context_window: Option<i64>) -> usize {
    let fallback = lha_memories_write::STAGE_ONE_DEFAULT_ROLLOUT_TOKEN_LIMIT;
    context_window
        .and_then(|context_window| {
            let budget = context_window
                .max(0)
                .saturating_mul(lha_memories_write::STAGE_ONE_CONTEXT_WINDOW_PERCENT)
                / 100;
            usize::try_from(budget).ok()
        })
        .filter(|budget| *budget > 0)
        .map(|budget| budget.min(fallback))
        .unwrap_or(fallback)
}

fn sanitize_transcript_item_for_memories(item: &TranscriptItem) -> Option<TranscriptItem> {
    let TranscriptItem::Message {
        id,
        role,
        content,
        end_turn,
    } = item
    else {
        return Some(item.clone());
    };

    if role == "developer" {
        return None;
    }
    if role != "user" {
        return Some(item.clone());
    }

    let content = content
        .iter()
        .filter_map(sanitize_user_content_item_for_memories)
        .collect::<Vec<_>>();
    (!content.is_empty()).then(|| TranscriptItem::Message {
        id: id.clone(),
        role: role.clone(),
        content,
        end_turn: *end_turn,
    })
}

fn sanitize_user_content_item_for_memories(content_item: &ContentItem) -> Option<ContentItem> {
    let ContentItem::InputText { text } = content_item else {
        return Some(content_item.clone());
    };

    if is_memory_excluded_contextual_user_fragment(text) {
        return None;
    }

    if let Some((_, user_message)) = text.split_once(USER_MESSAGE_BEGIN) {
        let user_message = user_message.trim();
        if user_message.is_empty() {
            None
        } else {
            Some(ContentItem::InputText {
                text: user_message.to_string(),
            })
        }
    } else {
        Some(content_item.clone())
    }
}

fn is_memory_excluded_contextual_user_fragment(text: &str) -> bool {
    matches_marked_fragment(text, "# AGENTS.md instructions for ", "</INSTRUCTIONS>")
        || matches_marked_fragment(text, "<skill>", "</skill>")
}

fn matches_marked_fragment(text: &str, start_marker: &str, end_marker: &str) -> bool {
    let trimmed = text.trim_start();
    let starts_with_marker = trimmed
        .get(..start_marker.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(start_marker));
    let trimmed = trimmed.trim_end();
    let ends_with_marker = trimmed
        .get(trimmed.len().saturating_sub(end_marker.len())..)
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(end_marker));
    starts_with_marker && ends_with_marker
}

async fn mark_no_output(
    context: &MemoryStartupContext,
    thread_id: lha_protocol::ThreadId,
    ownership_token: &str,
) -> JobOutcome {
    let Some(state_db) = context.state_db() else {
        return JobOutcome::Failed;
    };
    match state_db
        .memories()
        .mark_stage1_job_succeeded_no_output(thread_id, ownership_token)
        .await
    {
        Ok(true) => {
            metrics::counter(
                metrics::PHASE1_JOBS,
                1,
                &[("status", "succeeded_no_output")],
            );
            JobOutcome::SucceededNoOutput
        }
        Ok(false) => {
            metrics::counter(metrics::PHASE1_JOBS, 1, &[("status", "failed")]);
            JobOutcome::Failed
        }
        Err(err) => {
            warn!("failed to mark memory phase-1 no-output success: {err}");
            metrics::counter(metrics::PHASE1_JOBS, 1, &[("status", "failed")]);
            JobOutcome::Failed
        }
    }
}

async fn mark_failed(
    context: &MemoryStartupContext,
    thread_id: lha_protocol::ThreadId,
    ownership_token: &str,
    reason: &str,
) {
    warn!("memory phase-1 job failed for thread {thread_id}: {reason}");
    let Some(state_db) = context.state_db() else {
        return;
    };
    if let Err(err) = state_db
        .memories()
        .mark_stage1_job_failed(
            thread_id,
            ownership_token,
            reason,
            lha_memories_write::STAGE_ONE_JOB_RETRY_DELAY_SECONDS,
        )
        .await
    {
        warn!("failed to mark memory phase-1 job failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lha_protocol::models::ContentItem;
    use lha_protocol::models::TranscriptItem;
    use pretty_assertions::assert_eq;

    #[test]
    fn classifies_memory_excluded_fragments() {
        let cases = [
            (
                "# AGENTS.md instructions for /tmp\n\n<INSTRUCTIONS>\nbody\n</INSTRUCTIONS>",
                true,
            ),
            (
                "<skill>\n<name>demo</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
                true,
            ),
            (
                "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>",
                false,
            ),
        ];

        for (text, expected) in cases {
            assert_eq!(
                is_memory_excluded_contextual_user_fragment(text),
                expected,
                "{text}",
            );
        }
    }

    #[test]
    fn removes_user_message_prefix_from_memory_sample() {
        let item = user_message(format!("metadata {USER_MESSAGE_BEGIN} remember this").as_str());

        let Some(TranscriptItem::Message { content, .. }) =
            sanitize_transcript_item_for_memories(&item)
        else {
            panic!("expected sanitized message");
        };

        assert_eq!(
            content,
            vec![ContentItem::InputText {
                text: "remember this".to_string(),
            }]
        );
    }

    #[test]
    fn rollout_budget_keeps_newest_items_in_chronological_order() {
        let items = vec![
            RolloutItem::TranscriptItem(user_message("old ".repeat(2_000).as_str())),
            RolloutItem::TranscriptItem(user_message("middle")),
            RolloutItem::TranscriptItem(user_message("new")),
        ];
        let expected_retained = vec![user_message("middle"), user_message("new")];
        let budget = approx_token_count(&serde_json::to_string(&expected_retained).unwrap()) + 1;

        let serialized =
            serialize_filtered_rollout_items_with_budget(&items, budget).expect("serialized");
        let retained: Vec<TranscriptItem> =
            serde_json::from_str(&serialized.contents).expect("transcript items");

        assert!(serialized.truncated);
        assert_eq!(retained, expected_retained);
    }

    #[test]
    fn rollout_budget_leaves_short_rollout_unchanged() {
        let items = vec![
            RolloutItem::TranscriptItem(user_message("old")),
            RolloutItem::TranscriptItem(user_message("new")),
        ];

        let serialized =
            serialize_filtered_rollout_items_with_budget(&items, usize::MAX).expect("serialized");

        assert!(!serialized.truncated);
        assert_eq!(serialized.original_item_count, 2);
        assert_eq!(serialized.retained_item_count, 2);
    }

    #[test]
    fn rollout_filtering_runs_before_budgeting() {
        let items = vec![
            RolloutItem::TranscriptItem(developer_message("developer")),
            RolloutItem::TranscriptItem(user_message(
                "# AGENTS.md instructions for /tmp\n\n<INSTRUCTIONS>\nbody\n</INSTRUCTIONS>",
            )),
            RolloutItem::TranscriptItem(user_message("actual")),
        ];

        let serialized =
            serialize_filtered_rollout_items_with_budget(&items, usize::MAX).expect("serialized");
        let retained: Vec<TranscriptItem> =
            serde_json::from_str(&serialized.contents).expect("transcript items");

        assert_eq!(retained, vec![user_message("actual")]);
    }

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

    fn developer_message(text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }
}
