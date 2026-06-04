use std::sync::Arc;

use crate::product::agent::AuthManager;
use crate::product::agent::CodexThread;
use crate::product::agent::config::Config;
use crate::product::agent::features::Feature;
use crate::product::agent::memories::metrics;
use crate::product::agent::memories::runtime::MemoryStartupContext;
use crate::product::agent::models_manager::manager::ModelsManager;
use crate::product::agent::skills::SkillsManager;
use crate::product::protocol::ThreadId;
use crate::product::protocol::protocol::SessionSource;
use tracing::debug;
use tracing::warn;

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_memories_startup_task(
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    skills_manager: Arc<SkillsManager>,
    config: Config,
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    session_source: SessionSource,
) {
    if config.ephemeral {
        metrics::counter(metrics::STARTUP, 1, &[("status", "skipped_ephemeral")]);
        return;
    }
    if !config.features.enabled(Feature::MemoryTool) {
        metrics::counter(
            metrics::STARTUP,
            1,
            &[("status", "skipped_feature_disabled")],
        );
        return;
    }
    if session_source.is_non_root_agent() {
        metrics::counter(metrics::STARTUP, 1, &[("status", "skipped_non_root_agent")]);
        return;
    }
    if thread.state_db().is_none() {
        metrics::counter(
            metrics::STARTUP,
            1,
            &[("status", "skipped_state_db_unavailable")],
        );
        return;
    }

    let context = Arc::new(MemoryStartupContext::new(
        auth_manager,
        models_manager,
        skills_manager,
        thread_id,
        thread,
        config,
        session_source,
    ));
    tokio::spawn(async move {
        metrics::counter(metrics::STARTUP, 1, &[("status", "started")]);
        let memory_root = context.memory_root();
        if let Err(err) = crate::product::memories_write::ensure_layout(memory_root.as_path()).await
        {
            warn!("failed to initialize memories layout: {err}");
            metrics::counter(metrics::STARTUP, 1, &[("status", "layout_error")]);
            return;
        }
        if let Some(state_db) = context.state_db()
            && let Some(memories) = state_db.memories()
            && let Err(err) = memories
                .prune_stage1_outputs_for_retention(
                    context.config().memories.max_unused_days,
                    crate::product::memories_write::STAGE_ONE_PRUNE_BATCH_SIZE,
                )
                .await
        {
            warn!("failed to prune memory stage1 outputs: {err}");
        }

        debug!(
            min_rate_limit_remaining_percent =
                context.config().memories.min_rate_limit_remaining_percent,
            "memory startup rate-limit snapshot unavailable; continuing"
        );
        metrics::counter(
            metrics::STARTUP,
            1,
            &[("status", "rate_limit_snapshot_unavailable_continuing")],
        );

        crate::product::agent::memories::phase1::run(Arc::clone(&context)).await;
        crate::product::agent::memories::phase2::run(context).await;
        metrics::counter(metrics::STARTUP, 1, &[("status", "completed")]);
    });
}
