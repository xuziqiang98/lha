use std::sync::Arc;

use crate::product::agent::codex::Session;
use crate::product::agent::codex::TurnContext;
use crate::product::agent::compact::active_goal_plan_reminder_items;
use crate::product::agent::compact::backfilled_skill_items;
use crate::product::agent::compact::backfilled_update_plan_items;
use crate::product::agent::compact::last_backfillable_update_plan_from_history;
use crate::product::agent::compact::last_completed_plan_from_history;
use crate::product::agent::compact::proposed_plan_backfill_items;
use crate::product::agent::compact::recent_backfillable_skills_from_history;
use crate::product::agent::error::CodexErr;
use crate::product::agent::error::Result as CodexResult;
use crate::product::agent::protocol::CompactedItem;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::RolloutItem;
use crate::product::agent::protocol::TurnStartedEvent;
use crate::product::protocol::items::ContextCompactionItem;
use crate::product::protocol::items::TurnItem;
use lha_llm::TurnRequest;

pub(crate) async fn run_inline_remote_auto_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) {
    run_remote_compact_task_inner(&sess, &turn_context).await;
}

pub(crate) async fn run_remote_compact_task(sess: Arc<Session>, turn_context: Arc<TurnContext>) {
    let start_event = EventMsg::TurnStarted(TurnStartedEvent {
        model_context_window: turn_context.runtime.get_model_context_window(),
        identity_kind: turn_context.identity.kind,
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
    let mut history = sess.clone_history().await;
    let active_goal_plan_path = sess.active_proposed_plan_goal_path().await;
    let backfilled_plan_text = active_goal_plan_path
        .is_none()
        .then(|| last_completed_plan_from_history(history.raw_items()))
        .flatten();
    let backfilled_update_plan = last_backfillable_update_plan_from_history(history.raw_items());
    let backfilled_skills = recent_backfillable_skills_from_history(history.raw_items());

    let mut truncated_count = 0usize;
    let mut new_history = loop {
        let turn_input = history.clone().for_compaction_prompt();
        let turn_input_len = turn_input.len();
        let prompt = TurnRequest {
            conversation: turn_input.into_iter().collect(),
            base_instructions: sess.get_base_instructions().await,
            personality: turn_context.personality,
            ..Default::default()
        };

        match turn_context.runtime.compact_turn_request(&prompt).await {
            Ok(new_history) => break new_history,
            Err(e @ CodexErr::ContextWindowExceeded) => {
                if turn_input_len > 1 {
                    history.remove_first_item();
                    truncated_count += 1;
                    continue;
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        }
    };
    if truncated_count > 0 {
        sess.notify_background_event(
            turn_context,
            format!(
                "Trimmed {truncated_count} older thread item(s) before compacting so the prompt fits the model context window."
            ),
        )
        .await;
    }

    match active_goal_plan_path.as_deref() {
        Some(path) => {
            new_history.extend(active_goal_plan_reminder_items(path));
        }
        None => {
            if let Some(plan_text) = backfilled_plan_text.as_deref() {
                new_history.extend(proposed_plan_backfill_items(plan_text));
            }
        }
    }
    if let Some(update_plan) = backfilled_update_plan.as_ref() {
        new_history.extend(backfilled_update_plan_items(update_plan));
    }
    if !backfilled_skills.is_empty() {
        new_history.extend(backfilled_skill_items(&backfilled_skills));
    }
    sess.replace_history(new_history.clone()).await;
    sess.recompute_token_usage(turn_context).await;

    let compacted_item = CompactedItem {
        message: String::new(),
        replacement_history: Some(new_history.into_iter().collect()),
        replacement_history_omits_initial_context: false,
    };
    sess.persist_rollout_items(&[RolloutItem::Compacted(compacted_item)])
        .await;

    sess.emit_turn_item_completed(turn_context, compaction_item)
        .await;
    Ok(())
}
