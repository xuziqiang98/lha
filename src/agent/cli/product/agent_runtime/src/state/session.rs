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
use crate::product::agent::input_slimming::CandidateZone;
use crate::product::agent::input_slimming::InputSlimmingContextStatCandidate;
use crate::product::agent::input_slimming::InputSlimmingOccurrenceKey;
use crate::product::agent::input_slimming::billing::InputSlimmingBillingPending;
use crate::product::agent::input_slimming::billing::input_slimming_saved_usd_micros;
use crate::product::agent::protocol::InputSlimmingScope;
use crate::product::agent::protocol::InputSlimmingTokenStats;
use crate::product::agent::protocol::TokenUsage;
use crate::product::agent::protocol::TokenUsageInfo;
use crate::product::agent::truncate::TruncationPolicy;
use crate::product::agent::workflow::WorkflowSession;
use crate::product::protocol::openai_models::ModelPricing;

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
    pub(crate) input_slimming_counted_occurrences: HashSet<InputSlimmingOccurrenceKey>,
    pub(crate) pending_input_slimming_billing: HashMap<String, PendingInputSlimmingBilling>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompletedInputSlimmingBilling {
    pub(crate) scope: InputSlimmingScope,
    pub(crate) last: InputSlimmingTokenStats,
    pub(crate) total: InputSlimmingTokenStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingInputSlimmingBilling {
    scope: InputSlimmingScope,
    last: InputSlimmingTokenStats,
    pending: InputSlimmingBillingPending,
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
            input_slimming_counted_occurrences: HashSet::new(),
            pending_input_slimming_billing: HashMap::new(),
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

    pub(crate) fn record_input_slimming_context(
        &mut self,
        turn_id: &str,
        scope: InputSlimmingScope,
        candidates: &[InputSlimmingContextStatCandidate],
    ) -> Option<(InputSlimmingTokenStats, InputSlimmingTokenStats)> {
        let mut last = InputSlimmingTokenStats::default();
        let mut pending = InputSlimmingBillingPending::default();
        for candidate in candidates {
            if candidate.tokens_saved <= 0 {
                continue;
            }
            if !self
                .input_slimming_counted_occurrences
                .insert(candidate.occurrence_key.clone())
            {
                continue;
            }
            last.tokens_before = last.tokens_before.saturating_add(candidate.tokens_before);
            last.tokens_after = last.tokens_after.saturating_add(candidate.tokens_after);
            last.tokens_saved = last.tokens_saved.saturating_add(candidate.tokens_saved);
            last.replacements = last.replacements.saturating_add(1);
            match candidate.zone {
                CandidateZone::HistoricalToolOutput => {
                    pending.saved_historical_tokens = pending
                        .saved_historical_tokens
                        .saturating_add(candidate.tokens_saved);
                }
                CandidateZone::LiveToolOutput => {
                    pending.saved_live_tokens = pending
                        .saved_live_tokens
                        .saturating_add(candidate.tokens_saved);
                }
            }
        }
        if last.tokens_saved <= 0 || last.replacements <= 0 {
            return None;
        }
        self.input_slimming_total.add_assign(&last);
        self.pending_input_slimming_billing
            .entry(turn_id.to_string())
            .and_modify(|existing| {
                existing.last.add_assign(&last);
                existing.pending.saved_historical_tokens = existing
                    .pending
                    .saved_historical_tokens
                    .saturating_add(pending.saved_historical_tokens);
                existing.pending.saved_live_tokens = existing
                    .pending
                    .saved_live_tokens
                    .saturating_add(pending.saved_live_tokens);
            })
            .or_insert(PendingInputSlimmingBilling {
                scope,
                last,
                pending,
            });
        Some((last, self.input_slimming_total))
    }

    pub(crate) fn complete_input_slimming_billing(
        &mut self,
        turn_id: &str,
        pricing: Option<&ModelPricing>,
        usage: Option<&TokenUsage>,
    ) -> Option<CompletedInputSlimmingBilling> {
        let pending = self.pending_input_slimming_billing.remove(turn_id)?;
        let usage = usage?;
        let saved_usd_micros = input_slimming_saved_usd_micros(pricing, usage, pending.pending)?;
        let mut last = pending.last;
        last.saved_usd_micros = Some(saved_usd_micros);
        self.input_slimming_total
            .add_assign(&InputSlimmingTokenStats {
                saved_usd_micros: Some(saved_usd_micros),
                ..InputSlimmingTokenStats::default()
            });
        Some(CompletedInputSlimmingBilling {
            scope: pending.scope,
            last,
            total: self.input_slimming_total,
        })
    }

    pub(crate) fn discard_input_slimming_billing(&mut self, turn_id: &str) {
        self.pending_input_slimming_billing.remove(turn_id);
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
