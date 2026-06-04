use std::sync::Arc;

use crate::product::agent::codex::TurnContext;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::UndoCompletedEvent;
use crate::product::agent::protocol::UndoStartedEvent;
use crate::product::agent::state::TaskKind;
use crate::product::agent::tasks::SessionTask;
use crate::product::agent::tasks::SessionTaskContext;
use crate::product::git_utils::RestoreGhostCommitOptions;
use crate::product::git_utils::restore_ghost_commit_with_options;
use crate::product::protocol::protocol::GhostSnapshotRecord;
use crate::product::protocol::protocol::GhostSnapshotStatus;
use crate::product::protocol::user_input::UserInput;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::error;
use tracing::info;
use tracing::warn;

pub(crate) struct UndoTask;

impl UndoTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionTask for UndoTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let _ = session
            .session
            .services
            .otel_manager
            .counter("codex.task.undo", 1, &[]);
        let sess = session.clone_session();
        sess.send_event(
            ctx.as_ref(),
            EventMsg::UndoStarted(UndoStartedEvent {
                message: Some("Undo in progress...".to_string()),
            }),
        )
        .await;

        if cancellation_token.is_cancelled() {
            sess.send_event(
                ctx.as_ref(),
                EventMsg::UndoCompleted(UndoCompletedEvent {
                    success: false,
                    message: Some("Undo cancelled.".to_string()),
                }),
            )
            .await;
            return None;
        }

        let mut completed = UndoCompletedEvent {
            success: false,
            message: None,
        };

        let Some((turn_id, ghost_commit)) = sess
            .clone_ghost_snapshots()
            .await
            .into_iter()
            .rev()
            .find_map(|item| match item.status {
                GhostSnapshotStatus::Captured { ghost_commit } => {
                    Some((item.turn_id, ghost_commit))
                }
                GhostSnapshotStatus::Pending | GhostSnapshotStatus::Consumed => None,
            })
        else {
            completed.message = Some("No ghost snapshot available to undo.".to_string());
            sess.send_event(ctx.as_ref(), EventMsg::UndoCompleted(completed))
                .await;
            return None;
        };

        let commit_id = ghost_commit.id().to_string();
        let repo_path = ctx.cwd.clone();
        let ghost_snapshot = ctx.ghost_snapshot.clone();
        let restore_result = tokio::task::spawn_blocking(move || {
            let options = RestoreGhostCommitOptions::new(&repo_path).ghost_snapshot(ghost_snapshot);
            restore_ghost_commit_with_options(&options, &ghost_commit)
        })
        .await;

        match restore_result {
            Ok(Ok(())) => {
                sess.record_ghost_snapshot(
                    ctx.as_ref(),
                    GhostSnapshotRecord {
                        turn_id,
                        status: GhostSnapshotStatus::Consumed,
                    },
                )
                .await;
                let short_id: String = commit_id.chars().take(7).collect();
                info!(commit_id = commit_id, "Undo restored ghost snapshot");
                completed.success = true;
                completed.message = Some(format!("Undo restored snapshot {short_id}."));
            }
            Ok(Err(err)) => {
                let message = format!("Failed to restore snapshot {commit_id}: {err}");
                warn!("{message}");
                completed.message = Some(message);
            }
            Err(err) => {
                let message = format!("Failed to restore snapshot {commit_id}: {err}");
                error!("{message}");
                completed.message = Some(message);
            }
        }

        sess.send_event(ctx.as_ref(), EventMsg::UndoCompleted(completed))
            .await;
        None
    }
}
