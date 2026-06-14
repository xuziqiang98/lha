//! Session-wide mutable state.

use std::sync::Arc;
use std::sync::Mutex;

use crate::product::protocol::protocol::GhostSnapshotRecord;
use lha_llm::TranscriptItem;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::product::agent::codex::PromptSettingsSnapshot;
use crate::product::agent::codex::SessionConfiguration;
use crate::product::agent::context_manager::ContextManager;
use crate::product::agent::dynamic_context_window::DynamicContextWindowKey;
use crate::product::agent::dynamic_context_window::DynamicContextWindowState;
use crate::product::agent::protocol::InputSlimmingTokenStats;
use crate::product::agent::protocol::TokenUsage;
use crate::product::agent::protocol::TokenUsageInfo;
use crate::product::agent::truncate::TruncationPolicy;
use crate::product::agent::workflow::WorkflowSession;

/// Persistent, session-scoped state previously stored directly on `Session`.
pub(crate) struct SessionState {
    pub(crate) session_configuration: SessionConfiguration,
    pub(crate) history: ContextManager,
    pub(crate) dynamic_context_windows:
        HashMap<DynamicContextWindowKey, Arc<Mutex<DynamicContextWindowState>>>,
    pub(crate) server_reasoning_included: bool,
    pub(crate) dependency_env: HashMap<String, String>,
    pub(crate) mcp_dependency_prompted: HashSet<String>,
    pub(crate) ghost_snapshots: Vec<GhostSnapshotRecord>,
    pub(crate) workflow: Option<WorkflowSession>,
    /// Whether the session's initial context has been seeded into history.
    ///
    /// TODO(owen): This is a temporary solution to avoid updating a thread's updated_at
    /// timestamp when resuming a session. Remove this once SQLite is in place.
    pub(crate) initial_context_seeded: bool,
    /// Last turn settings that have been reflected in prompt history.
    pub(crate) prompt_settings_snapshot: Option<PromptSettingsSnapshot>,
    /// Whether current prompt history includes memory citation instructions.
    pub(crate) memory_citations_enabled: bool,
    /// Whether reconstructed history may need an explicit identity clear when the
    /// next seeded context has no preset identity.
    pub(crate) pending_identity_clear_from_history: bool,
    pub(crate) input_slimming_total: InputSlimmingTokenStats,
}

impl SessionState {
    /// Create a new session state mirroring previous `State::default()` semantics.
    pub(crate) fn new(session_configuration: SessionConfiguration) -> Self {
        let history = ContextManager::new();
        Self {
            session_configuration,
            history,
            dynamic_context_windows: HashMap::new(),
            server_reasoning_included: false,
            dependency_env: HashMap::new(),
            mcp_dependency_prompted: HashSet::new(),
            ghost_snapshots: Vec::new(),
            workflow: None,
            initial_context_seeded: false,
            prompt_settings_snapshot: None,
            memory_citations_enabled: false,
            pending_identity_clear_from_history: false,
            input_slimming_total: InputSlimmingTokenStats::default(),
        }
    }

    // History helpers
    pub(crate) fn record_items(&mut self, items: &[TranscriptItem], policy: TruncationPolicy) {
        self.history.record_items(items.iter(), policy);
    }

    pub(crate) fn clone_history(&self) -> ContextManager {
        self.history.clone()
    }

    pub(crate) fn replace_history(&mut self, items: Vec<TranscriptItem>) {
        self.history.replace(items);
    }

    pub(crate) fn clone_ghost_snapshots(&self) -> Vec<GhostSnapshotRecord> {
        self.ghost_snapshots.clone()
    }

    pub(crate) fn record_ghost_snapshot(&mut self, item: GhostSnapshotRecord) {
        if let Some(existing) = self
            .ghost_snapshots
            .iter_mut()
            .find(|existing| existing.turn_id == item.turn_id)
        {
            *existing = item;
        } else {
            self.ghost_snapshots.push(item);
        }
    }

    pub(crate) fn get_or_create_dynamic_context_window(
        &mut self,
        key: DynamicContextWindowKey,
    ) -> Arc<Mutex<DynamicContextWindowState>> {
        self.dynamic_context_windows
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(DynamicContextWindowState::new())))
            .clone()
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        self.history.set_token_info(info);
    }

    // Token/rate limit helpers
    pub(crate) fn update_token_info_from_usage(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<i64>,
    ) {
        self.history.update_token_info(usage, model_context_window);
    }

    pub(crate) fn token_info(&self) -> Option<TokenUsageInfo> {
        self.history.token_info()
    }

    pub(crate) fn total_reported_token_usage(&self) -> i64 {
        self.history
            .token_info()
            .map(|info| info.total_token_usage.total_tokens)
            .unwrap_or(0)
    }

    pub(crate) fn set_token_usage_full(&mut self, context_window: i64) {
        self.history.set_token_usage_full(context_window);
    }

    pub(crate) fn record_input_slimming(
        &mut self,
        last: InputSlimmingTokenStats,
    ) -> InputSlimmingTokenStats {
        self.input_slimming_total.add_assign(&last);
        self.input_slimming_total
    }

    pub(crate) fn get_total_token_usage(&self, server_reasoning_included: bool) -> i64 {
        self.history
            .get_total_token_usage(server_reasoning_included)
    }

    pub(crate) fn set_server_reasoning_included(&mut self, included: bool) {
        self.server_reasoning_included = included;
    }

    pub(crate) fn server_reasoning_included(&self) -> bool {
        self.server_reasoning_included
    }

    pub(crate) fn record_mcp_dependency_prompted<I>(&mut self, names: I)
    where
        I: IntoIterator<Item = String>,
    {
        self.mcp_dependency_prompted.extend(names);
    }

    pub(crate) fn mcp_dependency_prompted(&self) -> HashSet<String> {
        self.mcp_dependency_prompted.clone()
    }

    pub(crate) fn set_dependency_env(&mut self, values: HashMap<String, String>) {
        for (key, value) in values {
            self.dependency_env.insert(key, value);
        }
    }

    pub(crate) fn dependency_env(&self) -> HashMap<String, String> {
        self.dependency_env.clone()
    }
}
