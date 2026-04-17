use crate::prompt::Prompt;
use crate::prompt::ResponseEvent;
use crate::prompt::ResponseStream;
use crate::provider::WireApi;

pub type AgentTurnInput = Prompt;
pub type AgentEvent = ResponseEvent;
pub type AgentEventStream = ResponseStream;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub supports_parallel_tool_calls: bool,
    pub enforce_declared_tool_names: bool,
    pub supports_dynamic_context_window_probe: bool,
    pub supports_reasoning_summaries: bool,
    pub supports_output_schema: bool,
    pub supports_websocket_transport: bool,
}

impl RuntimeCapabilities {
    pub fn from_wire_and_model(
        wire_api: WireApi,
        model_info: &codex_protocol::openai_models::ModelInfo,
        supports_websockets: bool,
    ) -> Self {
        Self {
            supports_parallel_tool_calls: model_info.supports_parallel_tool_calls,
            enforce_declared_tool_names: wire_api == WireApi::Messages,
            supports_dynamic_context_window_probe: matches!(
                wire_api,
                WireApi::Chat | WireApi::Messages
            ),
            supports_reasoning_summaries: model_info.supports_reasoning_summaries,
            supports_output_schema: wire_api == WireApi::Responses,
            supports_websocket_transport: supports_websockets && wire_api == WireApi::Responses,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMetadata {
    pub provider_name: String,
    pub model: String,
    pub wire_api: WireApi,
    pub stream_max_retries: u64,
}
