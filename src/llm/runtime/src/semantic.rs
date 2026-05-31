use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use futures::Stream;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::BaseInstructions;
use crate::Personality;
use crate::Result;
use crate::TokenUsage;
use crate::TranscriptItem;
use crate::prompt::FreeformTool;
use crate::prompt::FreeformToolFormat;
use crate::prompt::JsonSchema;
use crate::prompt::Prompt;
use crate::prompt::ResponseEvent;
use crate::prompt::ResponseStream;
use crate::prompt::ToolSpec;
use crate::types::ToolCallItem as ToolCallRequest;
#[cfg(test)]
use crate::types::ToolCallPayload;

pub type ItemHandle = String;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AdditionalProperties {
    Boolean(bool),
    Schema(Box<ToolInputSchema>),
}

impl From<bool> for AdditionalProperties {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<ToolInputSchema> for AdditionalProperties {
    fn from(value: ToolInputSchema) -> Self {
        Self::Schema(Box::new(value))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolInputSchema {
    Boolean {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    String {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    #[serde(alias = "integer")]
    Number {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Array {
        items: Box<ToolInputSchema>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Object {
        properties: std::collections::BTreeMap<String, ToolInputSchema>,
        #[serde(skip_serializing_if = "Option::is_none")]
        required: Option<Vec<String>>,
        #[serde(
            rename = "additionalProperties",
            skip_serializing_if = "Option::is_none"
        )]
        additional_properties: Option<AdditionalProperties>,
    },
}

impl From<JsonSchema> for ToolInputSchema {
    fn from(value: JsonSchema) -> Self {
        match value {
            JsonSchema::Boolean { description } => Self::Boolean { description },
            JsonSchema::String { description } => Self::String { description },
            JsonSchema::Number { description } => Self::Number { description },
            JsonSchema::Array { items, description } => Self::Array {
                items: Box::new(Self::from(*items)),
                description,
            },
            JsonSchema::Object {
                properties,
                required,
                additional_properties,
            } => Self::Object {
                properties: properties
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value)))
                    .collect(),
                required,
                additional_properties: additional_properties.map(Into::into),
            },
        }
    }
}

impl From<ToolInputSchema> for JsonSchema {
    fn from(value: ToolInputSchema) -> Self {
        match value {
            ToolInputSchema::Boolean { description } => Self::Boolean { description },
            ToolInputSchema::String { description } => Self::String { description },
            ToolInputSchema::Number { description } => Self::Number { description },
            ToolInputSchema::Array { items, description } => Self::Array {
                items: Box::new(Self::from(*items)),
                description,
            },
            ToolInputSchema::Object {
                properties,
                required,
                additional_properties,
            } => Self::Object {
                properties: properties
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value)))
                    .collect(),
                required,
                additional_properties: additional_properties.map(Into::into),
            },
        }
    }
}

impl From<crate::prompt::AdditionalProperties> for AdditionalProperties {
    fn from(value: crate::prompt::AdditionalProperties) -> Self {
        match value {
            crate::prompt::AdditionalProperties::Boolean(value) => Self::Boolean(value),
            crate::prompt::AdditionalProperties::Schema(schema) => {
                Self::Schema(Box::new(ToolInputSchema::from(*schema)))
            }
        }
    }
}

impl From<AdditionalProperties> for crate::prompt::AdditionalProperties {
    fn from(value: AdditionalProperties) -> Self {
        match value {
            AdditionalProperties::Boolean(value) => Self::Boolean(value),
            AdditionalProperties::Schema(schema) => Self::Schema(Box::new((*schema).into())),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct FunctionToolDescriptor {
    pub name: String,
    pub description: String,
    pub strict: bool,
    pub parameters: ToolInputSchema,
}

impl From<crate::prompt::ResponsesApiTool> for FunctionToolDescriptor {
    fn from(value: crate::prompt::ResponsesApiTool) -> Self {
        Self {
            name: value.name,
            description: value.description,
            strict: value.strict,
            parameters: value.parameters.into(),
        }
    }
}

impl From<FunctionToolDescriptor> for crate::prompt::ResponsesApiTool {
    fn from(value: FunctionToolDescriptor) -> Self {
        Self {
            name: value.name,
            description: value.description,
            strict: value.strict,
            parameters: value.parameters.into(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct FreeformToolDescriptor {
    pub name: String,
    pub description: String,
    pub format: FreeformToolDescriptorFormat,
}

impl From<FreeformTool> for FreeformToolDescriptor {
    fn from(value: FreeformTool) -> Self {
        Self {
            name: value.name,
            description: value.description,
            format: value.format.into(),
        }
    }
}

impl From<FreeformToolDescriptor> for FreeformTool {
    fn from(value: FreeformToolDescriptor) -> Self {
        Self {
            name: value.name,
            description: value.description,
            format: value.format.into(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct FreeformToolDescriptorFormat {
    pub r#type: String,
    pub syntax: String,
    pub definition: String,
}

impl From<FreeformToolFormat> for FreeformToolDescriptorFormat {
    fn from(value: FreeformToolFormat) -> Self {
        Self {
            r#type: value.r#type,
            syntax: value.syntax,
            definition: value.definition,
        }
    }
}

impl From<FreeformToolDescriptorFormat> for FreeformToolFormat {
    fn from(value: FreeformToolDescriptorFormat) -> Self {
        Self {
            r#type: value.r#type,
            syntax: value.syntax,
            definition: value.definition,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum ToolDescriptor {
    #[serde(rename = "function")]
    Function(FunctionToolDescriptor),
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
            Self::WebSearch { .. } => "web_search",
            Self::Freeform(tool) => tool.name.as_str(),
        }
    }
}

impl From<ToolSpec> for ToolDescriptor {
    fn from(value: ToolSpec) -> Self {
        match value {
            ToolSpec::Function(tool) => Self::Function(tool.into()),
            ToolSpec::WebSearch {
                external_web_access,
            } => Self::WebSearch {
                external_web_access,
            },
            ToolSpec::Freeform(tool) => Self::Freeform(tool.into()),
        }
    }
}

impl From<ToolDescriptor> for ToolSpec {
    fn from(value: ToolDescriptor) -> Self {
        match value {
            ToolDescriptor::Function(tool) => Self::Function(tool.into()),
            ToolDescriptor::WebSearch {
                external_web_access,
            } => Self::WebSearch {
                external_web_access,
            },
            ToolDescriptor::Freeform(tool) => Self::Freeform(tool.into()),
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
    pub conversation: Vec<TranscriptItem>,
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
    AssistantMessage { item: TranscriptItem },
    Reasoning { item: TranscriptItem },
    HostedActivity { item: TranscriptItem },
}

impl SemanticOutputItem {
    pub fn item(&self) -> &TranscriptItem {
        match self {
            Self::AssistantMessage { item }
            | Self::Reasoning { item }
            | Self::HostedActivity { item } => item,
        }
    }

    pub fn into_item(self) -> TranscriptItem {
        match self {
            Self::AssistantMessage { item }
            | Self::Reasoning { item }
            | Self::HostedActivity { item } => item,
        }
    }

    fn from_transcript_item(item: TranscriptItem) -> Option<Self> {
        match &item {
            TranscriptItem::Message { role, .. } if role == "assistant" => {
                Some(Self::AssistantMessage { item })
            }
            TranscriptItem::Reasoning { .. } => Some(Self::Reasoning { item }),
            TranscriptItem::HostedActivity { .. } => Some(Self::HostedActivity { item }),
            _ => None,
        }
    }

    fn suggested_handle(&self) -> Option<String> {
        match self.item() {
            TranscriptItem::Message { id, .. } => id.clone(),
            TranscriptItem::Reasoning { id, .. } => Some(id.clone()),
            TranscriptItem::HostedActivity { id, .. } => id.clone(),
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
            Self::ToolCall(call) => ResponseEvent::OutputItemDone(call.to_transcript_item()),
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
        ResponseEvent::OutputItemAdded(item) => SemanticOutputItem::from_transcript_item(item)
            .map(|item| {
                let handle = item
                    .suggested_handle()
                    .unwrap_or_else(|| next_item_handle(next_synthetic_handle));
                *active_handle = Some(handle.clone());
                vec![TurnEvent::ItemStarted { handle, item }]
            })
            .unwrap_or_default(),
        ResponseEvent::OutputItemDone(item) => {
            if let Some(call) = ToolCallRequest::from_transcript_item(item.clone()) {
                vec![TurnEvent::ToolCall(call)]
            } else if let Some(item) = SemanticOutputItem::from_transcript_item(item) {
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
    use lha_llm_types::ContentItem;

    #[test]
    fn tool_call_request_is_derived_from_function_call_items() {
        let item = TranscriptItem::ToolCall {
            id: Some("fn-1".to_string()),
            call_id: "call-1".to_string(),
            tool_name: "shell".to_string(),
            payload: ToolCallPayload::JsonArguments {
                arguments: "{\"command\":[\"pwd\"]}".to_string(),
            },
        };

        let call = ToolCallRequest::from_transcript_item(item).expect("tool call");

        assert_eq!(call.tool_name, "shell");
        assert_eq!(call.call_id, "call-1");
        assert_eq!(
            call.payload,
            ToolCallPayload::JsonArguments {
                arguments: "{\"command\":[\"pwd\"]}".to_string(),
            }
        );
        assert_eq!(
            call.to_transcript_item(),
            TranscriptItem::ToolCall {
                id: Some("fn-1".to_string()),
                call_id: "call-1".to_string(),
                tool_name: "shell".to_string(),
                payload: ToolCallPayload::JsonArguments {
                    arguments: "{\"command\":[\"pwd\"]}".to_string(),
                },
            }
        );
    }

    #[test]
    fn semantic_output_items_roundtrip_to_legacy_events() {
        let item = TranscriptItem::Message {
            id: Some("msg-1".to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "hello".to_string(),
            }],
            end_turn: None,
        };
        let semantic_item =
            SemanticOutputItem::from_transcript_item(item.clone()).expect("assistant item");

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
