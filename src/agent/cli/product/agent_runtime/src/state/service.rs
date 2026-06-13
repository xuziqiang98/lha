use std::sync::Arc;

use crate::product::agent::AuthManager;
use crate::product::agent::RolloutRecorder;
use crate::product::agent::agent_jobs::AgentJobManager;
use crate::product::agent::exec_policy::ExecPolicyManager;
use crate::product::agent::input_slimming::InputSlimmingStore;
use crate::product::agent::mcp_connection_manager::McpConnectionManager;
use crate::product::agent::models_manager::manager::ModelsManager;
use crate::product::agent::skills::SkillsManager;
use crate::product::agent::state_db::StateDbHandle;
use crate::product::agent::tools::sandboxing::ApprovalStore;
use crate::product::agent::unified_exec::UnifiedExecProcessManager;
use crate::product::agent::user_notification::UserNotifier;
use crate::product::otel::OtelManager;
use lha_llm::RuntimeClientFactory;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub(crate) struct SessionServices {
    pub(crate) mcp_connection_manager: Arc<RwLock<McpConnectionManager>>,
    pub(crate) mcp_startup_cancellation_token: Mutex<CancellationToken>,
    pub(crate) unified_exec_manager: UnifiedExecProcessManager,
    pub(crate) notifier: UserNotifier,
    pub(crate) rollout: Mutex<Option<RolloutRecorder>>,
    pub(crate) user_shell: Arc<crate::product::agent::shell::Shell>,
    pub(crate) show_raw_agent_reasoning: bool,
    pub(crate) exec_policy: ExecPolicyManager,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) models_manager: Arc<ModelsManager>,
    pub(crate) otel_manager: OtelManager,
    pub(crate) tool_approvals: Mutex<ApprovalStore>,
    pub(crate) skills_manager: Arc<SkillsManager>,
    pub(crate) agent_jobs: AgentJobManager,
    pub(crate) state_db: Option<StateDbHandle>,
    pub(crate) runtime_factory: Arc<dyn RuntimeClientFactory>,
    pub(crate) input_slimming_store: InputSlimmingStore,
}
