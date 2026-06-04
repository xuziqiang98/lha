use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use lha_protocol::config_types::IdentityKind;
use lha_protocol::items::TurnItem;
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
use futures::Future;
use lha_llm::ToolCallPayload;
use lha_llm::ToolCallRequest;
use lha_llm::ToolResultItem;
use lha_llm::ToolResultPayload;
use lha_llm::TranscriptItem;
use lha_protocol::memory_citation::MemoryCitation;
use tracing::debug;
use tracing::instrument;
use tracing::warn;

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
    let (item, memory_citation) = if ctx.sess.memory_citations_enabled().await {
        strip_memory_citation_from_item(item)
    } else {
        (item, None)
    };
    if matches!(
        &item,
        TranscriptItem::HostedActivity { activity_type, .. } if activity_type == "web_search"
    ) {
        crate::memories::pollution::maybe_mark_memory_polluted(
            &ctx.sess,
            &ctx.turn_context,
            "web_search",
        )
        .await;
    }

    if let Some(mut turn_item) = handle_non_tool_response_item(&item, plan_mode).await {
        if let (Some(citation), TurnItem::AgentMessage(agent_message)) =
            (memory_citation.clone(), &mut turn_item)
        {
            agent_message.memory_citation = Some(citation);
        }
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
    if let Some(memory_citation) = memory_citation.as_ref() {
        record_memory_citation_usage(&ctx.sess, memory_citation).await;
    }
    output.last_agent_message = last_assistant_message_from_item(&item, plan_mode);

    Ok(output)
}

pub(crate) async fn record_memory_citation_usage(sess: &Session, memory_citation: &MemoryCitation) {
    if let Some(state_db) = sess.state_db()
        && let Some(memories) = state_db.memories()
    {
        let thread_ids =
            lha_memories_read::citations::thread_ids_from_memory_citation(memory_citation);
        if let Err(err) = memories.record_stage1_output_usage(&thread_ids).await {
            warn!("failed to record memory citation usage: {err}");
        }
    }
}

pub(crate) fn strip_memory_citation_from_item(
    item: TranscriptItem,
) -> (TranscriptItem, Option<MemoryCitation>) {
    let TranscriptItem::Message {
        id,
        role,
        content,
        end_turn,
    } = item
    else {
        return (item, None);
    };
    if role != "assistant" {
        return (
            TranscriptItem::Message {
                id,
                role,
                content,
                end_turn,
            },
            None,
        );
    }

    let mut combined = String::new();
    let mut has_output_text = false;
    for item in &content {
        if let lha_protocol::models::ContentItem::OutputText { text } = item {
            has_output_text = true;
            combined.push_str(text);
        }
    }
    if !has_output_text {
        return (
            TranscriptItem::Message {
                id,
                role,
                content,
                end_turn,
            },
            None,
        );
    }

    let (stripped, blocks) = lha_memories_read::citations::strip_memory_citation_block(&combined);
    let citation = lha_memories_read::citations::parse_memory_citation(blocks);
    let content = vec![lha_protocol::models::ContentItem::OutputText { text: stripped }];
    (
        TranscriptItem::Message {
            id,
            role,
            content,
            end_turn,
        },
        citation,
    )
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
                        lha_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
                    })
                    .collect::<String>();
                let stripped = strip_proposed_plan_blocks(&combined);
                agent_message.content =
                    vec![lha_protocol::items::AgentMessageContent::Text { text: stripped }];
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
                lha_protocol::models::ContentItem::OutputText { text } => Some(text.as_str()),
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

    #[test]
    fn strip_memory_citation_from_assistant_item_attaches_citation() {
        let item = assistant_message(
            "answer\n<oai-mem-citation>\n<citation_entries>\nMEMORY.md:1-2|note=[used]\n</citation_entries>\n<rollout_ids>\n00000000-0000-0000-0000-000000000001\n</rollout_ids>\n</oai-mem-citation>",
        );

        let (item, citation) = strip_memory_citation_from_item(item);

        assert_eq!(assistant_text(&item), Some("answer"));
        assert_eq!(
            citation.expect("citation").rollout_ids,
            vec!["00000000-0000-0000-0000-000000000001"]
        );
    }

    #[test]
    fn strip_memory_citation_from_assistant_item_hides_malformed_tail() {
        let item = assistant_message("answer\n<oai-mem-citation>\nhidden");

        let (item, citation) = strip_memory_citation_from_item(item);

        assert_eq!(assistant_text(&item), Some("answer"));
        assert_eq!(citation, None);
    }

    fn assistant_message(text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![lha_protocol::models::ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }

    fn assistant_text(item: &TranscriptItem) -> Option<&str> {
        let TranscriptItem::Message { content, .. } = item else {
            return None;
        };
        content.iter().find_map(|content_item| match content_item {
            lha_protocol::models::ContentItem::OutputText { text } => Some(text.as_str()),
            lha_protocol::models::ContentItem::InputText { .. }
            | lha_protocol::models::ContentItem::InputImage { .. } => None,
        })
    }
}
