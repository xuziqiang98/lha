use std::sync::Arc;
use std::sync::Mutex;

use adam_llm::RuntimeBuildSpec;
use adam_llm::RuntimeCapabilities;
use adam_llm::RuntimeClient;
use adam_llm::RuntimeClientFactory;
use adam_llm::RuntimeSession;
use adam_llm::TurnRequest;
pub use adam_llm::WEB_SEARCH_ELIGIBLE_HEADER;
use adam_otel::OtelManager;
use adam_protocol::ThreadId;
use adam_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use adam_protocol::models::TranscriptItem;
use adam_protocol::openai_models::ModelInfo;
use adam_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use adam_protocol::protocol::SessionSource;

use crate::AuthManager;
use crate::config::Config;
use crate::default_client::build_reqwest_client;
use crate::dynamic_context_window::DynamicContextWindowFailure;
use crate::dynamic_context_window::DynamicContextWindowState;
use crate::dynamic_context_window::DynamicContextWindowSuccess;
use crate::error::Result;
use crate::runtime_builder::AgentAuthSource;
use adam_llm::RuntimeEndpoint;

struct TurnRuntimeState {
    config: Arc<Config>,
    auth_manager: Option<Arc<AuthManager>>,
    #[cfg_attr(not(test), allow(dead_code))]
    runtime_factory: Arc<dyn RuntimeClientFactory>,
    #[cfg_attr(not(test), allow(dead_code))]
    conversation_id: ThreadId,
    model_info: ModelInfo,
    dynamic_context_window: Option<Arc<Mutex<DynamicContextWindowState>>>,
    otel_manager: OtelManager,
    endpoint: RuntimeEndpoint,
    effort: Option<ReasoningEffortConfig>,
    summary: ReasoningSummaryConfig,
    session_source: SessionSource,
    runtime: Arc<dyn RuntimeClient>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DynamicContextWindowStatus {
    pub(crate) current_context_window: i64,
    pub(crate) locked: bool,
}

#[derive(Clone)]
pub(crate) struct TurnRuntime {
    state: Arc<TurnRuntimeState>,
}

#[allow(clippy::too_many_arguments)]
impl TurnRuntime {
    pub(crate) fn new_with_dynamic_context_window(
        config: Arc<Config>,
        auth_manager: Option<Arc<AuthManager>>,
        runtime_factory: Arc<dyn RuntimeClientFactory>,
        model_info: ModelInfo,
        dynamic_context_window: Option<Arc<Mutex<DynamicContextWindowState>>>,
        otel_manager: OtelManager,
        endpoint: RuntimeEndpoint,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        conversation_id: ThreadId,
        session_source: SessionSource,
    ) -> Self {
        let mut endpoint = endpoint;
        if !config
            .features
            .enabled(crate::features::Feature::ResponsesWebsockets)
        {
            endpoint.set_realtime_turn_streaming_enabled(false);
        }

        let auth_source = AgentAuthSource::boxed(auth_manager.clone(), endpoint.clone());
        let runtime = runtime_factory.build_client(RuntimeBuildSpec {
            endpoint_id: config.model_provider_id.clone(),
            auth_source,
            http_client: build_reqwest_client(),
            model_info: model_info.clone().into(),
            otel_manager: otel_manager.clone(),
            endpoint: endpoint.clone(),
            effort,
            summary,
            session_id: conversation_id.to_string(),
            origin_tag: Some(session_source_to_origin_tag(&session_source)),
            show_raw_agent_reasoning: config.show_raw_agent_reasoning,
            model_verbosity: config.model_verbosity,
            web_search_mode: config.web_search_mode,
            experimental_beta_feature_keys: crate::features::FEATURES
                .iter()
                .filter_map(|spec| {
                    if spec.stage.experimental_menu_description().is_some()
                        && config.features.enabled(spec.id)
                    {
                        Some(spec.key.to_string())
                    } else {
                        None
                    }
                })
                .collect(),
            sse_fixture_path: (*crate::flags::CODEX_RS_SSE_FIXTURE).map(str::to_string),
        });

        Self {
            state: Arc::new(TurnRuntimeState {
                config,
                auth_manager,
                runtime_factory,
                conversation_id,
                model_info,
                dynamic_context_window,
                otel_manager,
                endpoint,
                effort,
                summary,
                session_source,
                runtime,
            }),
        }
    }

    pub(crate) fn new_session(&self) -> Box<dyn RuntimeSession> {
        self.state.runtime.new_session()
    }

    fn effective_model_info(&self) -> ModelInfo {
        let mut model_info = self.state.model_info.clone();
        if let Some(dynamic_context_window) = &self.state.dynamic_context_window
            && model_info.context_window.is_none()
        {
            let dynamic_context_window = dynamic_context_window
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            model_info.context_window = Some(dynamic_context_window.current_context_window());
        }
        model_info
    }

    pub fn get_model_context_window(&self) -> Option<i64> {
        let model_info = self.effective_model_info();
        let effective_context_window_percent = model_info.effective_context_window_percent;
        model_info.context_window.map(|context_window| {
            context_window.saturating_mul(effective_context_window_percent) / 100
        })
    }

    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.state.config)
    }

    pub fn endpoint(&self) -> RuntimeEndpoint {
        self.state.endpoint.clone()
    }

    pub fn get_otel_manager(&self) -> OtelManager {
        self.state.otel_manager.clone()
    }

    pub(crate) fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.state.auth_manager.clone()
    }

    pub(crate) fn endpoint_name(&self) -> &str {
        self.state.endpoint.name.as_str()
    }

    pub fn get_session_source(&self) -> SessionSource {
        self.state.session_source.clone()
    }

    pub fn get_model(&self) -> String {
        self.state.model_info.slug.clone()
    }

    pub fn get_model_info(&self) -> ModelInfo {
        self.effective_model_info()
    }

    pub fn get_reasoning_effort(&self) -> Option<ReasoningEffortConfig> {
        self.state.effort
    }

    pub fn get_reasoning_summary(&self) -> ReasoningSummaryConfig {
        self.state.summary
    }

    pub fn runtime_capabilities(&self) -> RuntimeCapabilities {
        self.state.runtime.capabilities()
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn derive_runtime(
        &self,
        config: Arc<Config>,
        model_info: ModelInfo,
        dynamic_context_window: Option<Arc<Mutex<DynamicContextWindowState>>>,
        otel_manager: OtelManager,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        session_source: SessionSource,
    ) -> Self {
        Self::new_with_dynamic_context_window(
            config,
            self.state.auth_manager.clone(),
            Arc::clone(&self.state.runtime_factory),
            model_info,
            dynamic_context_window,
            otel_manager,
            self.state.endpoint.clone(),
            effort,
            summary,
            self.state.conversation_id,
            session_source,
        )
    }

    pub(crate) fn record_dynamic_context_window_success(
        &self,
        input_tokens: i64,
    ) -> Option<DynamicContextWindowSuccess> {
        let effective_context_window_percent =
            self.state.model_info.effective_context_window_percent;
        self.state
            .dynamic_context_window
            .as_ref()
            .and_then(|window| {
                window
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .record_success(input_tokens, effective_context_window_percent)
            })
    }

    pub(crate) fn should_defer_auto_compact_until_after_dynamic_probe(&self) -> bool {
        self.state
            .dynamic_context_window
            .as_ref()
            .is_some_and(|window| {
                !window
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_locked()
            })
    }

    pub(crate) fn dynamic_context_window_auto_compact_limit(&self) -> Option<i64> {
        self.state
            .dynamic_context_window
            .as_ref()
            .and_then(|_| self.get_model_context_window())
    }

    pub(crate) fn dynamic_context_window_status(&self) -> Option<DynamicContextWindowStatus> {
        self.state.dynamic_context_window.as_ref().map(|window| {
            let window = window
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            DynamicContextWindowStatus {
                current_context_window: window.current_context_window(),
                locked: window.is_locked(),
            }
        })
    }

    pub(crate) fn should_preflight_dynamic_context_window_compact(
        &self,
        input_tokens: i64,
    ) -> bool {
        let effective_context_window_percent =
            self.state.model_info.effective_context_window_percent;
        self.state
            .dynamic_context_window
            .as_ref()
            .is_some_and(|window| {
                window
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .should_preflight_compact(input_tokens, effective_context_window_percent)
            })
    }

    pub(crate) fn record_dynamic_context_window_probe_failure(
        &self,
        turn_id: &str,
        input_tokens: i64,
    ) -> DynamicContextWindowFailure {
        let effective_context_window_percent =
            self.state.model_info.effective_context_window_percent;
        self.state
            .dynamic_context_window
            .as_ref()
            .map(|window| {
                window
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .record_probe_failure(turn_id, input_tokens, effective_context_window_percent)
            })
            .unwrap_or(DynamicContextWindowFailure {
                should_retry: false,
                learned_context_window: None,
            })
    }

    #[cfg(test)]
    pub(crate) fn dynamic_context_window(&self) -> Option<Arc<Mutex<DynamicContextWindowState>>> {
        self.state.dynamic_context_window.clone()
    }

    pub(crate) fn estimated_input_tokens_for_turn_request(
        &self,
        request: &TurnRequest,
    ) -> Option<i64> {
        self.state.runtime.estimated_input_tokens(request)
    }

    pub async fn compact_turn_request(&self, request: &TurnRequest) -> Result<Vec<TranscriptItem>> {
        self.state
            .runtime
            .compact_conversation_history(request)
            .await
            .map_err(Into::into)
    }
}

fn session_source_to_origin_tag(session_source: &SessionSource) -> String {
    match session_source {
        SessionSource::Cli => "cli".to_string(),
        SessionSource::VSCode => "vscode".to_string(),
        SessionSource::Exec => "exec".to_string(),
        SessionSource::Mcp => "mcp".to_string(),
        SessionSource::SubAgent(sub) => match sub {
            adam_protocol::protocol::SubAgentSource::Review => "review".to_string(),
            adam_protocol::protocol::SubAgentSource::Compact => "compact".to_string(),
            adam_protocol::protocol::SubAgentSource::ThreadSpawn { .. } => {
                "thread_spawn".to_string()
            }
            adam_protocol::protocol::SubAgentSource::Other(label) => label.clone(),
        },
        SessionSource::Unknown => "unknown".to_string(),
    }
}

impl std::fmt::Debug for TurnRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnRuntime")
            .field("model", &self.state.model_info.slug)
            .field("endpoint", &self.state.endpoint.name)
            .finish()
    }
}
