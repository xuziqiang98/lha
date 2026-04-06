use std::sync::Arc;

use crate::Prompt;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::compact::backfilled_skill_items;
use crate::compact::backfilled_update_plan_items;
use crate::compact::last_backfillable_update_plan_from_history;
use crate::compact::last_completed_plan_from_history;
use crate::compact::proposed_plan_message;
use crate::compact::recent_backfillable_skills_from_history;
use crate::error::Result as CodexResult;
use crate::features::Feature;
use crate::protocol::CompactedItem;
use crate::protocol::EventMsg;
use crate::protocol::RolloutItem;
use crate::protocol::TurnStartedEvent;
use codex_protocol::items::ContextCompactionItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseItem;

pub(crate) async fn run_inline_remote_auto_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) {
    run_remote_compact_task_inner(&sess, &turn_context).await;
}

pub(crate) async fn run_remote_compact_task(sess: Arc<Session>, turn_context: Arc<TurnContext>) {
    let start_event = EventMsg::TurnStarted(TurnStartedEvent {
        model_context_window: turn_context.client.get_model_context_window(),
        collaboration_mode_kind: turn_context.collaboration_mode.mode,
    });
    sess.send_event(&turn_context, start_event).await;

    run_remote_compact_task_inner(&sess, &turn_context).await;
}

async fn run_remote_compact_task_inner(sess: &Arc<Session>, turn_context: &Arc<TurnContext>) {
    if let Err(err) = run_remote_compact_task_inner_impl(sess, turn_context).await {
        let event = EventMsg::Error(
            err.to_error_event(Some("Error running remote compact task".to_string())),
        );
        sess.send_event(turn_context, event).await;
    }
}

async fn run_remote_compact_task_inner_impl(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
) -> CodexResult<()> {
    let compaction_item = TurnItem::ContextCompaction(ContextCompactionItem::new());
    sess.emit_turn_item_started(turn_context, &compaction_item)
        .await;
    let history = sess.clone_history().await;
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

    // Required to keep `/undo` available after compaction
    let ghost_snapshots: Vec<ResponseItem> = history
        .raw_items()
        .iter()
        .filter(|item| matches!(item, ResponseItem::GhostSnapshot { .. }))
        .cloned()
        .collect();

    let prompt = Prompt {
        input: history.for_compaction_prompt(),
        tools: vec![],
        parallel_tool_calls: false,
        base_instructions: sess.get_base_instructions().await,
        personality: turn_context.personality,
        output_schema: None,
    };

    let mut new_history = turn_context
        .client
        .compact_conversation_history(&prompt)
        .await?;

    if let Some(plan_text) = backfilled_plan_text.as_deref() {
        new_history.push(proposed_plan_message(plan_text));
    }
    if let Some(update_plan) = backfilled_update_plan.as_ref() {
        new_history.extend(backfilled_update_plan_items(update_plan));
    }
    if !backfilled_skills.is_empty() {
        new_history.extend(backfilled_skill_items(&backfilled_skills));
    }
    if !ghost_snapshots.is_empty() {
        new_history.extend(ghost_snapshots);
    }
    sess.replace_history(new_history.clone()).await;
    sess.recompute_token_usage(turn_context).await;

    let compacted_item = CompactedItem {
        message: String::new(),
        replacement_history: Some(new_history),
    };
    sess.persist_rollout_items(&[RolloutItem::Compacted(compacted_item)])
        .await;

    sess.emit_turn_item_completed(turn_context, compaction_item)
        .await;
    Ok(())
}
