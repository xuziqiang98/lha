use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use codex_protocol::config_types::Personality;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ConversationItem;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::SandboxPermissions;
use codex_protocol::models::ShellToolCallParams;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::TokenUsage;
use futures::Stream;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::Result;
use crate::prompt::FreeformTool;
use crate::prompt::FreeformToolFormat;
use crate::prompt::JsonSchema;
use crate::prompt::Prompt;
use crate::prompt::ResponseEvent;
use crate::prompt::ResponseStream;
use crate::prompt::ResponsesApiTool;
use crate::prompt::ToolSpec;

pub type FunctionToolDescriptor = ResponsesApiTool;
pub type ToolInputSchema = JsonSchema;
pub type FreeformToolDescriptor = FreeformTool;
pub type FreeformToolDescriptorFormat = FreeformToolFormat;
pub type ItemHandle = String;

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum ToolDescriptor {
    #[serde(rename = "function")]
    Function(FunctionToolDescriptor),
    #[serde(rename = "local_shell")]
    LocalShell {},
    #[serde(rename = "web_search")]
    WebSearch {
        #[serde(skip_serializing_if = "Option::is_none")]
        external_web_access: Option<bool>,
    },
    #[serde(rename = "custom")]
    Freeform(FreeformToolDescriptor),
}

impl ToolDescriptor {
    pub fn name(&self) -> &str {
        match self {
            Self::Function(tool) => tool.name.as_str(),
            Self::LocalShell {} => "local_shell",
            Self::WebSearch { .. } => "web_search",
            Self::Freeform(tool) => tool.name.as_str(),
        }
    }

    pub fn to_legacy_tool_spec(&self) -> ToolSpec {
        self.clone().into()
    }
}

impl From<ToolSpec> for ToolDescriptor {
    fn from(value: ToolSpec) -> Self {
        match value {
            ToolSpec::Function(tool) => Self::Function(tool),
            ToolSpec::LocalShell {} => Self::LocalShell {},
            ToolSpec::WebSearch {
                external_web_access,
            } => Self::WebSearch {
                external_web_access,
            },
            ToolSpec::Freeform(tool) => Self::Freeform(tool),
        }
    }
}

impl From<ToolDescriptor> for ToolSpec {
    fn from(value: ToolDescriptor) -> Self {
        match value {
            ToolDescriptor::Function(tool) => Self::Function(tool),
            ToolDescriptor::LocalShell {} => Self::LocalShell {},
            ToolDescriptor::WebSearch {
                external_web_access,
            } => Self::WebSearch {
                external_web_access,
            },
            ToolDescriptor::Freeform(tool) => Self::Freeform(tool),
        }
    }
}

impl From<&ToolDescriptor> for ToolSpec {
    fn from(value: &ToolDescriptor) -> Self {
        value.clone().into()
    }
}

#[derive(Default, Debug, Clone)]
pub struct TurnRequest {
    pub conversation: Vec<ConversationItem>,
    pub tools: Vec<ToolDescriptor>,
    pub parallel_tool_calls: bool,
    pub base_instructions: BaseInstructions,
    pub personality: Option<Personality>,
    pub output_schema: Option<Value>,
}

impl TurnRequest {
    pub fn to_prompt(&self) -> Prompt {
        Prompt {
            input: self.conversation.clone(),
            tools: self.tools.iter().map(ToolSpec::from).collect(),
            parallel_tool_calls: self.parallel_tool_calls,
            base_instructions: self.base_instructions.clone(),
            personality: self.personality,
            output_schema: self.output_schema.clone(),
        }
    }
}

impl From<TurnRequest> for Prompt {
    fn from(value: TurnRequest) -> Self {
        value.to_prompt()
    }
}

impl From<&TurnRequest> for Prompt {
    fn from(value: &TurnRequest) -> Self {
        value.to_prompt()
    }
}

impl From<Prompt> for TurnRequest {
    fn from(value: Prompt) -> Self {
        Self {
            conversation: value.input,
            tools: value.tools.into_iter().map(ToolDescriptor::from).collect(),
            parallel_tool_calls: value.parallel_tool_calls,
            base_instructions: value.base_instructions,
            personality: value.personality,
            output_schema: value.output_schema,
        }
    }
}

impl From<&Prompt> for TurnRequest {
    fn from(value: &Prompt) -> Self {
        value.clone().into()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SemanticOutputItem {
    AssistantMessage { item: ConversationItem },
    Reasoning { item: ConversationItem },
    WebSearch { item: ConversationItem },
}

impl SemanticOutputItem {
    pub fn item(&self) -> &ConversationItem {
        match self {
            Self::AssistantMessage { item }
            | Self::Reasoning { item }
            | Self::WebSearch { item } => item,
        }
    }

    pub fn into_item(self) -> ConversationItem {
        match self {
            Self::AssistantMessage { item }
            | Self::Reasoning { item }
            | Self::WebSearch { item } => item,
        }
    }

    fn from_conversation_item(item: ConversationItem) -> Option<Self> {
        match &item {
            ConversationItem::Message { role, .. } if role == "assistant" => {
                Some(Self::AssistantMessage { item })
            }
            ConversationItem::Reasoning { .. } => Some(Self::Reasoning { item }),
            ConversationItem::WebSearchCall { .. } => Some(Self::WebSearch { item }),
            _ => None,
        }
    }

    fn suggested_handle(&self) -> Option<String> {
        match self.item() {
            ConversationItem::Message { id, .. } => id.clone(),
            ConversationItem::Reasoning { id, .. } => Some(id.clone()),
            ConversationItem::WebSearchCall { id, .. } => id.clone(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallPayload {
    Function { arguments: String },
    Custom { input: String },
    LocalShell { params: ShellToolCallParams },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRequest {
    pub tool_name: String,
    pub call_id: String,
    pub payload: ToolCallPayload,
    pub item: ConversationItem,
}

impl ToolCallRequest {
    pub fn from_conversation_item(item: ConversationItem) -> Option<Self> {
        match item.clone() {
            ConversationItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => Some(Self {
                tool_name: name,
                call_id,
                payload: ToolCallPayload::Function { arguments },
                item,
            }),
            ConversationItem::CustomToolCall {
                name,
                input,
                call_id,
                ..
            } => Some(Self {
                tool_name: name,
                call_id,
                payload: ToolCallPayload::Custom { input },
                item,
            }),
            ConversationItem::LocalShellCall {
                id,
                call_id,
                action,
                ..
            } => {
                let call_id = call_id.or(id).unwrap_or_default();
                let LocalShellAction::Exec(exec) = action;
                Some(Self {
                    tool_name: "local_shell".to_string(),
                    call_id,
                    payload: ToolCallPayload::LocalShell {
                        params: ShellToolCallParams {
                            command: exec.command,
                            workdir: exec.working_directory,
                            timeout_ms: exec.timeout_ms,
                            sandbox_permissions: Some(SandboxPermissions::UseDefault),
                            prefix_rule: None,
                            justification: None,
                        },
                    },
                    item,
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    Created,
    RuntimeNotice(RuntimeNotice),
    ItemStarted {
        handle: ItemHandle,
        item: SemanticOutputItem,
    },
    ItemCompleted {
        handle: ItemHandle,
        item: SemanticOutputItem,
    },
    ToolCall(ToolCallRequest),
    OutputTextDelta {
        handle: ItemHandle,
        delta: String,
    },
    ProposedPlanDelta {
        handle: ItemHandle,
        delta: String,
    },
    ProposedPlanDone {
        handle: ItemHandle,
        text: String,
    },
    ReasoningSummaryDelta {
        handle: ItemHandle,
        delta: String,
        summary_index: i64,
    },
    ReasoningContentDelta {
        handle: ItemHandle,
        delta: String,
        content_index: i64,
    },
    ReasoningSummaryPartAdded {
        handle: ItemHandle,
        summary_index: i64,
    },
    ServerReasoningIncluded(bool),
    Completed {
        response_id: String,
        token_usage: Option<TokenUsage>,
    },
    RateLimits(RateLimitSnapshot),
    ModelsEtag(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeNotice {
    pub kind: RuntimeNoticeKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeNoticeKind {
    Reconnecting,
    TransportFallback,
    CompatibilityRetry,
}

impl TurnEvent {
    pub fn to_legacy_response_event(&self) -> Option<ResponseEvent> {
        Some(match self {
            Self::Created => ResponseEvent::Created,
            Self::RuntimeNotice(_) => return None,
            Self::ItemStarted { item, .. } => ResponseEvent::OutputItemAdded(item.item().clone()),
            Self::ItemCompleted { item, .. } => ResponseEvent::OutputItemDone(item.item().clone()),
            Self::ToolCall(call) => ResponseEvent::OutputItemDone(call.item.clone()),
            Self::OutputTextDelta { delta, .. } => ResponseEvent::OutputTextDelta(delta.clone()),
            Self::ProposedPlanDelta { delta, .. } => {
                ResponseEvent::ProposedPlanDelta(delta.clone())
            }
            Self::ProposedPlanDone { text, .. } => ResponseEvent::ProposedPlanDone(text.clone()),
            Self::ReasoningSummaryDelta {
                delta,
                summary_index,
                ..
            } => ResponseEvent::ReasoningSummaryDelta {
                delta: delta.clone(),
                summary_index: *summary_index,
            },
            Self::ReasoningContentDelta {
                delta,
                content_index,
                ..
            } => ResponseEvent::ReasoningContentDelta {
                delta: delta.clone(),
                content_index: *content_index,
            },
            Self::ReasoningSummaryPartAdded { summary_index, .. } => {
                ResponseEvent::ReasoningSummaryPartAdded {
                    summary_index: *summary_index,
                }
            }
            Self::ServerReasoningIncluded(included) => {
                ResponseEvent::ServerReasoningIncluded(*included)
            }
            Self::Completed {
                response_id,
                token_usage,
            } => ResponseEvent::Completed {
                response_id: response_id.clone(),
                token_usage: token_usage.clone(),
            },
            Self::RateLimits(snapshot) => ResponseEvent::RateLimits(snapshot.clone()),
            Self::ModelsEtag(etag) => ResponseEvent::ModelsEtag(etag.clone()),
        })
    }
}

pub struct TurnEventStream {
    pub(crate) rx_event: mpsc::Receiver<Result<TurnEvent>>,
}

impl TurnEventStream {
    pub fn from_receiver(rx_event: mpsc::Receiver<Result<TurnEvent>>) -> Self {
        Self { rx_event }
    }
}

impl Stream for TurnEventStream {
    type Item = Result<TurnEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx_event.poll_recv(cx)
    }
}

pub(crate) fn adapt_response_stream(stream: ResponseStream) -> TurnEventStream {
    let (tx_event, rx_event) = mpsc::channel::<Result<TurnEvent>>(1600);

    tokio::spawn(async move {
        let mut stream = stream;
        let mut next_synthetic_handle = 0usize;
        let mut active_handle: Option<ItemHandle> = None;

        while let Some(event) = stream.next().await {
            let adapted = match event {
                Ok(event) => {
                    adapt_response_event(event, &mut active_handle, &mut next_synthetic_handle)
                }
                Err(err) => Err(err),
            };

            let events = match adapted {
                Ok(events) => events,
                Err(err) => {
                    if tx_event.send(Err(err)).await.is_err() {
                        return;
                    }
                    continue;
                }
            };

            for event in events {
                if tx_event.send(Ok(event)).await.is_err() {
                    return;
                }
            }
        }
    });

    TurnEventStream::from_receiver(rx_event)
}

fn adapt_response_event(
    event: ResponseEvent,
    active_handle: &mut Option<ItemHandle>,
    next_synthetic_handle: &mut usize,
) -> Result<Vec<TurnEvent>> {
    let event = match event {
        ResponseEvent::Created => vec![TurnEvent::Created],
        ResponseEvent::OutputItemAdded(item) => SemanticOutputItem::from_conversation_item(item)
            .map(|item| {
                let handle = item
                    .suggested_handle()
                    .unwrap_or_else(|| next_item_handle(next_synthetic_handle));
                *active_handle = Some(handle.clone());
                vec![TurnEvent::ItemStarted { handle, item }]
            })
            .unwrap_or_default(),
        ResponseEvent::OutputItemDone(item) => {
            if let Some(call) = ToolCallRequest::from_conversation_item(item.clone()) {
                vec![TurnEvent::ToolCall(call)]
            } else if let Some(item) = SemanticOutputItem::from_conversation_item(item) {
                let handle = active_handle
                    .take()
                    .or_else(|| item.suggested_handle())
                    .unwrap_or_else(|| next_item_handle(next_synthetic_handle));
                vec![TurnEvent::ItemCompleted { handle, item }]
            } else {
                Vec::new()
            }
        }
        ResponseEvent::OutputTextDelta(delta) => active_handle
            .clone()
            .map(|handle| vec![TurnEvent::OutputTextDelta { handle, delta }])
            .unwrap_or_default(),
        ResponseEvent::ProposedPlanDelta(delta) => active_handle
            .clone()
            .map(|handle| vec![TurnEvent::ProposedPlanDelta { handle, delta }])
            .unwrap_or_default(),
        ResponseEvent::ProposedPlanDone(text) => active_handle
            .clone()
            .map(|handle| vec![TurnEvent::ProposedPlanDone { handle, text }])
            .unwrap_or_default(),
        ResponseEvent::ReasoningSummaryDelta {
            delta,
            summary_index,
        } => active_handle
            .clone()
            .map(|handle| {
                vec![TurnEvent::ReasoningSummaryDelta {
                    handle,
                    delta,
                    summary_index,
                }]
            })
            .unwrap_or_default(),
        ResponseEvent::ReasoningContentDelta {
            delta,
            content_index,
        } => active_handle
            .clone()
            .map(|handle| {
                vec![TurnEvent::ReasoningContentDelta {
                    handle,
                    delta,
                    content_index,
                }]
            })
            .unwrap_or_default(),
        ResponseEvent::ReasoningSummaryPartAdded { summary_index } => active_handle
            .clone()
            .map(|handle| {
                vec![TurnEvent::ReasoningSummaryPartAdded {
                    handle,
                    summary_index,
                }]
            })
            .unwrap_or_default(),
        ResponseEvent::ServerReasoningIncluded(included) => {
            vec![TurnEvent::ServerReasoningIncluded(included)]
        }
        ResponseEvent::Completed {
            response_id,
            token_usage,
        } => {
            active_handle.take();
            vec![TurnEvent::Completed {
                response_id,
                token_usage,
            }]
        }
        ResponseEvent::RateLimits(snapshot) => vec![TurnEvent::RateLimits(snapshot)],
        ResponseEvent::ModelsEtag(etag) => vec![TurnEvent::ModelsEtag(etag)],
    };

    Ok(event)
}

fn next_item_handle(next_synthetic_handle: &mut usize) -> String {
    let handle = format!("llm-item-{}", *next_synthetic_handle);
    *next_synthetic_handle += 1;
    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    #[test]
    fn tool_call_request_is_derived_from_function_call_items() {
        let item = ConversationItem::FunctionCall {
            id: Some("fn-1".to_string()),
            name: "shell".to_string(),
            arguments: "{\"command\":[\"pwd\"]}".to_string(),
            call_id: "call-1".to_string(),
        };

        let call = ToolCallRequest::from_conversation_item(item.clone()).expect("tool call");

        assert_eq!(call.tool_name, "shell");
        assert_eq!(call.call_id, "call-1");
        assert_eq!(call.item, item);
        assert_eq!(
            call.payload,
            ToolCallPayload::Function {
                arguments: "{\"command\":[\"pwd\"]}".to_string(),
            }
        );
    }

    #[test]
    fn semantic_output_items_roundtrip_to_legacy_events() {
        let item = ConversationItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "hello".to_string(),
            }],
            end_turn: None,
        };
        let semantic_item =
            SemanticOutputItem::from_conversation_item(item.clone()).expect("assistant item");

        match (TurnEvent::ItemStarted {
            handle: "msg-1".to_string(),
            item: semantic_item,
        })
        .to_legacy_response_event()
        .expect("legacy response event")
        {
            ResponseEvent::OutputItemAdded(actual) => assert_eq!(actual, item),
            other => panic!("expected OutputItemAdded, got {other:?}"),
        }
    }
}
