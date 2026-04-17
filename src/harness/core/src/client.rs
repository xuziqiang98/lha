use std::sync::Arc;
use std::sync::Mutex;

use futures::StreamExt;
use tokio::sync::mpsc;

use codex_llm::AgentRuntime;
use codex_llm::AgentRuntimeFactory;
use codex_llm::AgentRuntimeSession;
use codex_llm::DefaultAgentRuntimeFactory;
use codex_llm::RuntimeBuildSpec;
use codex_llm::RuntimeCapabilities;
use codex_llm::RuntimeMetadata;
pub use codex_llm::WEB_SEARCH_ELIGIBLE_HEADER;
use codex_otel::OtelManager;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::SessionSource;

use crate::AuthManager;
use crate::chat_role_compatibility::ChatRoleCompatibilityHandle;
use crate::chat_role_compatibility::ChatRoleCompatibilityState;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::client_common::ResponseStream;
use crate::config::Config;
use crate::default_client::build_reqwest_client;
use crate::dynamic_context_window::DynamicContextWindowFailure;
use crate::dynamic_context_window::DynamicContextWindowState;
use crate::dynamic_context_window::DynamicContextWindowSuccess;
use crate::error::Result;
use crate::llm_adapter::CoreAuthSource;
use crate::llm_adapter::llm_runtime_config_from_core_config;
use crate::model_provider_info::ModelProviderInfo;
use crate::transport_manager::TransportManager;

struct ModelClientState {
    config: Arc<Config>,
    auth_manager: Option<Arc<AuthManager>>,
    model_info: ModelInfo,
    dynamic_context_window: Option<Arc<Mutex<DynamicContextWindowState>>>,
    #[cfg_attr(not(test), allow(dead_code))]
    chat_role_compatibility: Option<ChatRoleCompatibilityHandle>,
    otel_manager: OtelManager,
    provider: ModelProviderInfo,
    effort: Option<ReasoningEffortConfig>,
    summary: ReasoningSummaryConfig,
    session_source: SessionSource,
    transport_manager: TransportManager,
    runtime: Arc<dyn AgentRuntime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DynamicContextWindowStatus {
    pub(crate) current_context_window: i64,
    pub(crate) locked: bool,
}

#[derive(Clone)]
pub struct ModelClient {
    state: Arc<ModelClientState>,
}

pub struct ModelClientSession {
    inner: Box<dyn AgentRuntimeSession>,
}

#[allow(clippy::too_many_arguments)]
impl ModelClient {
    pub fn new(
        config: Arc<Config>,
        auth_manager: Option<Arc<AuthManager>>,
        model_info: ModelInfo,
        otel_manager: OtelManager,
        provider: ModelProviderInfo,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        conversation_id: ThreadId,
        session_source: SessionSource,
        transport_manager: TransportManager,
    ) -> Self {
        Self::new_with_dynamic_context_window(
            config,
            auth_manager,
            model_info,
            None,
            None,
            otel_manager,
            provider,
            effort,
            summary,
            conversation_id,
            session_source,
            transport_manager,
        )
    }

    pub(crate) fn new_with_dynamic_context_window(
        config: Arc<Config>,
        auth_manager: Option<Arc<AuthManager>>,
        model_info: ModelInfo,
        dynamic_context_window: Option<Arc<Mutex<DynamicContextWindowState>>>,
        chat_role_compatibility: Option<Arc<Mutex<ChatRoleCompatibilityState>>>,
        otel_manager: OtelManager,
        provider: ModelProviderInfo,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        conversation_id: ThreadId,
        session_source: SessionSource,
        transport_manager: TransportManager,
    ) -> Self {
        let runtime_config = Arc::new(llm_runtime_config_from_core_config(&config));
        let auth_source = CoreAuthSource::boxed(auth_manager.clone(), provider.clone());
        let runtime_factory = DefaultAgentRuntimeFactory;
        let runtime = runtime_factory.build_runtime(RuntimeBuildSpec {
            runtime_config,
            auth_source,
            http_client: build_reqwest_client(),
            model_info: model_info.clone(),
            chat_role_compatibility: chat_role_compatibility.clone(),
            otel_manager: otel_manager.clone(),
            provider: provider.clone(),
            effort,
            summary,
            conversation_id,
            session_source: session_source.clone(),
            transport_manager: transport_manager.clone(),
        });

        Self {
            state: Arc::new(ModelClientState {
                config,
                auth_manager,
                model_info,
                dynamic_context_window,
                chat_role_compatibility,
                otel_manager,
                provider,
                effort,
                summary,
                session_source,
                transport_manager,
                runtime,
            }),
        }
    }

    pub fn new_session(&self) -> ModelClientSession {
        ModelClientSession {
            inner: self.state.runtime.new_session(),
        }
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

    pub fn provider(&self) -> &ModelProviderInfo {
        &self.state.provider
    }

    pub fn get_provider(&self) -> ModelProviderInfo {
        self.state.provider.clone()
    }

    pub fn get_otel_manager(&self) -> OtelManager {
        self.state.otel_manager.clone()
    }

    pub fn get_session_source(&self) -> SessionSource {
        self.state.session_source.clone()
    }

    pub(crate) fn transport_manager(&self) -> TransportManager {
        self.state.transport_manager.clone()
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

    pub fn runtime_metadata(&self) -> RuntimeMetadata {
        self.state.runtime.metadata()
    }

    pub fn get_auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.state.auth_manager.clone()
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

    #[cfg(test)]
    pub(crate) fn chat_role_compatibility(&self) -> Option<Arc<Mutex<ChatRoleCompatibilityState>>> {
        self.state.chat_role_compatibility.clone()
    }

    pub(crate) fn estimated_input_tokens_for_prompt(&self, prompt: &Prompt) -> Option<i64> {
        self.state.runtime.estimated_input_tokens(prompt)
    }

    pub async fn compact_conversation_history(&self, prompt: &Prompt) -> Result<Vec<ResponseItem>> {
        self.state
            .runtime
            .compact_conversation_history(prompt)
            .await
            .map_err(Into::into)
    }
}

impl ModelClientSession {
    pub async fn stream(&mut self, prompt: &Prompt) -> Result<ResponseStream> {
        let mut inner_stream = self
            .inner
            .run_turn(prompt)
            .await
            .map_err(crate::error::CodexErr::from)?;
        let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent>>(1600);

        tokio::spawn(async move {
            while let Some(event) = inner_stream.next().await {
                if tx_event.send(event.map_err(Into::into)).await.is_err() {
                    return;
                }
            }
        });

        Ok(ResponseStream { rx_event })
    }

    pub(crate) fn try_switch_fallback_transport(&mut self) -> bool {
        self.inner.try_switch_fallback_transport()
    }
}

impl std::fmt::Debug for ModelClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelClient")
            .field("model", &self.state.model_info.slug)
            .field("provider", &self.state.provider.name)
            .finish()
    }
}
