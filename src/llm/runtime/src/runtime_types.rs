use crate::provider::RuntimeEndpoint;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub supports_parallel_tool_calls: bool,
    pub enforce_declared_tool_names: bool,
    pub supports_dynamic_context_window_probe: bool,
    pub supports_reasoning_summaries: bool,
    pub supports_output_schema: bool,
    pub supports_remote_compaction: bool,
}

impl RuntimeCapabilities {
    pub(crate) fn from_endpoint_and_model(
        endpoint: &RuntimeEndpoint,
        model_info: &codex_protocol::openai_models::ModelInfo,
    ) -> Self {
        Self {
            supports_parallel_tool_calls: model_info.supports_parallel_tool_calls,
            enforce_declared_tool_names: endpoint.enforce_declared_tool_names(),
            supports_dynamic_context_window_probe: endpoint.supports_dynamic_context_window_probe(),
            supports_reasoning_summaries: model_info.supports_reasoning_summaries,
            supports_output_schema: endpoint.supports_output_schema(),
            supports_remote_compaction: endpoint.supports_remote_compaction(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeMetadata {
    pub endpoint_name: String,
    pub model: String,
}
