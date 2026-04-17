use std::sync::Arc;

use async_trait::async_trait;
use codex_otel::OtelManager;
use reqwest::Client as HttpClient;

use crate::AuthSource;
use crate::ChatRoleCompatibilityHandle;
use crate::LlmClient;
use crate::LlmClientSession;
use crate::LlmRuntimeConfig;
use crate::ModelProviderInfo;
use crate::Result;
use crate::TransportManager;
use crate::runtime_types::AgentEventStream;
use crate::runtime_types::AgentTurnInput;
use crate::runtime_types::RuntimeCapabilities;
use crate::runtime_types::RuntimeMetadata;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::SessionSource;

#[async_trait]
pub trait ConversationCompactor: Send + Sync {
    async fn compact_conversation_history(
        &self,
        input: &AgentTurnInput,
    ) -> Result<Vec<ResponseItem>>;
}

#[async_trait]
pub trait AgentRuntime: ConversationCompactor + Send + Sync {
    fn new_session(&self) -> Box<dyn AgentRuntimeSession>;
    fn capabilities(&self) -> RuntimeCapabilities;
    fn metadata(&self) -> RuntimeMetadata;
    fn estimated_input_tokens(&self, input: &AgentTurnInput) -> Option<i64>;
}

#[async_trait]
pub trait AgentRuntimeSession: Send {
    async fn run_turn(&mut self, input: &AgentTurnInput) -> Result<AgentEventStream>;
    fn try_switch_fallback_transport(&mut self) -> bool;
}

pub struct RuntimeBuildSpec {
    pub runtime_config: Arc<LlmRuntimeConfig>,
    pub auth_source: Arc<dyn AuthSource>,
    pub http_client: HttpClient,
    pub model_info: ModelInfo,
    pub chat_role_compatibility: Option<ChatRoleCompatibilityHandle>,
    pub otel_manager: OtelManager,
    pub provider: ModelProviderInfo,
    pub effort: Option<ReasoningEffortConfig>,
    pub summary: ReasoningSummaryConfig,
    pub conversation_id: ThreadId,
    pub session_source: SessionSource,
    pub transport_manager: TransportManager,
}

pub trait AgentRuntimeFactory: Send + Sync {
    fn build_runtime(&self, spec: RuntimeBuildSpec) -> Arc<dyn AgentRuntime>;
}

#[derive(Default)]
pub struct DefaultAgentRuntimeFactory;

impl AgentRuntimeFactory for DefaultAgentRuntimeFactory {
    fn build_runtime(&self, spec: RuntimeBuildSpec) -> Arc<dyn AgentRuntime> {
        let RuntimeBuildSpec {
            runtime_config,
            auth_source,
            http_client,
            model_info,
            chat_role_compatibility,
            otel_manager,
            provider,
            effort,
            summary,
            conversation_id,
            session_source,
            transport_manager,
        } = spec;

        let capabilities = RuntimeCapabilities::from_wire_and_model(
            provider.wire_api,
            &model_info,
            provider.supports_websockets,
        );
        let metadata = RuntimeMetadata {
            provider_name: provider.name.clone(),
            model: model_info.slug.clone(),
            wire_api: provider.wire_api,
            stream_max_retries: provider.stream_max_retries(),
        };
        let client = LlmClient::new(
            runtime_config,
            auth_source,
            http_client,
            model_info,
            chat_role_compatibility,
            otel_manager,
            provider,
            effort,
            summary,
            conversation_id,
            session_source,
            transport_manager,
        );

        Arc::new(DefaultAgentRuntime {
            client,
            capabilities,
            metadata,
        })
    }
}

struct DefaultAgentRuntime {
    client: LlmClient,
    capabilities: RuntimeCapabilities,
    metadata: RuntimeMetadata,
}

#[async_trait]
impl ConversationCompactor for DefaultAgentRuntime {
    async fn compact_conversation_history(
        &self,
        input: &AgentTurnInput,
    ) -> Result<Vec<ResponseItem>> {
        self.client.compact_conversation_history(input).await
    }
}

#[async_trait]
impl AgentRuntime for DefaultAgentRuntime {
    fn new_session(&self) -> Box<dyn AgentRuntimeSession> {
        Box::new(DefaultAgentRuntimeSession {
            inner: self.client.new_session(),
        })
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        self.capabilities.clone()
    }

    fn metadata(&self) -> RuntimeMetadata {
        self.metadata.clone()
    }

    fn estimated_input_tokens(&self, input: &AgentTurnInput) -> Option<i64> {
        self.client.estimated_input_tokens_for_prompt(input)
    }
}

struct DefaultAgentRuntimeSession {
    inner: LlmClientSession,
}

#[async_trait]
impl AgentRuntimeSession for DefaultAgentRuntimeSession {
    async fn run_turn(&mut self, input: &AgentTurnInput) -> Result<AgentEventStream> {
        self.inner.stream(input).await
    }

    fn try_switch_fallback_transport(&mut self) -> bool {
        self.inner.try_switch_fallback_transport()
    }
}
