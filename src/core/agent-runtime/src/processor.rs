use crate::Error;
use crate::events::AgentEvent;
use crate::events::TurnItemDelta;
use crate::events::TurnSummary;
use crate::session::AgentSessionInner;
use crate::session::SubmissionId;
use crate::tools::ToolExecutor;
use async_trait::async_trait;
use codex_agent_core::kernel::TurnEventProcessor;
use codex_agent_core::kernel::TurnEventUpdate;
use codex_agent_core::kernel::TurnStreamOutcome;
use codex_agent_core::kernel::TurnStreamState;
use codex_llm::TurnEvent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ConversationItem;
use codex_protocol::models::ResponseInputItem;
use serde_json::to_string;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) struct SessionTurnProcessor {
    session: Arc<AgentSessionInner>,
    submission_id: SubmissionId,
    tool_executor: ToolExecutor,
    cancellation_token: CancellationToken,
    response_total_tokens: Option<i64>,
    tool_output_tokens: i64,
}

impl SessionTurnProcessor {
    pub(crate) fn new(
        session: Arc<AgentSessionInner>,
        submission_id: SubmissionId,
        tool_executor: ToolExecutor,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            session,
            submission_id,
            tool_executor,
            cancellation_token,
            response_total_tokens: None,
            tool_output_tokens: 0,
        }
    }
}

#[async_trait]
impl TurnEventProcessor for SessionTurnProcessor {
    type Error = Error;

    async fn handle_event(
        &mut self,
        event: TurnEvent,
    ) -> Result<TurnEventUpdate<Self::Error>, Self::Error> {
        match event {
            TurnEvent::Created => Ok(TurnEventUpdate::default()),
            TurnEvent::RuntimeNotice(notice) => {
                self.session
                    .emit_event(AgentEvent::RuntimeNotice {
                        session_id: self.session.session_id,
                        notice,
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::ItemStarted { handle, item } => {
                self.session
                    .emit_event(AgentEvent::OutputItemStarted {
                        session_id: self.session.session_id,
                        submission_id: self.submission_id,
                        handle: handle.clone(),
                        item,
                    })
                    .await;
                Ok(TurnEventUpdate {
                    active_handle: Some(handle),
                    ..Default::default()
                })
            }
            TurnEvent::ItemCompleted { handle, item } => {
                let conversation_item = item.clone().into_item();
                self.session
                    .push_conversation_item(conversation_item.clone())
                    .await;
                self.session
                    .emit_event(AgentEvent::OutputItemCompleted {
                        session_id: self.session.session_id,
                        submission_id: self.submission_id,
                        handle,
                        item,
                    })
                    .await;
                Ok(TurnEventUpdate {
                    last_agent_message: last_assistant_message(&conversation_item),
                    ..Default::default()
                })
            }
            TurnEvent::ToolCall(call) => {
                self.session.push_conversation_item(call.item.clone()).await;
                self.session
                    .emit_event(AgentEvent::ToolCallRequested {
                        session_id: self.session.session_id,
                        submission_id: self.submission_id,
                        call: call.clone(),
                    })
                    .await;
                Ok(TurnEventUpdate {
                    tool_future: Some(
                        self.tool_executor
                            .clone()
                            .handle_tool_call(call, self.cancellation_token.child_token()),
                    ),
                    needs_follow_up: true,
                    ..Default::default()
                })
            }
            TurnEvent::OutputTextDelta { handle, delta } => {
                self.session
                    .emit_event(AgentEvent::OutputItemDelta {
                        session_id: self.session.session_id,
                        submission_id: self.submission_id,
                        handle,
                        delta: TurnItemDelta::OutputText { delta },
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::ProposedPlanDelta { handle, delta } => {
                self.session
                    .emit_event(AgentEvent::OutputItemDelta {
                        session_id: self.session.session_id,
                        submission_id: self.submission_id,
                        handle,
                        delta: TurnItemDelta::ProposedPlan { delta },
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::ReasoningSummaryDelta {
                handle,
                delta,
                summary_index,
            } => {
                self.session
                    .emit_event(AgentEvent::OutputItemDelta {
                        session_id: self.session.session_id,
                        submission_id: self.submission_id,
                        handle,
                        delta: TurnItemDelta::ReasoningSummary {
                            delta,
                            summary_index,
                        },
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::ReasoningContentDelta {
                handle,
                delta,
                content_index,
            } => {
                self.session
                    .emit_event(AgentEvent::OutputItemDelta {
                        session_id: self.session.session_id,
                        submission_id: self.submission_id,
                        handle,
                        delta: TurnItemDelta::ReasoningContent {
                            delta,
                            content_index,
                        },
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::ReasoningSummaryPartAdded {
                handle,
                summary_index,
            } => {
                self.session
                    .emit_event(AgentEvent::OutputItemDelta {
                        session_id: self.session.session_id,
                        submission_id: self.submission_id,
                        handle,
                        delta: TurnItemDelta::ReasoningSummaryPartAdded { summary_index },
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::ServerReasoningIncluded(included) => {
                self.session
                    .emit_event(AgentEvent::ServerReasoningIncluded {
                        session_id: self.session.session_id,
                        included,
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::RateLimits(snapshot) => {
                self.session
                    .emit_event(AgentEvent::RateLimitsUpdated {
                        session_id: self.session.session_id,
                        snapshot,
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::ModelsEtag(etag) => {
                self.session
                    .emit_event(AgentEvent::ModelsEtagUpdated {
                        session_id: self.session.session_id,
                        etag,
                    })
                    .await;
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::Completed {
                response_id: _,
                token_usage,
            } => {
                self.response_total_tokens = token_usage.map(|usage| usage.total_tokens);
                Ok(TurnEventUpdate::default())
            }
            TurnEvent::ProposedPlanDone { .. } => Ok(TurnEventUpdate::default()),
        }
    }

    async fn record_tool_result(&mut self, response: ResponseInputItem) -> Result<(), Self::Error> {
        if let Some(item) = response_input_to_conversation_item(&response) {
            self.session.push_conversation_item(item).await;
        }
        self.tool_output_tokens += estimate_token_count(&response);
        self.session
            .emit_event(AgentEvent::ToolCallCompleted {
                session_id: self.session.session_id,
                submission_id: self.submission_id,
                response,
            })
            .await;
        Ok(())
    }

    async fn on_tool_future_error(&mut self, err: Self::Error) -> Result<(), Self::Error> {
        Err(err)
    }

    async fn finish(self, state: TurnStreamState) -> Result<TurnStreamOutcome, Self::Error> {
        Ok(TurnStreamOutcome {
            needs_follow_up: state.needs_follow_up,
            last_agent_message: state.last_agent_message,
            response_total_tokens: self.response_total_tokens,
            tool_output_tokens: self.tool_output_tokens,
        })
    }

    fn cancelled_error(&self) -> Self::Error {
        Error::Aborted
    }

    fn llm_error(&self, err: codex_llm::Error) -> Self::Error {
        Error::Runtime(err)
    }

    fn stream_closed_error(&self) -> Self::Error {
        Error::StreamClosed
    }
}

pub(crate) fn outcome_summary(outcome: TurnStreamOutcome) -> TurnSummary {
    TurnSummary {
        needs_follow_up: outcome.needs_follow_up,
        last_agent_message: outcome.last_agent_message,
        response_total_tokens: outcome.response_total_tokens,
        tool_output_tokens: outcome.tool_output_tokens,
    }
}

fn last_assistant_message(item: &ConversationItem) -> Option<String> {
    match item {
        ConversationItem::Message { role, content, .. } if role == "assistant" => {
            content.iter().rev().find_map(|entry| match entry {
                ContentItem::OutputText { text } => Some(text.clone()),
                ContentItem::InputText { .. } | ContentItem::InputImage { .. } => None,
            })
        }
        ConversationItem::Reasoning { .. }
        | ConversationItem::LocalShellCall { .. }
        | ConversationItem::FunctionCall { .. }
        | ConversationItem::FunctionCallOutput { .. }
        | ConversationItem::CustomToolCall { .. }
        | ConversationItem::CustomToolCallOutput { .. }
        | ConversationItem::WebSearchCall { .. }
        | ConversationItem::GhostSnapshot { .. }
        | ConversationItem::Compaction { .. }
        | ConversationItem::Other => None,
        ConversationItem::Message { .. } => None,
    }
}

fn response_input_to_conversation_item(response: &ResponseInputItem) -> Option<ConversationItem> {
    match response {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            Some(ConversationItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: output.clone(),
            })
        }
        ResponseInputItem::CustomToolCallOutput { call_id, output } => {
            Some(ConversationItem::CustomToolCallOutput {
                call_id: call_id.clone(),
                output: output.clone(),
            })
        }
        ResponseInputItem::Message { role, content } => Some(ConversationItem::Message {
            id: None,
            role: role.clone(),
            content: content.clone(),
            end_turn: None,
        }),
        ResponseInputItem::McpToolCallOutput { .. } => None,
    }
}

fn estimate_token_count(response: &ResponseInputItem) -> i64 {
    to_string(response)
        .ok()
        .and_then(|text| i64::try_from((text.len() / 4) + 1).ok())
        .unwrap_or(0)
}
