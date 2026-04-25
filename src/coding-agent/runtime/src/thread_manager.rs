use crate::AuthManager;
#[cfg(any(test, feature = "test-support"))]
use crate::CodexAuth;
use crate::codex::Codex;
use crate::codex::CodexSpawnOk;
use crate::codex::INITIAL_SUBMIT_ID;
use crate::codex_thread::CodexThread;
use crate::config::Config;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::models_manager::manager::ModelsManager;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::SessionConfiguredEvent;
use crate::rollout::RolloutRecorder;
use crate::rollout::truncation;
use crate::skills::SkillsManager;
use crate::subagents::AgentControl;
use crate::subagents::status::is_final;
use codex_llm::CatalogRefreshStrategy;
use codex_llm::RuntimeEndpoint;
use codex_protocol::ThreadId;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::McpServerRefreshConfig;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionSource;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use tempfile::TempDir;
use tokio::sync::RwLock;
use tokio::sync::TryLockError;
use tokio::sync::broadcast;
use tokio::sync::watch;
use tracing::warn;

const THREAD_CREATED_CHANNEL_CAPACITY: usize = 1024;

/// Represents a newly created Codex thread (formerly called a conversation), including the first event
/// (which is [`EventMsg::SessionConfigured`]).
pub struct NewThread {
    pub thread_id: ThreadId,
    pub thread: Arc<CodexThread>,
    pub session_configured: SessionConfiguredEvent,
}

/// [`ThreadManager`] is responsible for creating threads and maintaining
/// them in memory.
pub struct ThreadManager {
    state: Arc<ThreadManagerState>,
    #[cfg(any(test, feature = "test-support"))]
    _test_adam_home_guard: Option<TempDir>,
}

/// Shared, `Arc`-owned state for [`ThreadManager`]. This `Arc` is required to have a single
/// `Arc` reference that can be downgraded to by `AgentControl` while preventing every single
/// function to require an `Arc<&Self>`.
pub(crate) struct ThreadManagerState {
    threads: Arc<RwLock<HashMap<ThreadId, Arc<CodexThread>>>>,
    retained_agent_statuses: Arc<RwLock<HashMap<ThreadId, RetainedAgentStatus>>>,
    thread_generations: Arc<RwLock<HashMap<ThreadId, u64>>>,
    thread_created_tx: broadcast::Sender<ThreadId>,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    skills_manager: Arc<SkillsManager>,
    session_source: SessionSource,
    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    // Captures submitted ops for testing purpose.
    ops_log: Arc<std::sync::Mutex<Vec<(ThreadId, Op)>>>,
}

#[derive(Clone)]
pub(crate) enum RetainedAgentStatus {
    Pending {
        generation: u64,
        _thread: Arc<CodexThread>,
        status_rx: watch::Receiver<AgentStatus>,
    },
    Final {
        generation: u64,
        status: AgentStatus,
    },
}

impl ThreadManager {
    pub fn new(
        adam_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        model_provider_id: &str,
        provider: RuntimeEndpoint,
        session_source: SessionSource,
    ) -> Self {
        let (thread_created_tx, _) = broadcast::channel(THREAD_CREATED_CHANNEL_CAPACITY);
        Self {
            state: Arc::new(ThreadManagerState {
                threads: Arc::new(RwLock::new(HashMap::new())),
                retained_agent_statuses: Arc::new(RwLock::new(HashMap::new())),
                thread_generations: Arc::new(RwLock::new(HashMap::new())),
                thread_created_tx,
                models_manager: Arc::new(ModelsManager::new(
                    adam_home.clone(),
                    auth_manager.clone(),
                    model_provider_id,
                    provider,
                )),
                skills_manager: Arc::new(SkillsManager::new(adam_home)),
                auth_manager,
                session_source,
                #[cfg(any(test, feature = "test-support"))]
                ops_log: Arc::new(std::sync::Mutex::new(Vec::new())),
            }),
            #[cfg(any(test, feature = "test-support"))]
            _test_adam_home_guard: None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Construct with a dummy AuthManager containing the provided CodexAuth.
    /// Used for integration tests: should not be used by ordinary business logic.
    pub fn with_models_provider(auth: CodexAuth, provider: RuntimeEndpoint) -> Self {
        let temp_dir = tempfile::tempdir().unwrap_or_else(|err| panic!("temp codex home: {err}"));
        let adam_home = temp_dir.path().to_path_buf();
        let mut manager =
            Self::with_models_provider_and_home(auth, "test-provider", provider, adam_home);
        manager._test_adam_home_guard = Some(temp_dir);
        manager
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Construct with a dummy AuthManager containing the provided CodexAuth and codex home.
    /// Used for integration tests: should not be used by ordinary business logic.
    pub fn with_models_provider_and_home(
        auth: CodexAuth,
        model_provider_id: &str,
        provider: RuntimeEndpoint,
        adam_home: PathBuf,
    ) -> Self {
        let auth_manager = AuthManager::from_auth_for_testing(auth);
        let (thread_created_tx, _) = broadcast::channel(THREAD_CREATED_CHANNEL_CAPACITY);
        Self {
            state: Arc::new(ThreadManagerState {
                threads: Arc::new(RwLock::new(HashMap::new())),
                retained_agent_statuses: Arc::new(RwLock::new(HashMap::new())),
                thread_generations: Arc::new(RwLock::new(HashMap::new())),
                thread_created_tx,
                models_manager: Arc::new(ModelsManager::with_provider(
                    adam_home.clone(),
                    auth_manager.clone(),
                    model_provider_id,
                    provider,
                )),
                skills_manager: Arc::new(SkillsManager::new(adam_home)),
                auth_manager,
                session_source: SessionSource::Exec,
                #[cfg(any(test, feature = "test-support"))]
                ops_log: Arc::new(std::sync::Mutex::new(Vec::new())),
            }),
            _test_adam_home_guard: None,
        }
    }

    pub fn session_source(&self) -> SessionSource {
        self.state.session_source.clone()
    }

    pub fn skills_manager(&self) -> Arc<SkillsManager> {
        self.state.skills_manager.clone()
    }

    pub fn get_models_manager(&self) -> Arc<ModelsManager> {
        self.state.models_manager.clone()
    }

    pub async fn list_models(
        &self,
        config: &Config,
        refresh_strategy: CatalogRefreshStrategy,
    ) -> Vec<ModelPreset> {
        self.state
            .models_manager
            .list_models(config, refresh_strategy)
            .await
    }

    pub async fn list_picker_models(
        &self,
        config: &Config,
        refresh_strategy: CatalogRefreshStrategy,
    ) -> Vec<ModelPreset> {
        self.state
            .models_manager
            .list_picker_models(config, refresh_strategy)
            .await
    }

    pub fn try_list_models(&self, config: &Config) -> Result<Vec<ModelPreset>, TryLockError> {
        self.state.models_manager.try_list_models(config)
    }

    pub fn try_list_picker_models(
        &self,
        config: &Config,
    ) -> Result<Vec<ModelPreset>, TryLockError> {
        self.state.models_manager.try_list_picker_models(config)
    }

    pub async fn list_model_switcher_models(
        &self,
        config: &Config,
        refresh_strategy: CatalogRefreshStrategy,
    ) -> Vec<ModelPreset> {
        self.state
            .models_manager
            .list_model_switcher_models(config, refresh_strategy)
            .await
    }

    pub fn try_list_model_switcher_models(
        &self,
        config: &Config,
    ) -> Result<Vec<ModelPreset>, TryLockError> {
        self.state
            .models_manager
            .try_list_model_switcher_models(config)
    }

    pub fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.state.models_manager.list_collaboration_modes()
    }

    pub fn try_is_official_openai_model(
        &self,
        config: &Config,
        model: &str,
        model_provider_id: &str,
    ) -> Result<bool, TryLockError> {
        self.state
            .models_manager
            .try_is_official_openai_model(config, model, model_provider_id)
    }

    pub async fn get_default_model(
        &self,
        model: &Option<String>,
        config: &Config,
        refresh_strategy: CatalogRefreshStrategy,
    ) -> CodexResult<String> {
        self.state
            .models_manager
            .get_default_model(model, config, refresh_strategy)
            .await
    }

    pub async fn get_model_info(&self, model: &str, config: &Config) -> ModelInfo {
        self.state
            .models_manager
            .get_model_info(model, config)
            .await
    }

    pub async fn switch_model_provider(&self, model_provider_id: &str, provider: RuntimeEndpoint) {
        self.state
            .models_manager
            .switch_provider(model_provider_id, provider)
            .await;
    }

    pub async fn list_thread_ids(&self) -> Vec<ThreadId> {
        self.state.threads.read().await.keys().copied().collect()
    }

    pub async fn refresh_mcp_servers(&self, refresh_config: McpServerRefreshConfig) {
        let threads = self
            .state
            .threads
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for thread in threads {
            if let Err(err) = thread
                .submit(Op::RefreshMcpServers {
                    config: refresh_config.clone(),
                })
                .await
            {
                warn!("failed to request MCP server refresh: {err}");
            }
        }
    }

    pub fn subscribe_thread_created(&self) -> broadcast::Receiver<ThreadId> {
        self.state.thread_created_tx.subscribe()
    }

    pub async fn get_thread(&self, thread_id: ThreadId) -> CodexResult<Arc<CodexThread>> {
        self.state.get_thread(thread_id).await
    }

    pub async fn start_thread(&self, config: Config) -> CodexResult<NewThread> {
        self.start_thread_with_tools(config, Vec::new()).await
    }

    pub async fn start_thread_with_tools(
        &self,
        config: Config,
        dynamic_tools: Vec<codex_protocol::dynamic_tools::DynamicToolSpec>,
    ) -> CodexResult<NewThread> {
        self.state
            .spawn_thread(
                config,
                InitialHistory::New,
                Arc::clone(&self.state.auth_manager),
                self.agent_control(),
                dynamic_tools,
            )
            .await
    }

    pub async fn resume_thread_from_rollout(
        &self,
        config: Config,
        rollout_path: PathBuf,
        auth_manager: Arc<AuthManager>,
    ) -> CodexResult<NewThread> {
        let initial_history = RolloutRecorder::get_rollout_history(&rollout_path).await?;
        self.resume_thread_with_history(config, initial_history, auth_manager)
            .await
    }

    pub async fn resume_thread_with_history(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
    ) -> CodexResult<NewThread> {
        self.state
            .spawn_thread(
                config,
                initial_history,
                auth_manager,
                self.agent_control(),
                Vec::new(),
            )
            .await
    }

    /// Removes the thread from the manager's internal map, though the thread is stored
    /// as `Arc<CodexThread>`, it is possible that other references to it exist elsewhere.
    /// Returns the thread if the thread was found and removed.
    pub async fn remove_thread(&self, thread_id: &ThreadId) -> Option<Arc<CodexThread>> {
        self.state.remove_thread(thread_id).await
    }

    /// Closes all threads open in this ThreadManager
    pub async fn remove_and_close_all_threads(&self) -> CodexResult<()> {
        for thread in self.state.threads.read().await.values() {
            thread.submit(Op::Shutdown).await?;
        }
        self.state.threads.write().await.clear();
        self.state.retained_agent_statuses.write().await.clear();
        self.state.thread_generations.write().await.clear();
        Ok(())
    }

    /// Fork an existing thread by taking messages up to the given position (not including
    /// the message at the given position) and starting a new thread with identical
    /// configuration (unless overridden by the caller's `config`). The new thread will have
    /// a fresh id. Pass `usize::MAX` to keep the full rollout history.
    pub async fn fork_thread(
        &self,
        nth_user_message: usize,
        config: Config,
        path: PathBuf,
    ) -> CodexResult<NewThread> {
        let history = RolloutRecorder::get_rollout_history(&path).await?;
        let history = truncate_before_nth_user_message(history, nth_user_message);
        self.state
            .spawn_thread(
                config,
                history,
                Arc::clone(&self.state.auth_manager),
                self.agent_control(),
                Vec::new(),
            )
            .await
    }

    /// Fork an existing thread using the provided session source for the new thread.
    pub async fn fork_thread_with_source(
        &self,
        nth_user_message: usize,
        config: Config,
        path: PathBuf,
        session_source: SessionSource,
    ) -> CodexResult<NewThread> {
        let history = RolloutRecorder::get_rollout_history(&path).await?;
        let history = truncate_before_nth_user_message(history, nth_user_message);
        self.state
            .spawn_thread_with_source(
                config,
                history,
                Arc::clone(&self.state.auth_manager),
                self.agent_control(),
                session_source,
                Vec::new(),
            )
            .await
    }

    pub(crate) fn agent_control(&self) -> AgentControl {
        AgentControl::new(Arc::downgrade(&self.state))
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub(crate) fn captured_ops(&self) -> Vec<(ThreadId, Op)> {
        self.state
            .ops_log
            .lock()
            .map(|log| log.clone())
            .unwrap_or_default()
    }
}

impl ThreadManagerState {
    /// Fetch a thread by ID or return ThreadNotFound.
    pub(crate) async fn get_thread(&self, thread_id: ThreadId) -> CodexResult<Arc<CodexThread>> {
        let threads = self.threads.read().await;
        threads
            .get(&thread_id)
            .cloned()
            .ok_or_else(|| CodexErr::ThreadNotFound(thread_id))
    }

    pub(crate) async fn get_status_or_retained(&self, thread_id: ThreadId) -> Option<AgentStatus> {
        if let Ok(thread) = self.get_thread(thread_id).await {
            return Some(thread.agent_status().await);
        }
        let retained = self.retained_agent_statuses.read().await;
        retained.get(&thread_id).map(RetainedAgentStatus::status)
    }

    pub(crate) async fn subscribe_status_or_retained(
        &self,
        thread_id: ThreadId,
    ) -> Option<watch::Receiver<AgentStatus>> {
        if let Ok(thread) = self.get_thread(thread_id).await {
            return Some(thread.subscribe_status());
        }
        let retained = self.retained_agent_statuses.read().await;
        retained
            .get(&thread_id)
            .map(RetainedAgentStatus::subscribe_status)
    }

    pub(crate) async fn list_thread_ids(&self) -> Vec<ThreadId> {
        self.threads.read().await.keys().copied().collect()
    }

    /// Send an operation to a thread by ID.
    pub(crate) async fn send_op(&self, thread_id: ThreadId, op: Op) -> CodexResult<String> {
        let thread = self.get_thread(thread_id).await?;
        #[cfg(any(test, feature = "test-support"))]
        {
            if let Ok(mut log) = self.ops_log.lock() {
                log.push((thread_id, op.clone()));
            }
        }
        thread.submit(op).await
    }

    /// Remove a thread from the manager by ID, returning it when present.
    pub(crate) async fn remove_thread(&self, thread_id: &ThreadId) -> Option<Arc<CodexThread>> {
        let thread = self.threads.write().await.remove(thread_id)?;
        let generation = {
            let generations = self.thread_generations.read().await;
            generations.get(thread_id).copied().unwrap_or_default()
        };
        self.track_detached_thread(*thread_id, generation, thread.clone())
            .await;
        Some(thread)
    }

    /// Spawn a new thread with no history using a provided config.
    pub(crate) async fn spawn_new_thread(
        &self,
        config: Config,
        agent_control: AgentControl,
    ) -> CodexResult<NewThread> {
        self.spawn_new_thread_with_source(config, agent_control, self.session_source.clone())
            .await
    }

    pub(crate) async fn spawn_new_thread_with_source(
        &self,
        config: Config,
        agent_control: AgentControl,
        session_source: SessionSource,
    ) -> CodexResult<NewThread> {
        self.spawn_thread_with_source(
            config,
            InitialHistory::New,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            Vec::new(),
        )
        .await
    }

    /// Spawn a new thread with optional history and register it with the manager.
    pub(crate) async fn spawn_thread(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        agent_control: AgentControl,
        dynamic_tools: Vec<codex_protocol::dynamic_tools::DynamicToolSpec>,
    ) -> CodexResult<NewThread> {
        self.spawn_thread_with_source(
            config,
            initial_history,
            auth_manager,
            agent_control,
            self.session_source.clone(),
            dynamic_tools,
        )
        .await
    }

    pub(crate) async fn spawn_thread_with_source(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        agent_control: AgentControl,
        session_source: SessionSource,
        dynamic_tools: Vec<codex_protocol::dynamic_tools::DynamicToolSpec>,
    ) -> CodexResult<NewThread> {
        let CodexSpawnOk {
            codex, thread_id, ..
        } = Codex::spawn(
            config,
            auth_manager,
            Arc::clone(&self.models_manager),
            Arc::clone(&self.skills_manager),
            initial_history,
            session_source,
            agent_control,
            dynamic_tools,
        )
        .await?;
        self.finalize_thread_spawn(codex, thread_id).await
    }

    pub(crate) async fn spawn_thread_with_initial_history_and_source(
        &self,
        config: Config,
        initial_history: InitialHistory,
        agent_control: AgentControl,
        session_source: SessionSource,
    ) -> CodexResult<NewThread> {
        self.spawn_thread_with_source(
            config,
            initial_history,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            Vec::new(),
        )
        .await
    }

    pub(crate) async fn resume_thread_from_rollout_with_source(
        &self,
        config: Config,
        rollout_path: PathBuf,
        agent_control: AgentControl,
        session_source: SessionSource,
    ) -> CodexResult<NewThread> {
        let initial_history = RolloutRecorder::get_rollout_history(&rollout_path).await?;
        self.spawn_thread_with_initial_history_and_source(
            config,
            initial_history,
            agent_control,
            session_source,
        )
        .await
    }

    async fn finalize_thread_spawn(
        &self,
        codex: Codex,
        thread_id: ThreadId,
    ) -> CodexResult<NewThread> {
        let event = codex.next_event().await?;
        let session_configured = match event {
            Event {
                id,
                msg: EventMsg::SessionConfigured(session_configured),
            } if id == INITIAL_SUBMIT_ID => session_configured,
            _ => {
                return Err(CodexErr::SessionConfiguredNotFirstEvent);
            }
        };

        let thread = Arc::new(CodexThread::new(
            codex,
            session_configured.rollout_path.clone(),
        ));
        self.activate_thread(thread_id, thread.clone()).await;

        Ok(NewThread {
            thread_id,
            thread,
            session_configured,
        })
    }

    pub(crate) fn notify_thread_created(&self, thread_id: ThreadId) {
        let _ = self.thread_created_tx.send(thread_id);
    }

    async fn activate_thread(&self, thread_id: ThreadId, thread: Arc<CodexThread>) {
        {
            let mut generations = self.thread_generations.write().await;
            let generation = generations.entry(thread_id).or_insert(0);
            *generation += 1;
        }
        self.retained_agent_statuses
            .write()
            .await
            .remove(&thread_id);
        self.threads.write().await.insert(thread_id, thread);
    }

    async fn track_detached_thread(
        &self,
        thread_id: ThreadId,
        generation: u64,
        thread: Arc<CodexThread>,
    ) {
        let status = thread.agent_status().await;
        if is_final(&status) {
            self.retained_agent_statuses
                .write()
                .await
                .insert(thread_id, RetainedAgentStatus::Final { generation, status });
            return;
        }

        let status_rx = thread.subscribe_status();
        self.retained_agent_statuses.write().await.insert(
            thread_id,
            RetainedAgentStatus::Pending {
                generation,
                _thread: thread.clone(),
                status_rx: status_rx.clone(),
            },
        );

        let retained_agent_statuses = Arc::clone(&self.retained_agent_statuses);
        tokio::spawn(async move {
            let final_status = wait_for_final_retained_status(status_rx).await;
            let mut retained = retained_agent_statuses.write().await;
            let Some(current) = retained.get(&thread_id) else {
                return;
            };
            if current.generation() != generation {
                return;
            }
            match final_status {
                Some(status) => {
                    retained.insert(thread_id, RetainedAgentStatus::Final { generation, status });
                }
                None => {
                    retained.remove(&thread_id);
                }
            }
        });
    }
}

impl RetainedAgentStatus {
    fn generation(&self) -> u64 {
        match self {
            Self::Pending { generation, .. } | Self::Final { generation, .. } => *generation,
        }
    }

    fn status(&self) -> AgentStatus {
        match self {
            Self::Pending { status_rx, .. } => status_rx.borrow().clone(),
            Self::Final { status, .. } => status.clone(),
        }
    }

    fn subscribe_status(&self) -> watch::Receiver<AgentStatus> {
        match self {
            Self::Pending { status_rx, .. } => status_rx.clone(),
            Self::Final { status, .. } => {
                let (_tx, rx) = watch::channel(status.clone());
                rx
            }
        }
    }
}

async fn wait_for_final_retained_status(
    mut status_rx: watch::Receiver<AgentStatus>,
) -> Option<AgentStatus> {
    let mut status = status_rx.borrow().clone();
    if is_final(&status) {
        return Some(status);
    }

    loop {
        if status_rx.changed().await.is_err() {
            let latest = status_rx.borrow().clone();
            return is_final(&latest).then_some(latest);
        }
        status = status_rx.borrow().clone();
        if is_final(&status) {
            return Some(status);
        }
    }
}

/// Return a prefix of `items` obtained by cutting strictly before the nth user message
/// (0-based) and all items that follow it.
fn truncate_before_nth_user_message(history: InitialHistory, n: usize) -> InitialHistory {
    let items: Vec<RolloutItem> = history.get_rollout_items();
    let rolled = truncation::truncate_rollout_before_nth_user_message_from_start(&items, n);

    if rolled.is_empty() {
        InitialHistory::New
    } else {
        InitialHistory::Forked(rolled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context;
    use assert_matches::assert_matches;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ReasoningItemReasoningSummary;
    use codex_protocol::models::TranscriptItem;
    use pretty_assertions::assert_eq;

    fn user_msg(text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }
    fn assistant_msg(text: &str) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
        }
    }

    #[test]
    fn drops_from_last_user_only() {
        let items = [
            user_msg("u1"),
            assistant_msg("a1"),
            assistant_msg("a2"),
            user_msg("u2"),
            assistant_msg("a3"),
            TranscriptItem::Reasoning {
                id: "r1".to_string(),
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "s".to_string(),
                }],
                content: None,
                encrypted_content: None,
            },
            TranscriptItem::ToolCall {
                id: None,
                call_id: "c1".to_string(),
                tool_name: "tool".to_string(),
                payload: codex_llm::ToolCallPayload::JsonArguments {
                    arguments: "{}".to_string(),
                },
            },
            assistant_msg("a4"),
        ];

        let initial: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::TranscriptItem)
            .collect();
        let truncated = truncate_before_nth_user_message(InitialHistory::Forked(initial), 1);
        let got_items = truncated.get_rollout_items();
        let expected_items = vec![
            RolloutItem::TranscriptItem(items[0].clone()),
            RolloutItem::TranscriptItem(items[1].clone()),
            RolloutItem::TranscriptItem(items[2].clone()),
        ];
        assert_eq!(
            serde_json::to_value(&got_items).unwrap(),
            serde_json::to_value(&expected_items).unwrap()
        );

        let initial2: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::TranscriptItem)
            .collect();
        let truncated2 = truncate_before_nth_user_message(InitialHistory::Forked(initial2), 2);
        assert_matches!(truncated2, InitialHistory::New);
    }

    #[tokio::test]
    async fn ignores_session_prefix_messages_when_truncating() {
        let (session, turn_context) = make_session_and_context().await;
        let mut items = session.build_initial_context(&turn_context).await;
        items.push(user_msg("feature request"));
        items.push(assistant_msg("ack"));
        items.push(user_msg("second question"));
        items.push(assistant_msg("answer"));

        let rollout_items: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::TranscriptItem)
            .collect();

        let truncated = truncate_before_nth_user_message(InitialHistory::Forked(rollout_items), 1);
        let got_items = truncated.get_rollout_items();

        let expected: Vec<RolloutItem> = vec![
            RolloutItem::TranscriptItem(items[0].clone()),
            RolloutItem::TranscriptItem(items[1].clone()),
            RolloutItem::TranscriptItem(items[2].clone()),
            RolloutItem::TranscriptItem(items[3].clone()),
        ];

        assert_eq!(
            serde_json::to_value(&got_items).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }
}
