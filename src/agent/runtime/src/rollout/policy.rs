use crate::protocol::EventMsg;
use crate::protocol::RolloutItem;
use lha_protocol::models::TranscriptItem;

/// Whether a rollout `item` should be persisted in rollout files.
#[inline]
pub(crate) fn is_persisted_response_item(item: &RolloutItem) -> bool {
    match item {
        RolloutItem::TranscriptItem(item) => should_persist_response_item(item),
        RolloutItem::EventMsg(ev) => should_persist_event_msg(ev),
        RolloutItem::GhostSnapshot(_) => true,
        // Persist LHA executive markers so we can analyze flows (e.g., compaction, API turns).
        RolloutItem::Compacted(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::Workflow(_)
        | RolloutItem::SessionMeta(_) => true,
    }
}

/// Whether a semantic transcript item should be persisted in rollout files.
#[inline]
pub(crate) fn should_persist_response_item(item: &TranscriptItem) -> bool {
    match item {
        TranscriptItem::Message { .. }
        | TranscriptItem::Reasoning { .. }
        | TranscriptItem::HostedActivity { .. }
        | TranscriptItem::ToolCall { .. }
        | TranscriptItem::ToolResult { .. }
        | TranscriptItem::Unknown { .. } => true,
    }
}

/// Whether an `EventMsg` should be persisted in rollout files.
#[inline]
pub(crate) fn should_persist_event_msg(ev: &EventMsg) -> bool {
    match ev {
        EventMsg::UserMessage(_)
        | EventMsg::AgentMessage(_)
        | EventMsg::AgentReasoning(_)
        | EventMsg::AgentReasoningRawContent(_)
        | EventMsg::TokenCount(_)
        | EventMsg::ContextCompacted(_)
        | EventMsg::EnteredReviewMode(_)
        | EventMsg::ExitedReviewMode(_)
        | EventMsg::ThreadRolledBack(_)
        | EventMsg::ThreadGoalUpdated(_)
        | EventMsg::ThreadGoalCleared(_)
        | EventMsg::ThreadPlanRunUpdated(_)
        | EventMsg::ThreadPlanRunCleared(_)
        | EventMsg::UndoCompleted(_)
        | EventMsg::TurnAborted(_) => true,
        EventMsg::ItemCompleted(event) => {
            // Plan items are derived from streaming tags and are not part of the
            // raw conversation history, so we persist their completion to replay
            // them on resume without bloating rollouts with every item lifecycle.
            matches!(
                event.item,
                lha_protocol::items::TurnItem::Plan(_)
                    | lha_protocol::items::TurnItem::ContextCompaction(_)
            )
        }
        EventMsg::Error(_)
        | EventMsg::Warning(_)
        | EventMsg::TurnStarted(_)
        | EventMsg::TurnComplete(_)
        | EventMsg::BuddyReaction(_)
        | EventMsg::AgentMessageDelta(_)
        | EventMsg::AgentReasoningDelta(_)
        | EventMsg::AgentReasoningRawContentDelta(_)
        | EventMsg::AgentReasoningSectionBreak(_)
        | EventMsg::RawTranscriptItem(_)
        | EventMsg::SessionConfigured(_)
        | EventMsg::ThreadNameUpdated(_)
        | EventMsg::ThreadGoalSnapshot(_)
        | EventMsg::ThreadGoalReplaceConfirmationRequired(_)
        | EventMsg::ThreadPlanRunSnapshot(_)
        | EventMsg::McpToolCallBegin(_)
        | EventMsg::McpToolCallEnd(_)
        | EventMsg::WebSearchBegin(_)
        | EventMsg::WebSearchEnd(_)
        | EventMsg::ExecCommandBegin(_)
        | EventMsg::TerminalInteraction(_)
        | EventMsg::ExecCommandOutputDelta(_)
        | EventMsg::ExecCommandEnd(_)
        | EventMsg::ExecApprovalRequest(_)
        | EventMsg::RequestUserInput(_)
        | EventMsg::DynamicToolCallRequest(_)
        | EventMsg::WorkflowUpdate(_)
        | EventMsg::ElicitationRequest(_)
        | EventMsg::ApplyPatchApprovalRequest(_)
        | EventMsg::BackgroundEvent(_)
        | EventMsg::StreamError(_)
        | EventMsg::PatchApplyBegin(_)
        | EventMsg::PatchApplyEnd(_)
        | EventMsg::TurnDiff(_)
        | EventMsg::GetHistoryEntryResponse(_)
        | EventMsg::UndoStarted(_)
        | EventMsg::McpListToolsResponse(_)
        | EventMsg::McpStartupUpdate(_)
        | EventMsg::McpStartupComplete(_)
        | EventMsg::ListCustomPromptsResponse(_)
        | EventMsg::ListSkillsResponse(_)
        | EventMsg::PlanUpdate(_)
        | EventMsg::AgentJobStatus(_)
        | EventMsg::ShutdownComplete
        | EventMsg::ViewImageToolCall(_)
        | EventMsg::DeprecationNotice(_)
        | EventMsg::ItemStarted(_)
        | EventMsg::AgentMessageContentDelta(_)
        | EventMsg::PlanDelta(_)
        | EventMsg::ReasoningContentDelta(_)
        | EventMsg::ReasoningRawContentDelta(_)
        | EventMsg::SkillsUpdateAvailable => false,
    }
}

#[cfg(test)]
mod tests {
    use super::should_persist_event_msg;
    use crate::protocol::EventMsg;
    use crate::protocol::ItemCompletedEvent;
    use lha_protocol::ThreadId;
    use lha_protocol::items::ContextCompactionItem;
    use lha_protocol::items::PlanItem;
    use lha_protocol::items::TurnItem;
    use pretty_assertions::assert_eq;

    #[test]
    fn persists_context_compaction_item_completed_events() {
        let event = EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: ThreadId::new(),
            turn_id: "turn-1".to_string(),
            item: TurnItem::ContextCompaction(ContextCompactionItem::new()),
        });

        assert_eq!(should_persist_event_msg(&event), true);
    }

    #[test]
    fn still_persists_plan_item_completed_events() {
        let event = EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: ThreadId::new(),
            turn_id: "turn-1".to_string(),
            item: TurnItem::Plan(PlanItem {
                id: "plan-1".to_string(),
                text: "plan".to_string(),
            }),
        });

        assert_eq!(should_persist_event_msg(&event), true);
    }
}
