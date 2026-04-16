use codex_protocol::config_types::Verbosity;
use codex_protocol::config_types::WebSearchMode;

#[derive(Debug, Clone, Default)]
pub struct LlmRuntimeConfig {
    pub model_provider_id: String,
    pub show_raw_agent_reasoning: bool,
    pub model_verbosity: Option<Verbosity>,
    pub web_search_mode: Option<WebSearchMode>,
    pub responses_websockets_enabled: bool,
    pub experimental_beta_feature_keys: Vec<String>,
    pub sse_fixture_path: Option<String>,
}
