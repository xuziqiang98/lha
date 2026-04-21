use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use codex_protocol::config_types::ModeKind;
use codex_protocol::items::TurnItem;
use tokio_util::sync::CancellationToken;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::error::CodexErr;
use crate::error::Result;
use crate::function_tool::FunctionCallError;
use crate::parse_turn_item;
use crate::proposed_plan_parser::strip_proposed_plan_blocks;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::ToolRouter;
use codex_llm::ToolCallPayload;
use codex_llm::ToolCallRequest;
use codex_protocol::models::ConversationItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use futures::Future;
use tracing::debug;
use tracing::instrument;

/// Handle a completed output item from the model stream, recording it and
/// queuing any tool execution futures. This records items immediately so
/// history and rollout stay in sync even if the turn is later cancelled.
pub(crate) type InFlightFuture<'f> =
    Pin<Box<dyn Future<Output = Result<ResponseInputItem>> + Send + 'f>>;

#[derive(Default)]
pub(crate) struct OutputItemResult {
    pub last_agent_message: Option<String>,
    pub needs_follow_up: bool,
    pub tool_future: Option<InFlightFuture<'static>>,
}

pub(crate) struct HandleOutputCtx {
    pub sess: Arc<Session>,
    pub turn_context: Arc<TurnContext>,
    pub tool_runtime: ToolCallRuntime,
    pub cancellation_token: CancellationToken,
}

#[instrument(level = "trace", skip_all)]
pub(crate) async fn handle_output_item_done(
    ctx: &mut HandleOutputCtx,
    item: ConversationItem,
    previously_active_item: Option<TurnItem>,
) -> Result<OutputItemResult> {
    let mut output = OutputItemResult::default();
    let plan_mode = ctx.turn_context.collaboration_mode.mode == ModeKind::Plan;

    if let Some(turn_item) = handle_non_tool_response_item(&item, plan_mode).await {
        if previously_active_item.is_none() {
            ctx.sess
                .emit_turn_item_started(&ctx.turn_context, &turn_item)
                .await;
        }

        ctx.sess
            .emit_turn_item_completed(&ctx.turn_context, turn_item)
            .await;
    }

    ctx.sess
        .record_conversation_items(&ctx.turn_context, std::slice::from_ref(&item))
        .await;
    output.last_agent_message = last_assistant_message_from_item(&item, plan_mode);

    Ok(output)
}

#[instrument(level = "trace", skip_all)]
pub(crate) async fn handle_tool_call_request(
    ctx: &mut HandleOutputCtx,
    request: ToolCallRequest,
) -> Result<OutputItemResult> {
    let mut output = OutputItemResult::default();
    let source_item = codex_llm::tool_call_to_transcript_item(&request)
        .ok_or_else(|| CodexErr::Fatal("failed to reconstruct tool call item".to_string()))?;
    let call_id = request.call_id.clone();
    let payload_outputs_custom = matches!(request.payload, ToolCallPayload::Custom { .. });

    match ToolRouter::build_tool_call(ctx.sess.as_ref(), request).await {
        Ok(call) => {
            let payload_preview = call.payload.log_payload().into_owned();
            tracing::info!(
                thread_id = %ctx.sess.conversation_id,
                "ToolCall: {} {}",
                call.tool_name,
                payload_preview
            );

            ctx.sess
                .record_conversation_items(&ctx.turn_context, std::slice::from_ref(&source_item))
                .await;

            let cancellation_token = ctx.cancellation_token.child_token();
            output.tool_future = Some(Box::pin(
                ctx.tool_runtime
                    .clone()
                    .handle_tool_call(call, cancellation_token),
            ));
            output.needs_follow_up = true;
        }
        Err(FunctionCallError::RespondToModel(message)) => {
            let response = if payload_outputs_custom {
                ResponseInputItem::CustomToolCallOutput {
                    call_id: call_id.clone(),
                    output: message,
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id,
                    output: FunctionCallOutputPayload {
                        content: message,
                        ..Default::default()
                    },
                }
            };
            ctx.sess
                .record_conversation_items(&ctx.turn_context, std::slice::from_ref(&source_item))
                .await;
            if let Some(response_item) = response_input_to_response_item(&response) {
                ctx.sess
                    .record_conversation_items(
                        &ctx.turn_context,
                        std::slice::from_ref(&response_item),
                    )
                    .await;
            }
            output.needs_follow_up = true;
        }
        Err(FunctionCallError::Fatal(message)) => {
            return Err(CodexErr::Fatal(message));
        }
        Err(FunctionCallError::MissingLocalShellCallId) => {
            let message = FunctionCallError::MissingLocalShellCallId.to_string();
            ctx.turn_context.runtime.get_otel_manager().tool_result(
                "local_shell",
                "",
                "",
                Duration::ZERO,
                false,
                &message,
            );

            let response = ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: message,
                    success: Some(false),
                    ..Default::default()
                },
            };
            ctx.sess
                .record_conversation_items(&ctx.turn_context, std::slice::from_ref(&source_item))
                .await;
            if let Some(response_item) = response_input_to_response_item(&response) {
                ctx.sess
                    .record_conversation_items(
                        &ctx.turn_context,
                        std::slice::from_ref(&response_item),
                    )
                    .await;
            }
            output.needs_follow_up = true;
        }
    }

    Ok(output)
}

pub(crate) async fn handle_non_tool_response_item(
    item: &ConversationItem,
    plan_mode: bool,
) -> Option<TurnItem> {
    debug!(?item, "Output item");

    match item {
        ConversationItem::Message { .. }
        | ConversationItem::Reasoning { .. }
        | ConversationItem::WebSearchCall { .. } => {
            let mut turn_item = parse_turn_item(item)?;
            if plan_mode && let TurnItem::AgentMessage(agent_message) = &mut turn_item {
                let combined = agent_message
                    .content
                    .iter()
                    .map(|entry| match entry {
                        codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
                    })
                    .collect::<String>();
                let stripped = strip_proposed_plan_blocks(&combined);
                agent_message.content =
                    vec![codex_protocol::items::AgentMessageContent::Text { text: stripped }];
            }
            Some(turn_item)
        }
        ConversationItem::FunctionCallOutput { .. }
        | ConversationItem::CustomToolCallOutput { .. } => {
            debug!("unexpected tool output from stream");
            None
        }
        _ => None,
    }
}

pub(crate) fn last_assistant_message_from_item(
    item: &ConversationItem,
    plan_mode: bool,
) -> Option<String> {
    if let ConversationItem::Message { role, content, .. } = item
        && role == "assistant"
    {
        let combined = content
            .iter()
            .filter_map(|ci| match ci {
                codex_protocol::models::ContentItem::OutputText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        if combined.is_empty() {
            return None;
        }
        return if plan_mode {
            let stripped = strip_proposed_plan_blocks(&combined);
            (!stripped.trim().is_empty()).then_some(stripped)
        } else {
            Some(combined)
        };
    }
    None
}

pub(crate) fn response_input_to_response_item(
    input: &ResponseInputItem,
) -> Option<ConversationItem> {
    match input {
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
        ResponseInputItem::McpToolCallOutput { call_id, result } => {
            let output = match result {
                Ok(call_tool_result) => FunctionCallOutputPayload::from(call_tool_result),
                Err(err) => FunctionCallOutputPayload {
                    content: err.clone(),
                    success: Some(false),
                    ..Default::default()
                },
            };
            Some(ConversationItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn response_input_to_response_item_keeps_empty_function_call_output_id() {
        let input = ResponseInputItem::FunctionCallOutput {
            call_id: String::new(),
            output: FunctionCallOutputPayload::default(),
        };

        assert_eq!(
            response_input_to_response_item(&input),
            Some(ConversationItem::FunctionCallOutput {
                call_id: String::new(),
                output: FunctionCallOutputPayload::default(),
            })
        );
    }

    #[test]
    fn response_input_to_response_item_keeps_function_call_output_with_id() {
        let input = ResponseInputItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: FunctionCallOutputPayload {
                content: "done".to_string(),
                success: Some(true),
                ..Default::default()
            },
        };

        assert_eq!(
            response_input_to_response_item(&input),
            Some(ConversationItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: FunctionCallOutputPayload {
                    content: "done".to_string(),
                    success: Some(true),
                    ..Default::default()
                },
            })
        );
    }
}
