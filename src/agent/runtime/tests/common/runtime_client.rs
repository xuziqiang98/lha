use std::sync::Arc;

use lha_agent::AuthManager;
use lha_agent::config::Config;
use lha_agent::default_client::build_reqwest_client;
use lha_agent::models_manager::manager::ModelsManager;
use lha_agent::protocol::SessionSource;
use lha_llm::DefaultRuntimeClientFactory;
use lha_llm::RuntimeBuildSpec;
use lha_llm::RuntimeClient;
use lha_llm::RuntimeClientFactory;
use lha_llm::RuntimeEndpoint;
use lha_llm::RuntimeSession;
use lha_llm::TurnEventStream;
use lha_llm::TurnRequest;
use lha_otel::OtelManager;
use lha_protocol::ThreadId;
use lha_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use lha_protocol::openai_models::ModelInfo;
use lha_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;

pub struct TestRuntimeClient {
    inner: Arc<dyn RuntimeClient>,
}

pub struct TestRuntimeSession {
    inner: Box<dyn RuntimeSession>,
}

impl TestRuntimeClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<Config>,
        _auth_manager: Option<Arc<AuthManager>>,
        model_info: ModelInfo,
        otel_manager: OtelManager,
        endpoint: RuntimeEndpoint,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        conversation_id: ThreadId,
        session_source: SessionSource,
    ) -> Self {
        let runtime_factory = DefaultRuntimeClientFactory::new();
        let mut endpoint = endpoint;
        if !config
            .features
            .enabled(lha_agent::features::Feature::ResponsesWebsockets)
        {
            endpoint.set_realtime_turn_streaming_enabled(false);
        }
        let runtime = runtime_factory.build_client(RuntimeBuildSpec {
            endpoint_id: config.model_provider_id.clone(),
            http_client: build_reqwest_client(),
            model_info: model_info.into(),
            otel_manager,
            endpoint,
            effort,
            summary,
            session_id: conversation_id.to_string(),
            origin_tag: Some(session_source_to_origin_tag(&session_source)),
            show_raw_agent_reasoning: config.show_raw_agent_reasoning,
            model_verbosity: config.model_verbosity,
            web_search_mode: config.web_search_mode,
            experimental_beta_feature_keys: lha_agent::features::FEATURES
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
            sse_fixture_path: None,
        });

        Self { inner: runtime }
    }

    pub fn for_test_config(
        config: Arc<Config>,
        otel_manager: OtelManager,
        endpoint: RuntimeEndpoint,
        conversation_id: ThreadId,
        session_source: SessionSource,
    ) -> Self {
        let model = ModelsManager::get_model_offline(config.model.as_deref());
        let model_info = ModelsManager::construct_model_info_offline(model.as_str(), &config);
        Self::new(
            config.clone(),
            None,
            model_info,
            otel_manager,
            endpoint,
            config.model_reasoning_effort,
            config.model_reasoning_summary,
            conversation_id,
            session_source,
        )
    }

    pub fn new_session(&self) -> TestRuntimeSession {
        TestRuntimeSession {
            inner: self.inner.new_session(),
        }
    }
}

impl TestRuntimeSession {
    pub async fn run_turn(&mut self, turn: &TurnRequest) -> lha_llm::Result<TurnEventStream> {
        self.inner.run_turn(turn).await
    }
}

fn session_source_to_origin_tag(session_source: &SessionSource) -> String {
    match session_source {
        SessionSource::Cli => "cli".to_string(),
        SessionSource::VSCode => "vscode".to_string(),
        SessionSource::Exec => "exec".to_string(),
        SessionSource::Mcp => "mcp".to_string(),
        SessionSource::Agent => "agent".to_string(),
        SessionSource::Unknown => "unknown".to_string(),
    }
}
