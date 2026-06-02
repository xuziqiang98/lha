use std::sync::Arc;

use lha_llm::DefaultRuntimeClientFactory;
use lha_otel::OtelManager;
use lha_protocol::ThreadId;
use lha_protocol::openai_models::ModelInfo;
use lha_protocol::openai_models::ReasoningEffort;
use lha_protocol::protocol::InitialHistory;
use lha_protocol::protocol::Op;
use lha_protocol::protocol::SessionSource;
use lha_protocol::user_input::UserInput;
use lha_state::StateRuntime;

use crate::AuthManager;
use crate::CodexThread;
use crate::client::TurnRuntime;
use crate::codex::Codex;
use crate::config::Config;
use crate::error::CodexErr;
use crate::features::Feature;
use crate::memories::metrics;
use crate::models_manager::manager::ModelsManager;
use crate::skills::SkillsManager;
use tracing::warn;

#[derive(Clone)]
pub(crate) struct MemoryStartupContext {
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    skills_manager: Arc<SkillsManager>,
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    config: Config,
}

pub(crate) struct SpawnedConsolidationAgent {
    pub(crate) thread_id: ThreadId,
    pub(crate) thread: Arc<CodexThread>,
}

impl MemoryStartupContext {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        models_manager: Arc<ModelsManager>,
        skills_manager: Arc<SkillsManager>,
        thread_id: ThreadId,
        thread: Arc<CodexThread>,
        config: Config,
        _session_source: SessionSource,
    ) -> Self {
        Self {
            auth_manager,
            models_manager,
            skills_manager,
            thread_id,
            thread,
            config,
        }
    }

    pub(crate) fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn state_db(&self) -> Option<Arc<StateRuntime>> {
        self.thread.state_db()
    }

    pub(crate) fn memory_root(&self) -> std::path::PathBuf {
        lha_memories_write::memory_root_path(self.config.lha_home.as_path())
    }

    pub(crate) async fn stage_one_runtime(&self) -> TurnRuntime {
        let model = self.memory_model_name(
            "stage1",
            self.config.memories.extract_model.as_deref(),
            self.config.model.as_deref(),
            lha_memories_write::STAGE_ONE_MODEL,
        );
        self.runtime_for_model(model, Some(ReasoningEffort::Low), SessionSource::Agent)
            .await
    }

    pub(crate) fn stage_two_model(&self) -> String {
        self.memory_model_name(
            "stage2",
            self.config.memories.consolidation_model.as_deref(),
            self.config.model.as_deref(),
            lha_memories_write::STAGE_TWO_MODEL,
        )
    }

    pub(crate) async fn spawn_consolidation_agent(
        &self,
        config: Config,
        prompt: String,
    ) -> Result<SpawnedConsolidationAgent, CodexErr> {
        let spawn = Codex::spawn(
            config,
            Arc::clone(&self.auth_manager),
            Arc::clone(&self.models_manager),
            Arc::clone(&self.skills_manager),
            InitialHistory::New,
            SessionSource::Agent,
            Vec::new(),
        )
        .await?;
        let thread_id = spawn.thread_id;
        let event = spawn.codex.next_event().await?;
        if !matches!(
            event.msg,
            lha_protocol::protocol::EventMsg::SessionConfigured(_)
        ) {
            return Err(CodexErr::SessionConfiguredNotFirstEvent);
        }
        let thread = Arc::new(CodexThread::new(spawn.codex, None));
        thread
            .submit(Op::UserInput {
                items: vec![UserInput::Text {
                    text: prompt,
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .await?;

        Ok(SpawnedConsolidationAgent { thread_id, thread })
    }

    pub(crate) async fn shutdown_consolidation_agent(
        &self,
        agent: SpawnedConsolidationAgent,
    ) -> Result<(), CodexErr> {
        let _ = agent.thread.submit(Op::Shutdown).await;
        agent.thread.wait_for_shutdown_complete().await
    }

    async fn runtime_for_model(
        &self,
        model: String,
        effort: Option<ReasoningEffort>,
        session_source: SessionSource,
    ) -> TurnRuntime {
        let mut runtime_config = self.config.clone();
        runtime_config.model = Some(model.clone());
        let model_info = self
            .models_manager
            .get_model_info(model.as_str(), &runtime_config)
            .await;
        build_runtime(
            Arc::new(runtime_config),
            Some(Arc::clone(&self.auth_manager)),
            model_info,
            self.thread_id,
            session_source,
            effort,
        )
    }

    fn memory_model_name(
        &self,
        phase: &'static str,
        configured_memory_model: Option<&str>,
        current_model: Option<&str>,
        default_model: &str,
    ) -> String {
        let available_models = match self.models_manager.try_list_models(&self.config) {
            Ok(models) => Some(
                models
                    .into_iter()
                    .map(|model| model.model)
                    .collect::<Vec<_>>(),
            ),
            Err(_err) => {
                metrics::counter(
                    metrics::MODEL_FALLBACK,
                    1,
                    &[("phase", phase), ("reason", "lock_unavailable")],
                );
                None
            }
        };
        resolve_memory_model_name(
            phase,
            configured_memory_model,
            current_model,
            default_model,
            available_models.as_deref(),
        )
    }
}

fn resolve_memory_model_name(
    phase: &'static str,
    configured_memory_model: Option<&str>,
    current_model: Option<&str>,
    default_model: &str,
    available_models: Option<&[String]>,
) -> String {
    if let Some(configured_memory_model) = configured_memory_model {
        if let Some(available_models) = available_models
            && !available_models
                .iter()
                .any(|model| model == configured_memory_model)
        {
            metrics::counter(
                metrics::MODEL_FALLBACK,
                1,
                &[("phase", phase), ("reason", "configured_unavailable")],
            );
            warn!(
                "configured memory model `{configured_memory_model}` is unavailable for {phase}; falling back"
            );
            return current_or_default_model(phase, current_model, default_model);
        }
        return configured_memory_model.to_string();
    }

    current_or_default_model(phase, current_model, default_model)
}

fn current_or_default_model(
    phase: &'static str,
    current_model: Option<&str>,
    default_model: &str,
) -> String {
    if let Some(current_model) = current_model {
        return current_model.to_string();
    }
    metrics::counter(
        metrics::MODEL_FALLBACK,
        1,
        &[("phase", phase), ("reason", "no_current_model")],
    );
    warn!("no current model configured for {phase}; falling back to `{default_model}`");
    default_model.to_string()
}

fn build_runtime(
    config: Arc<Config>,
    auth_manager: Option<Arc<AuthManager>>,
    model_info: ModelInfo,
    thread_id: ThreadId,
    session_source: SessionSource,
    effort: Option<ReasoningEffort>,
) -> TurnRuntime {
    let otel_manager = OtelManager::new(
        thread_id,
        model_info.slug.as_str(),
        model_info.slug.as_str(),
        None,
        None,
        None,
        config.otel.log_user_prompt,
        "memory".to_string(),
        session_source.clone(),
    );
    TurnRuntime::new_with_dynamic_context_window(
        Arc::clone(&config),
        auth_manager,
        Arc::new(DefaultRuntimeClientFactory::new()),
        model_info,
        None,
        otel_manager,
        config.model_provider.clone(),
        effort,
        config.model_reasoning_summary,
        thread_id,
        session_source,
    )
}

pub(crate) fn disable_consolidation_features(config: &mut Config) {
    config.memories.generate_memories = false;
    config.memories.use_memories = false;
    config.features.disable(Feature::MemoryTool);
    config.features.disable(Feature::AgentJobs);
    config.features.disable(Feature::Apps);
    config.features.disable(Feature::WebSearchRequest);
    config.features.disable(Feature::WebSearchCached);
    config.features.disable(Feature::SkillMcpDependencyInstall);
    config
        .features
        .disable(Feature::SkillEnvVarDependencyPrompt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn available(models: &[&str]) -> Vec<String> {
        models.iter().map(|model| (*model).to_string()).collect()
    }

    #[test]
    fn configured_memory_model_wins_when_available() {
        assert_eq!(
            resolve_memory_model_name(
                "stage1",
                Some("memory-model"),
                Some("current-model"),
                "default-model",
                Some(&available(&["memory-model"]))
            ),
            "memory-model"
        );
    }

    #[test]
    fn configured_memory_model_falls_back_to_current_when_unavailable() {
        assert_eq!(
            resolve_memory_model_name(
                "stage1",
                Some("missing-model"),
                Some("current-model"),
                "default-model",
                Some(&available(&["current-model"]))
            ),
            "current-model"
        );
    }

    #[test]
    fn configured_memory_model_falls_back_to_default_without_current() {
        assert_eq!(
            resolve_memory_model_name(
                "stage1",
                Some("missing-model"),
                None,
                "default-model",
                Some(&available(&["other-model"]))
            ),
            "default-model"
        );
    }

    #[test]
    fn current_model_wins_without_configured_memory_model() {
        assert_eq!(
            resolve_memory_model_name(
                "stage1",
                None,
                Some("current-model"),
                "default-model",
                Some(&available(&["current-model"]))
            ),
            "current-model"
        );
    }

    #[test]
    fn default_model_is_used_without_configured_or_current_model() {
        assert_eq!(
            resolve_memory_model_name(
                "stage1",
                None,
                None,
                "default-model",
                Some(&available(&["other-model"]))
            ),
            "default-model"
        );
    }
}
