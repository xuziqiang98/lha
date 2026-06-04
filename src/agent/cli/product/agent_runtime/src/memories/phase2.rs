use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::product::protocol::config_types::WebSearchMode;
use crate::product::protocol::protocol::AskForApproval;
use crate::product::protocol::protocol::EventMsg;
use crate::product::protocol::protocol::SandboxPolicy;
use crate::product::state::MemoryStore;
use crate::product::state::Phase2JobClaimOutcome;
use crate::product::state::Stage1Output;
use crate::product::utils_absolute_path::AbsolutePathBuf;
use tokio::time::MissedTickBehavior;
use tracing::debug;
use tracing::warn;

use crate::product::agent::config::Config;
use crate::product::agent::config::Constrained;
use crate::product::agent::memories::metrics;
use crate::product::agent::memories::runtime::MemoryStartupContext;
use crate::product::agent::memories::runtime::SpawnedConsolidationAgent;
use crate::product::agent::memories::runtime::disable_consolidation_features;

#[derive(Debug, Clone)]
struct Claim {
    token: String,
    watermark: i64,
}

pub(crate) async fn run(context: Arc<MemoryStartupContext>) {
    let Some(state_db) = context.state_db() else {
        return;
    };
    let Some(memories) = state_db.memories().cloned() else {
        metrics::counter(
            metrics::PHASE2_JOBS,
            1,
            &[("status", "skipped_memory_store_unavailable")],
        );
        return;
    };
    let claim = match claim_global_job(context.as_ref(), &memories).await {
        Ok(claim) => {
            metrics::counter(metrics::PHASE2_JOBS, 1, &[("status", "claimed")]);
            claim
        }
        Err(status) => {
            debug!(status, "memory phase-2 skipped");
            metrics::counter(metrics::PHASE2_JOBS, 1, &[("status", status)]);
            return;
        }
    };

    let memory_root = context.memory_root();
    if let Err(err) =
        crate::product::memories_write::prepare_memory_workspace(memory_root.as_path()).await
    {
        warn!("failed to prepare memory workspace: {err}");
        mark_failed(
            context.as_ref(),
            &memories,
            &claim,
            "failed_prepare_workspace",
        )
        .await;
        return;
    }

    let selected_outputs = match memories
        .get_phase2_input_selection(
            context.config().memories.max_raw_memories_for_consolidation,
            context.config().memories.max_unused_days,
        )
        .await
    {
        Ok(outputs) => outputs,
        Err(err) => {
            warn!("failed to select memory phase-2 inputs: {err}");
            mark_failed(
                context.as_ref(),
                &memories,
                &claim,
                "failed_load_stage1_outputs",
            )
            .await;
            return;
        }
    };
    let completion_watermark = get_watermark(claim.watermark, &selected_outputs);

    if let Err(err) = sync_workspace_inputs(
        memory_root.as_path(),
        &selected_outputs,
        context.config().memories.max_raw_memories_for_consolidation,
    )
    .await
    {
        warn!("failed syncing memory phase-2 workspace inputs: {err}");
        mark_failed(
            context.as_ref(),
            &memories,
            &claim,
            "failed_sync_workspace_inputs",
        )
        .await;
        return;
    }

    let workspace_diff =
        match crate::product::memories_write::memory_workspace_diff(memory_root.as_path()).await {
            Ok(diff) => diff,
            Err(err) => {
                warn!("failed checking memory workspace diff: {err}");
                mark_failed(
                    context.as_ref(),
                    &memories,
                    &claim,
                    "failed_workspace_status",
                )
                .await;
                return;
            }
        };

    if !workspace_diff.has_changes() {
        mark_succeeded(
            &memories,
            &claim,
            completion_watermark,
            &selected_outputs,
            "succeeded_no_workspace_changes",
        )
        .await;
        return;
    }

    if let Err(err) =
        crate::product::memories_write::write_workspace_diff(memory_root.as_path(), &workspace_diff)
            .await
    {
        warn!("failed writing memory workspace diff: {err}");
        mark_failed(
            context.as_ref(),
            &memories,
            &claim,
            "failed_workspace_diff_file",
        )
        .await;
        return;
    }

    let agent_config = match consolidation_config(
        context.config(),
        memory_root.clone(),
        context.stage_two_model(),
    ) {
        Ok(config) => config,
        Err(err) => {
            warn!("failed building memory consolidation config: {err}");
            mark_failed(context.as_ref(), &memories, &claim, "failed_sandbox_policy").await;
            return;
        }
    };
    let prompt = crate::product::memories_write::build_consolidation_prompt(memory_root.as_path());
    let agent = match context
        .spawn_consolidation_agent(agent_config, prompt)
        .await
    {
        Ok(agent) => agent,
        Err(err) => {
            warn!("failed spawning memory consolidation agent: {err}");
            mark_failed(context.as_ref(), &memories, &claim, "failed_spawn_agent").await;
            return;
        }
    };

    let success = run_consolidation_agent(memories.clone(), &claim, &agent).await;
    if success {
        match memories
            .heartbeat_global_phase2_job(
                &claim.token,
                crate::product::memories_write::STAGE_TWO_JOB_LEASE_SECONDS,
            )
            .await
        {
            Ok(true) => {
                if let Err(err) = crate::product::memories_write::reset_memory_workspace_baseline(
                    memory_root.as_path(),
                )
                .await
                {
                    warn!("failed resetting memory workspace baseline: {err}");
                    mark_failed(
                        context.as_ref(),
                        &memories,
                        &claim,
                        "failed_workspace_commit",
                    )
                    .await;
                } else {
                    mark_succeeded(
                        &memories,
                        &claim,
                        completion_watermark,
                        &selected_outputs,
                        "succeeded",
                    )
                    .await;
                }
            }
            Ok(false) => {
                mark_failed(
                    context.as_ref(),
                    &memories,
                    &claim,
                    "failed_confirm_ownership",
                )
                .await;
            }
            Err(err) => {
                warn!("failed confirming memory phase-2 ownership: {err}");
                mark_failed(
                    context.as_ref(),
                    &memories,
                    &claim,
                    "failed_confirm_ownership",
                )
                .await;
            }
        }
    } else {
        mark_failed(context.as_ref(), &memories, &claim, "failed_agent").await;
    }

    if let Err(err) = context.shutdown_consolidation_agent(agent).await {
        warn!("failed shutting down memory consolidation agent: {err}");
    }
}

async fn claim_global_job(
    context: &MemoryStartupContext,
    memories: &MemoryStore,
) -> Result<Claim, &'static str> {
    match memories
        .try_claim_global_phase2_job(
            context.thread_id(),
            crate::product::memories_write::STAGE_TWO_JOB_LEASE_SECONDS,
        )
        .await
        .map_err(|err| {
            warn!("failed claiming memory phase-2 job: {err}");
            "failed_claim"
        })? {
        Phase2JobClaimOutcome::Claimed {
            ownership_token,
            input_watermark,
        } => Ok(Claim {
            token: ownership_token,
            watermark: input_watermark,
        }),
        Phase2JobClaimOutcome::SkippedRetryUnavailable => Err("skipped_retry_unavailable"),
        Phase2JobClaimOutcome::SkippedCooldown => Err("skipped_cooldown"),
        Phase2JobClaimOutcome::SkippedRunning => Err("skipped_running"),
    }
}

async fn sync_workspace_inputs(
    root: &std::path::Path,
    selected_outputs: &[Stage1Output],
    max_raw_memories: usize,
) -> std::io::Result<()> {
    crate::product::memories_write::sync_rollout_summaries_from_memories(
        root,
        selected_outputs,
        max_raw_memories,
    )
    .await?;
    crate::product::memories_write::rebuild_raw_memories_file_from_memories(
        root,
        selected_outputs,
        max_raw_memories,
    )
    .await
}

fn consolidation_config(
    config: &Config,
    memory_root: std::path::PathBuf,
    stage_two_model: String,
) -> anyhow::Result<Config> {
    let mut agent_config = config.clone();
    agent_config.cwd = memory_root.clone();
    agent_config.ephemeral = true;
    disable_consolidation_features(&mut agent_config);
    agent_config.web_search_mode = Some(WebSearchMode::Disabled);
    agent_config.mcp_servers = Constrained::allow_any(HashMap::new());
    agent_config.approval_policy = Constrained::allow_any(AskForApproval::Never);
    agent_config.sandbox_policy = Constrained::allow_any(SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![AbsolutePathBuf::try_from(memory_root.as_path())?],
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    });
    agent_config.model = Some(stage_two_model);
    agent_config.model_reasoning_effort =
        Some(crate::product::protocol::openai_models::ReasoningEffort::Medium);
    Ok(agent_config)
}

async fn run_consolidation_agent(
    memories: MemoryStore,
    claim: &Claim,
    agent: &SpawnedConsolidationAgent,
) -> bool {
    let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(
        crate::product::memories_write::STAGE_TWO_JOB_HEARTBEAT_SECONDS,
    ));
    heartbeat_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut latest_token_usage = None;
    loop {
        tokio::select! {
            event = agent.thread.next_event() => {
                match event {
                    Ok(event) => match event.msg {
                        EventMsg::TurnComplete(_) => {
                            emit_phase2_token_usage(latest_token_usage);
                            return true;
                        }
                        EventMsg::TokenCount(token_count) => {
                            latest_token_usage = token_count
                                .info
                                .map(|info| info.last_token_usage.total_tokens);
                        }
                        EventMsg::Error(_) | EventMsg::StreamError(_) | EventMsg::ShutdownComplete => {
                            emit_phase2_token_usage(latest_token_usage);
                            return false;
                        }
                        _ => {}
                    },
                    Err(err) => {
                        warn!("memory consolidation agent event loop failed: {err}");
                        return false;
                    }
                }
            }
            _ = heartbeat_interval.tick() => {
                match memories
                    .heartbeat_global_phase2_job(
                        &claim.token,
                        crate::product::memories_write::STAGE_TWO_JOB_LEASE_SECONDS,
                    )
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        warn!(
                            thread_id = %agent.thread_id,
                            "lost memory phase-2 ownership during consolidation heartbeat"
                        );
                        return false;
                    }
                    Err(err) => {
                        warn!(
                            thread_id = %agent.thread_id,
                            "memory phase-2 heartbeat failed: {err}"
                        );
                        return false;
                    }
                }
            }
        }
    }
}

fn emit_phase2_token_usage(total_tokens: Option<i64>) {
    let Some(total_tokens) = total_tokens else {
        return;
    };
    metrics::counter(
        metrics::PHASE2_TOKEN_USAGE,
        u64::try_from(total_tokens.max(0)).unwrap_or(u64::MAX),
        &[("kind", "total")],
    );
}

async fn mark_failed(
    _context: &MemoryStartupContext,
    memories: &MemoryStore,
    claim: &Claim,
    reason: &str,
) {
    metrics::counter(metrics::PHASE2_JOBS, 1, &[("status", reason)]);
    match memories
        .mark_global_phase2_job_failed(
            &claim.token,
            reason,
            crate::product::memories_write::STAGE_TWO_JOB_RETRY_DELAY_SECONDS,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            let _ = memories
                .mark_global_phase2_job_failed_if_unowned(
                    &claim.token,
                    reason,
                    crate::product::memories_write::STAGE_TWO_JOB_RETRY_DELAY_SECONDS,
                )
                .await;
        }
        Err(err) => warn!("failed marking memory phase-2 job failed: {err}"),
    }
}

async fn mark_succeeded(
    memories: &MemoryStore,
    claim: &Claim,
    completion_watermark: i64,
    selected_outputs: &[Stage1Output],
    status: &'static str,
) {
    metrics::counter(metrics::PHASE2_JOBS, 1, &[("status", status)]);
    if let Err(err) = memories
        .mark_global_phase2_job_succeeded(&claim.token, completion_watermark, selected_outputs)
        .await
    {
        warn!("failed marking memory phase-2 job succeeded: {err}");
    }
}

fn get_watermark(claimed_watermark: i64, selected_outputs: &[Stage1Output]) -> i64 {
    selected_outputs
        .iter()
        .map(|memory| memory.source_updated_at.timestamp())
        .max()
        .unwrap_or(claimed_watermark)
        .max(claimed_watermark)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::features::Feature;
    use pretty_assertions::assert_eq;

    #[test]
    fn consolidation_config_disables_external_context_and_memory_features() {
        let mut config = crate::product::agent::config::test_config();
        config.features.enable(Feature::MemoryTool);
        config.features.enable(Feature::AgentJobs);
        config.features.enable(Feature::WebSearchRequest);
        config.features.enable(Feature::WebSearchCached);
        config.memories.generate_memories = true;
        config.memories.use_memories = true;
        config.web_search_mode = Some(WebSearchMode::Live);
        config.mcp_servers = Constrained::allow_any(HashMap::from([(
            "demo".to_string(),
            crate::product::agent::config::types::McpServerConfig {
                transport: crate::product::agent::config::types::McpServerTransportConfig::Stdio {
                    command: "demo".to_string(),
                    args: Vec::new(),
                    env: None,
                    env_vars: Vec::new(),
                    cwd: None,
                },
                enabled: true,
                disabled_reason: None,
                startup_timeout_sec: None,
                tool_timeout_sec: None,
                enabled_tools: None,
                disabled_tools: None,
                scopes: None,
            },
        )]));
        let memory_root = config.lha_home.join("memories");

        let agent_config =
            consolidation_config(&config, memory_root.clone(), "stage-two-model".to_string())
                .expect("config");

        assert_eq!(agent_config.model.as_deref(), Some("stage-two-model"));
        assert!(agent_config.ephemeral);
        assert!(!agent_config.features.enabled(Feature::MemoryTool));
        assert!(!agent_config.features.enabled(Feature::AgentJobs));
        assert!(!agent_config.features.enabled(Feature::WebSearchRequest));
        assert!(!agent_config.features.enabled(Feature::WebSearchCached));
        assert!(!agent_config.memories.generate_memories);
        assert!(!agent_config.memories.use_memories);
        assert_eq!(agent_config.web_search_mode, Some(WebSearchMode::Disabled));
        assert!(agent_config.mcp_servers.get().is_empty());
        assert_eq!(agent_config.approval_policy.value(), AskForApproval::Never);
        assert_eq!(
            agent_config.sandbox_policy.get(),
            &SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![AbsolutePathBuf::try_from(memory_root.as_path()).unwrap()],
                network_access: false,
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            }
        );
    }
}
