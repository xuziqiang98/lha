use std::sync::Arc;

use tracing::debug;
use tracing::warn;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::features::Feature;

pub(crate) async fn maybe_mark_memory_polluted(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    reason: &'static str,
) {
    let config = turn_context.runtime.config();
    if !config.features.enabled(Feature::MemoryTool) || !config.memories.disable_on_external_context
    {
        return;
    }
    let Some(state_db) = sess.state_db() else {
        return;
    };
    match state_db
        .memories()
        .mark_thread_memory_mode_polluted(sess.conversation_id)
        .await
    {
        Ok(true) => debug!(%reason, thread_id = %sess.conversation_id, "marked memory polluted"),
        Ok(false) => {}
        Err(err) => warn!("failed marking memory polluted after {reason}: {err}"),
    }
}
