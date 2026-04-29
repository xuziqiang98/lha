use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use adam_protocol::config_types::IdentityKind;
use adam_protocol::items::TurnItem;
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
use adam_llm::ToolCallPayload;
use adam_llm::ToolCallRequest;
use adam_llm::ToolResultItem;
use adam_llm::ToolResultPayload;
use adam_llm::TranscriptItem;
use futures::Future;
use tracing::debug;
use tracing::instrument;

/// Handle a completed output item from the model stream, recording it and
/// queuing any tool execution futures. This records items immediately so
/// history and rollout stay in sync even if the turn is later cancelled.
pub(crate) type InFlightFuture<'f> =
    Pin<Box<dyn Future<Output = Result<ToolResultItem>> + Send + 'f>>;

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
    item: TranscriptItem,
    previously_active_item: Option<TurnItem>,
) -> Result<OutputItemResult> {
    let mut output = OutputItemResult::default();
    let plan_mode = ctx.turn_context.identity.kind == IdentityKind::Planner;

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
    let tool_name = request.tool_name.clone();
    let source_item = request.to_transcript_item();
    let call_id = request.call_id.clone();
    let payload_outputs_custom = matches!(request.payload, ToolCallPayload::TextInput { .. });

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
            let tool_runtime = ctx.tool_runtime.clone();
            output.tool_future = Some(Box::pin(async move {
                tool_runtime
                    .handle_tool_call(call, cancellation_token)
                    .await
            }));
            output.needs_follow_up = true;
        }
        Err(FunctionCallError::RespondToModel(message)) => {
            let response_item = response_error_to_transcript_item(
                &call_id,
                &tool_name,
                payload_outputs_custom,
                message,
            );
            ctx.sess
                .record_conversation_items(&ctx.turn_context, std::slice::from_ref(&source_item))
                .await;
            ctx.sess
                .record_conversation_items(&ctx.turn_context, std::slice::from_ref(&response_item))
                .await;
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

            let response_item = TranscriptItem::ToolResult {
                call_id,
                tool_name: tool_name.clone(),
                payload: ToolResultPayload::Structured {
                    content: message,
                    content_items: None,
                    success: Some(false),
                },
            };
            ctx.sess
                .record_conversation_items(&ctx.turn_context, std::slice::from_ref(&source_item))
                .await;
            ctx.sess
                .record_conversation_items(&ctx.turn_context, std::slice::from_ref(&response_item))
                .await;
            output.needs_follow_up = true;
        }
    }

    Ok(output)
}

pub(crate) async fn handle_non_tool_response_item(
    item: &TranscriptItem,
    plan_mode: bool,
) -> Option<TurnItem> {
    debug!(?item, "Output item");

    match item {
        TranscriptItem::Message { .. }
        | TranscriptItem::Reasoning { .. }
        | TranscriptItem::HostedActivity { .. } => {
            let mut turn_item = parse_turn_item(item)?;
            if plan_mode && let TurnItem::AgentMessage(agent_message) = &mut turn_item {
                let combined = agent_message
                    .content
                    .iter()
                    .map(|entry| match entry {
                        adam_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
                    })
                    .collect::<String>();
                let stripped = strip_proposed_plan_blocks(&combined);
                agent_message.content =
                    vec![adam_protocol::items::AgentMessageContent::Text { text: stripped }];
            }
            Some(turn_item)
        }
        TranscriptItem::ToolResult { .. } => {
            debug!("unexpected tool output from stream");
            None
        }
        TranscriptItem::ToolCall { .. } | TranscriptItem::Unknown { .. } => None,
    }
}

pub(crate) fn last_assistant_message_from_item(
    item: &TranscriptItem,
    plan_mode: bool,
) -> Option<String> {
    if let TranscriptItem::Message { role, content, .. } = item
        && role == "assistant"
    {
        let combined = content
            .iter()
            .filter_map(|ci| match ci {
                adam_protocol::models::ContentItem::OutputText { text } => Some(text.as_str()),
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

fn response_error_to_transcript_item(
    call_id: &str,
    tool_name: &str,
    payload_outputs_custom: bool,
    message: String,
) -> TranscriptItem {
    let tool_result = if payload_outputs_custom {
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
    };

    tool_result.into()
}

#[cfg(test)]
fn tool_result_to_response_item(input: &ToolResultItem) -> TranscriptItem {
    input.clone().into()
}

#[cfg(test)]
fn empty_structured_tool_result(call_id: &str, tool_name: &str) -> ToolResultItem {
    ToolResultItem {
        call_id: call_id.to_string(),
        tool_name: tool_name.to_string(),
        payload: ToolResultPayload::Structured {
            content: String::new(),
            content_items: None,
            success: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn tool_result_to_response_item_keeps_empty_function_call_output_id() {
        let input = empty_structured_tool_result("", "local_shell");

        assert_eq!(
            tool_result_to_response_item(&input),
            TranscriptItem::ToolResult {
                call_id: String::new(),
                tool_name: "local_shell".to_string(),
                payload: ToolResultPayload::Structured {
                    content: String::new(),
                    content_items: None,
                    success: None,
                },
            }
        );
    }

    #[test]
    fn tool_result_to_response_item_keeps_function_call_output_with_id() {
        let input = ToolResultItem {
            call_id: "call-1".to_string(),
            tool_name: "apply_patch".to_string(),
            payload: ToolResultPayload::Structured {
                content: "done".to_string(),
                content_items: None,
                success: Some(true),
            },
        };

        assert_eq!(
            tool_result_to_response_item(&input),
            TranscriptItem::ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "apply_patch".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "done".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            }
        );
    }
}
