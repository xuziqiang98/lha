use crate::codex::Codex;
use crate::error::Result as CodexResult;
use crate::protocol::Event;
use crate::protocol::Op;
use crate::protocol::Submission;
use adam_agent_runtime::SessionSnapshot;
use adam_agent_runtime::SessionStatus;
use adam_llm::RuntimeEndpoint;
use adam_llm::RuntimeMetadata;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::config_types::Personality;
use adam_protocol::openai_models::ReasoningEffort;
use adam_protocol::protocol::AskForApproval;
use adam_protocol::protocol::SandboxPolicy;
use adam_protocol::protocol::SessionSource;
#[cfg(any(test, feature = "test-support"))]
use adam_protocol::workflow::WorkflowDefinition;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::PathBuf;

use crate::state_db::StateDbHandle;

#[derive(Clone, Debug)]
pub struct ThreadConfigSnapshot {
    pub model: String,
    pub identity_kind: IdentityKind,
    pub model_provider_id: String,
    pub approval_policy: AskForApproval,
    pub sandbox_policy: SandboxPolicy,
    pub cwd: PathBuf,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub personality: Option<Personality>,
    pub session_source: SessionSource,
}

pub struct CodexThread {
    codex: Codex,
    rollout_path: Option<PathBuf>,
}

/// Conduit for the bidirectional stream of messages that compose a thread
/// (formerly called a conversation) in Adam.
impl CodexThread {
    pub(crate) fn new(codex: Codex, rollout_path: Option<PathBuf>) -> Self {
        Self {
            codex,
            rollout_path,
        }
    }

    pub async fn submit(&self, op: Op) -> CodexResult<String> {
        self.codex.submit(op).await
    }

    /// Use sparingly: this is intended to be removed soon.
    pub async fn submit_with_id(&self, sub: Submission) -> CodexResult<()> {
        self.codex.submit_with_id(sub).await
    }

    pub async fn next_event(&self) -> CodexResult<Event> {
        self.codex.next_event().await
    }

    pub async fn session_status(&self) -> SessionStatus {
        self.codex.session_status().await
    }

    pub async fn wait_for_shutdown_complete(&self) -> CodexResult<()> {
        self.codex.wait_for_shutdown_complete().await
    }

    pub fn rollout_path(&self) -> Option<PathBuf> {
        self.rollout_path.clone()
    }

    pub fn state_db(&self) -> Option<StateDbHandle> {
        self.codex.state_db()
    }

    pub async fn config_snapshot(&self) -> ThreadConfigSnapshot {
        self.codex.thread_config_snapshot().await
    }

    pub async fn core_snapshot(&self) -> SessionSnapshot {
        let config = self.config_snapshot().await;
        let history = self.codex.session.clone_history().await;
        let mut hasher = DefaultHasher::new();
        self.codex.session.conversation_id.hash(&mut hasher);
        let status = self.session_status().await;

        SessionSnapshot {
            session_id: hasher.finish(),
            status,
            conversation: history.raw_items().to_vec(),
            steering_queue: Vec::new(),
            follow_up_queue: Vec::new(),
            runtime: RuntimeMetadata {
                endpoint_name: config.model_provider_id,
                model: config.model,
            },
            active_turn: None,
        }
    }

    pub async fn flush_rollout(&self) {
        self.codex.session.flush_rollout().await;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn set_workflow_for_testing(
        &self,
        definition: WorkflowDefinition,
    ) -> std::result::Result<(), Vec<adam_protocol::workflow::WorkflowValidationError>> {
        self.codex
            .session
            .set_workflow_for_testing(definition)
            .await
    }

    pub async fn update_model_provider(&self, provider: RuntimeEndpoint) {
        self.codex.update_model_provider(provider).await;
    }

    pub async fn update_tui_buddy(&self, buddy: crate::config::types::TuiBuddy) {
        self.codex.update_tui_buddy(buddy).await;
    }

    pub async fn switch_provider_and_model(
        &self,
        model_provider_id: String,
        provider: RuntimeEndpoint,
        model: String,
    ) {
        self.codex
            .switch_provider_and_model(model_provider_id, provider, model)
            .await;
    }
}
