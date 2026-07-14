use crate::product::otel::OtelManager;
use crate::product::protocol::ThreadId;
use crate::product::protocol::protocol::RolloutItem;
use crate::product::state::DB_ERROR_METRIC;
use crate::product::state::LogEntry;
use crate::product::state::LogQuery;
use crate::product::state::LogRow;
use crate::product::state::SortKey;
use crate::product::state::ThreadMetadata;
use crate::product::state::ThreadMetadataBuilder;
use crate::product::state::ThreadsPage;
use crate::product::state::apply_rollout_item;
use crate::product::state::migrations::MEMORIES_MIGRATOR;
use crate::product::state::migrations::MIGRATOR;
use crate::product::state::model::Phase2JobClaimOutcome;
use crate::product::state::model::Stage1JobClaim;
use crate::product::state::model::Stage1JobClaimOutcome;
use crate::product::state::model::Stage1Output;
use crate::product::state::model::Stage1StartupClaimParams;
use crate::product::state::model::ThreadGoalRow;
use crate::product::state::model::ThreadRow;
use crate::product::state::model::anchor_from_item;
use crate::product::state::model::datetime_to_epoch_millis;
use crate::product::state::model::datetime_to_epoch_seconds;
use crate::product::state::model::epoch_seconds_to_datetime;
use crate::product::state::paths::file_modified_time_utc;
use chrono::DateTime;
use chrono::Utc;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::sqlite::SqliteSynchronous;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;
use uuid::Uuid;

pub const STATE_DB_FILENAME: &str = "state.sqlite";
pub const MEMORIES_DB_FILENAME: &str = "memories_1.sqlite";

const METRIC_DB_INIT: &str = "lha.db.init";

pub struct GoalUpdate {
    pub objective: Option<String>,
    pub status: Option<crate::product::state::ThreadGoalStatus>,
    pub token_budget: Option<Option<i64>>,
    pub expected_goal_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadGoalSeed {
    pub objective: String,
    pub status: crate::product::state::ThreadGoalStatus,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum GoalAccountingOutcome {
    Unchanged(Option<crate::product::state::ThreadGoal>),
    Updated(crate::product::state::ThreadGoal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// The shared prefix documents that every mode accounts against an active goal baseline.
#[allow(clippy::enum_variant_names)]
pub enum GoalAccountingMode {
    ActiveStatusOnly,
    ActiveOnly,
    ActiveOrComplete,
    ActiveOrStopped,
}

const JOB_KIND_MEMORY_STAGE1: &str = "memory_stage1";
const JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL: &str = "memory_consolidate_global";
const MEMORY_CONSOLIDATION_JOB_KEY: &str = "global";
const PHASE2_SUCCESS_COOLDOWN_SECONDS: i64 = 6 * 60 * 60;
const PHASE2_INPUT_SELECTION_PAGE_SIZE: usize = 512;
const DEFAULT_RETRY_REMAINING: i64 = 3;

/// Store for generated memory state and memory extraction/consolidation jobs.
#[derive(Clone)]
pub struct MemoryStore {
    pool: Arc<SqlitePool>,
    state_pool: Arc<SqlitePool>,
}

impl MemoryStore {
    fn new(pool: Arc<SqlitePool>, state_pool: Arc<SqlitePool>) -> Self {
        Self { pool, state_pool }
    }

    pub async fn clear_memory_data(&self) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM stage1_outputs")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM jobs WHERE kind IN (?, ?)")
            .bind(JOB_KIND_MEMORY_STAGE1)
            .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn record_stage1_output_usage(
        &self,
        thread_ids: &[ThreadId],
    ) -> anyhow::Result<usize> {
        if thread_ids.is_empty() {
            return Ok(0);
        }
        let now = Utc::now().timestamp();
        let mut tx = self.pool.begin().await?;
        let mut updated_rows = 0;
        for thread_id in thread_ids {
            updated_rows += sqlx::query(
                r#"
UPDATE stage1_outputs
SET usage_count = COALESCE(usage_count, 0) + 1,
    last_usage = ?
WHERE thread_id = ?
                "#,
            )
            .bind(now)
            .bind(thread_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;
        }
        tx.commit().await?;
        Ok(updated_rows)
    }

    pub async fn claim_stage1_jobs_for_startup(
        &self,
        current_thread_id: ThreadId,
        params: Stage1StartupClaimParams<'_>,
    ) -> anyhow::Result<Vec<Stage1JobClaim>> {
        let Stage1StartupClaimParams {
            scan_limit,
            max_claimed,
            max_age_days,
            min_rollout_idle_hours,
            allowed_sources,
            lease_seconds,
        } = params;
        if scan_limit == 0 || max_claimed == 0 {
            return Ok(Vec::new());
        }

        let max_age_cutoff = (Utc::now() - chrono::Duration::days(max_age_days.max(0))).timestamp();
        let idle_cutoff =
            (Utc::now() - chrono::Duration::hours(min_rollout_idle_hours.max(0))).timestamp();
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
SELECT
    id,
    rollout_path,
    created_at,
    updated_at,
    source,
    model_provider,
    cwd,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    has_user_event,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
FROM threads
WHERE archived = 0
  AND has_user_event = 1
  AND memory_mode = 'enabled'
  AND id !=
            "#,
        );
        builder.push_bind(current_thread_id.to_string());
        builder
            .push(" AND updated_at >= ")
            .push_bind(max_age_cutoff);
        builder.push(" AND updated_at <= ").push_bind(idle_cutoff);
        if !allowed_sources.is_empty() {
            builder.push(" AND source IN (");
            let mut separated = builder.separated(", ");
            for source in allowed_sources {
                separated.push_bind(source);
            }
            separated.push_unseparated(")");
        }
        builder.push(" ORDER BY updated_at DESC, id DESC LIMIT ");
        builder.push_bind(scan_limit as i64);

        let rows = builder.build().fetch_all(self.state_pool.as_ref()).await?;
        let mut claims = Vec::new();
        for row in rows {
            if claims.len() >= max_claimed {
                break;
            }
            let thread = ThreadRow::try_from_row(&row).and_then(ThreadMetadata::try_from)?;
            let source_updated_at = datetime_to_epoch_seconds(thread.updated_at);
            match self
                .try_claim_stage1_job(
                    thread.id,
                    current_thread_id,
                    source_updated_at,
                    lease_seconds,
                )
                .await?
            {
                Stage1JobClaimOutcome::Claimed { ownership_token } => {
                    claims.push(Stage1JobClaim {
                        thread,
                        ownership_token,
                    });
                }
                Stage1JobClaimOutcome::SkippedUpToDate
                | Stage1JobClaimOutcome::SkippedRunning
                | Stage1JobClaimOutcome::SkippedRetryBackoff
                | Stage1JobClaimOutcome::SkippedRetryExhausted => {}
            }
        }
        Ok(claims)
    }

    async fn try_claim_stage1_job(
        &self,
        thread_id: ThreadId,
        worker_id: ThreadId,
        source_updated_at: i64,
        lease_seconds: i64,
    ) -> anyhow::Result<Stage1JobClaimOutcome> {
        if !self
            .stage1_source_needs_update(thread_id, source_updated_at)
            .await?
        {
            return Ok(Stage1JobClaimOutcome::SkippedUpToDate);
        }
        let now = Utc::now().timestamp();
        let lease_until = now.saturating_add(lease_seconds.max(0));
        let ownership_token = uuid::Uuid::new_v4().to_string();
        let thread_key = thread_id.to_string();

        let row = sqlx::query(
            r#"
SELECT status, lease_until, retry_at, retry_remaining
FROM jobs
WHERE kind = ? AND job_key = ?
            "#,
        )
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(thread_key.as_str())
        .fetch_optional(self.pool.as_ref())
        .await?;
        if let Some(row) = row {
            let status: String = row.try_get("status")?;
            let existing_lease_until: Option<i64> = row.try_get("lease_until")?;
            let retry_at: Option<i64> = row.try_get("retry_at")?;
            let retry_remaining: i64 = row.try_get("retry_remaining")?;
            if retry_remaining <= 0 {
                return Ok(Stage1JobClaimOutcome::SkippedRetryExhausted);
            }
            if retry_at.is_some_and(|retry_at| retry_at > now) {
                return Ok(Stage1JobClaimOutcome::SkippedRetryBackoff);
            }
            if status == "running" && existing_lease_until.is_some_and(|value| value > now) {
                return Ok(Stage1JobClaimOutcome::SkippedRunning);
            }
        }

        let result = sqlx::query(
            r#"
INSERT INTO jobs (
    kind, job_key, status, worker_id, ownership_token, started_at, lease_until, retry_remaining, input_watermark
) VALUES (?, ?, 'running', ?, ?, ?, ?, ?, ?)
ON CONFLICT(kind, job_key) DO UPDATE SET
    status = 'running',
    worker_id = excluded.worker_id,
    ownership_token = excluded.ownership_token,
    started_at = excluded.started_at,
    finished_at = NULL,
    lease_until = excluded.lease_until,
    input_watermark = excluded.input_watermark,
    last_error = NULL
WHERE jobs.retry_remaining > 0
  AND (jobs.retry_at IS NULL OR jobs.retry_at <= excluded.started_at)
  AND (jobs.status != 'running' OR jobs.lease_until IS NULL OR jobs.lease_until <= excluded.started_at)
            "#,
        )
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(thread_key.as_str())
        .bind(worker_id.to_string())
        .bind(ownership_token.as_str())
        .bind(now)
        .bind(lease_until)
        .bind(DEFAULT_RETRY_REMAINING)
        .bind(source_updated_at)
        .execute(self.pool.as_ref())
        .await?;
        if result.rows_affected() == 0 {
            return Ok(Stage1JobClaimOutcome::SkippedRunning);
        }
        Ok(Stage1JobClaimOutcome::Claimed { ownership_token })
    }

    async fn stage1_source_needs_update(
        &self,
        thread_id: ThreadId,
        source_updated_at: i64,
    ) -> anyhow::Result<bool> {
        let thread_id = thread_id.to_string();
        if let Some(row) =
            sqlx::query("SELECT source_updated_at FROM stage1_outputs WHERE thread_id = ?")
                .bind(thread_id.as_str())
                .fetch_optional(self.pool.as_ref())
                .await?
        {
            let existing_source_updated_at: i64 = row.try_get("source_updated_at")?;
            if existing_source_updated_at >= source_updated_at {
                return Ok(false);
            }
        }
        if let Some(row) =
            sqlx::query("SELECT last_success_watermark FROM jobs WHERE kind = ? AND job_key = ?")
                .bind(JOB_KIND_MEMORY_STAGE1)
                .bind(thread_id.as_str())
                .fetch_optional(self.pool.as_ref())
                .await?
        {
            let last_success_watermark: Option<i64> = row.try_get("last_success_watermark")?;
            if last_success_watermark.is_some_and(|watermark| watermark >= source_updated_at) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn mark_stage1_job_succeeded(
        &self,
        thread_id: ThreadId,
        ownership_token: &str,
        source_updated_at: i64,
        raw_memory: &str,
        rollout_summary: &str,
        rollout_slug: Option<&str>,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
UPDATE jobs
SET status = 'done',
    finished_at = ?,
    lease_until = NULL,
    retry_at = NULL,
    last_success_watermark = ?,
    last_error = NULL
WHERE kind = ? AND job_key = ? AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(source_updated_at)
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(thread_id.to_string())
        .bind(ownership_token)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            r#"
INSERT INTO stage1_outputs (
    thread_id, source_updated_at, raw_memory, rollout_summary, rollout_slug, generated_at
) VALUES (?, ?, ?, ?, ?, ?)
ON CONFLICT(thread_id) DO UPDATE SET
    source_updated_at = excluded.source_updated_at,
    raw_memory = excluded.raw_memory,
    rollout_summary = excluded.rollout_summary,
    rollout_slug = excluded.rollout_slug,
    generated_at = excluded.generated_at
WHERE excluded.source_updated_at >= stage1_outputs.source_updated_at
            "#,
        )
        .bind(thread_id.to_string())
        .bind(source_updated_at)
        .bind(raw_memory)
        .bind(rollout_summary)
        .bind(rollout_slug)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        self.bump_phase2_input_watermark_tx(&mut tx, source_updated_at)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn mark_stage1_job_succeeded_no_output(
        &self,
        thread_id: ThreadId,
        ownership_token: &str,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
UPDATE jobs
SET status = 'done',
    finished_at = ?,
    lease_until = NULL,
    retry_at = NULL,
    last_success_watermark = input_watermark,
    last_error = NULL
WHERE kind = ? AND job_key = ? AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(thread_id.to_string())
        .bind(ownership_token)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(false);
        }

        let source_updated_at = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT input_watermark FROM jobs WHERE kind = ? AND job_key = ? AND ownership_token = ?",
        )
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(thread_id.to_string())
        .bind(ownership_token)
        .fetch_one(&mut *tx)
        .await?
        .unwrap_or(now);

        let deleted_rows = sqlx::query("DELETE FROM stage1_outputs WHERE thread_id = ?")
            .bind(thread_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted_rows > 0 {
            self.bump_phase2_input_watermark_tx(&mut tx, source_updated_at)
                .await?;
        }

        tx.commit().await?;
        Ok(true)
    }

    pub async fn mark_stage1_job_failed(
        &self,
        thread_id: ThreadId,
        ownership_token: &str,
        reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let retry_at = now.saturating_add(retry_delay_seconds.max(0));
        let result = sqlx::query(
            r#"
UPDATE jobs
SET status = 'error',
    finished_at = ?,
    lease_until = NULL,
    retry_at = ?,
    retry_remaining = max(retry_remaining - 1, 0),
    last_error = ?
WHERE kind = ? AND job_key = ? AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(retry_at)
        .bind(reason)
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(thread_id.to_string())
        .bind(ownership_token)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_phase2_input_selection(
        &self,
        limit: usize,
        max_unused_days: i64,
    ) -> anyhow::Result<Vec<Stage1Output>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let cutoff = (Utc::now() - chrono::Duration::days(max_unused_days.max(0))).timestamp();

        let page_size = limit.clamp(1, PHASE2_INPUT_SELECTION_PAGE_SIZE);
        let mut offset = 0_i64;
        let mut selected_keys = Vec::with_capacity(limit);

        while selected_keys.len() < limit {
            let candidate_rows = sqlx::query(
                r#"
SELECT thread_id, source_updated_at
FROM stage1_outputs
WHERE (length(trim(raw_memory)) > 0 OR length(trim(rollout_summary)) > 0)
  AND (
        (last_usage IS NOT NULL AND last_usage >= ?)
        OR (last_usage IS NULL AND source_updated_at >= ?)
  )
ORDER BY
    COALESCE(usage_count, 0) DESC,
    COALESCE(last_usage, source_updated_at) DESC,
    source_updated_at DESC,
    thread_id DESC
LIMIT ? OFFSET ?
                "#,
            )
            .bind(cutoff)
            .bind(cutoff)
            .bind(page_size as i64)
            .bind(offset)
            .fetch_all(self.pool.as_ref())
            .await?;
            if candidate_rows.is_empty() {
                break;
            }

            let candidate_count = i64::try_from(candidate_rows.len()).unwrap_or(i64::MAX);
            for row in candidate_rows {
                let thread_id: String = row.try_get("thread_id")?;
                let source_updated_at: i64 = row.try_get("source_updated_at")?;
                if self
                    .enabled_thread_metadata(ThreadId::try_from(thread_id.as_str())?)
                    .await?
                    .is_some()
                {
                    selected_keys.push((thread_id, source_updated_at));
                    if selected_keys.len() >= limit {
                        break;
                    }
                }
            }

            offset = offset.saturating_add(candidate_count);
        }

        let mut selected = Vec::with_capacity(selected_keys.len());
        for (thread_id, source_updated_at) in selected_keys {
            let Some(row) = sqlx::query(
                r#"
SELECT
    thread_id,
    source_updated_at,
    raw_memory,
    rollout_summary,
    rollout_slug,
    generated_at
FROM stage1_outputs
WHERE thread_id = ? AND source_updated_at = ?
                "#,
            )
            .bind(thread_id.as_str())
            .bind(source_updated_at)
            .fetch_optional(self.pool.as_ref())
            .await?
            else {
                continue;
            };
            if let Some(output) = self.stage1_output_from_row_if_thread_enabled(row).await? {
                selected.push(output);
            }
        }
        selected.sort_by_key(|output| output.thread_id.to_string());
        Ok(selected)
    }

    pub async fn list_stage1_outputs_for_global(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<Stage1Output>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            r#"
SELECT
    thread_id,
    source_updated_at,
    raw_memory,
    rollout_summary,
    rollout_slug,
    generated_at
FROM stage1_outputs
WHERE length(trim(raw_memory)) > 0 OR length(trim(rollout_summary)) > 0
ORDER BY source_updated_at DESC, thread_id DESC
            "#,
        )
        .fetch_all(self.pool.as_ref())
        .await?;

        let mut outputs = Vec::new();
        for row in rows {
            if let Some(output) = self.stage1_output_from_row_if_thread_enabled(row).await? {
                outputs.push(output);
                if outputs.len() >= limit {
                    break;
                }
            }
        }
        Ok(outputs)
    }

    pub async fn prune_stage1_outputs_for_retention(
        &self,
        max_unused_days: i64,
        limit: usize,
    ) -> anyhow::Result<usize> {
        if limit == 0 {
            return Ok(0);
        }
        let cutoff = (Utc::now() - chrono::Duration::days(max_unused_days.max(0))).timestamp();
        let rows_affected = sqlx::query(
            r#"
DELETE FROM stage1_outputs
WHERE thread_id IN (
    SELECT thread_id
    FROM stage1_outputs
    WHERE selected_for_phase2 = 0
      AND COALESCE(last_usage, source_updated_at) < ?
    ORDER BY
      COALESCE(last_usage, source_updated_at) ASC,
      source_updated_at ASC,
      thread_id ASC
    LIMIT ?
)
            "#,
        )
        .bind(cutoff)
        .bind(limit as i64)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();
        Ok(rows_affected as usize)
    }

    async fn stage1_output_from_row_if_thread_enabled(
        &self,
        row: sqlx::sqlite::SqliteRow,
    ) -> anyhow::Result<Option<Stage1Output>> {
        let thread_id: String = row.try_get("thread_id")?;
        let Some(thread) = self
            .enabled_thread_metadata(ThreadId::try_from(thread_id.as_str())?)
            .await?
        else {
            return Ok(None);
        };
        stage1_output_from_row_and_thread(row, thread).map(Some)
    }

    async fn enabled_thread_metadata(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<Option<ThreadMetadata>> {
        let row = sqlx::query(
            r#"
SELECT
    id,
    rollout_path,
    created_at,
    updated_at,
    source,
    model_provider,
    cwd,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    has_user_event,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
FROM threads
WHERE id = ? AND memory_mode = 'enabled'
            "#,
        )
        .bind(thread_id.to_string())
        .fetch_optional(self.state_pool.as_ref())
        .await?;
        row.map(|row| ThreadRow::try_from_row(&row).and_then(ThreadMetadata::try_from))
            .transpose()
    }

    pub async fn try_claim_global_phase2_job(
        &self,
        worker_id: ThreadId,
        lease_seconds: i64,
    ) -> anyhow::Result<Phase2JobClaimOutcome> {
        let now = Utc::now().timestamp();
        let lease_until = now.saturating_add(lease_seconds.max(0));
        if let Some(row) =
            sqlx::query("SELECT status, lease_until, retry_at, input_watermark, finished_at FROM jobs WHERE kind = ? AND job_key = ?")
                .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
                .bind(MEMORY_CONSOLIDATION_JOB_KEY)
                .fetch_optional(self.pool.as_ref())
                .await?
        {
            let status: String = row.try_get("status")?;
            let existing_lease_until: Option<i64> = row.try_get("lease_until")?;
            let retry_at: Option<i64> = row.try_get("retry_at")?;
            let finished_at: Option<i64> = row.try_get("finished_at")?;
            if retry_at.is_some_and(|value| value > now) {
                return Ok(Phase2JobClaimOutcome::SkippedRetryUnavailable);
            }
            if status == "running" && existing_lease_until.is_some_and(|value| value > now) {
                return Ok(Phase2JobClaimOutcome::SkippedRunning);
            }
            if status == "done"
                && finished_at.is_some_and(|value| now.saturating_sub(value) < PHASE2_SUCCESS_COOLDOWN_SECONDS)
            {
                return Ok(Phase2JobClaimOutcome::SkippedCooldown);
            }
        }
        let token = uuid::Uuid::new_v4().to_string();
        let result = sqlx::query(
            r#"
INSERT INTO jobs (
    kind, job_key, status, worker_id, ownership_token, started_at, lease_until, retry_remaining, input_watermark
) VALUES (?, ?, 'running', ?, ?, ?, ?, ?, 0)
ON CONFLICT(kind, job_key) DO UPDATE SET
    status = 'running',
    worker_id = excluded.worker_id,
    ownership_token = excluded.ownership_token,
    started_at = excluded.started_at,
    finished_at = NULL,
    lease_until = excluded.lease_until,
    last_error = NULL
WHERE jobs.retry_remaining > 0
  AND (jobs.retry_at IS NULL OR jobs.retry_at <= excluded.started_at)
  AND (jobs.status != 'running' OR jobs.lease_until IS NULL OR jobs.lease_until <= excluded.started_at)
            "#,
        )
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(worker_id.to_string())
        .bind(token.as_str())
        .bind(now)
        .bind(lease_until)
        .bind(DEFAULT_RETRY_REMAINING)
        .execute(self.pool.as_ref())
        .await?;
        if result.rows_affected() == 0 {
            return Ok(Phase2JobClaimOutcome::SkippedRunning);
        }
        let watermark = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT input_watermark FROM jobs WHERE kind = ? AND job_key = ?",
        )
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .fetch_one(self.pool.as_ref())
        .await?
        .unwrap_or(0);
        Ok(Phase2JobClaimOutcome::Claimed {
            ownership_token: token,
            input_watermark: watermark,
        })
    }

    pub async fn heartbeat_global_phase2_job(
        &self,
        ownership_token: &str,
        lease_seconds: i64,
    ) -> anyhow::Result<bool> {
        let lease_until = Utc::now().timestamp().saturating_add(lease_seconds.max(0));
        let result = sqlx::query(
            "UPDATE jobs SET lease_until = ? WHERE kind = ? AND job_key = ? AND status = 'running' AND ownership_token = ?",
        )
        .bind(lease_until)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(ownership_token)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn mark_global_phase2_job_failed(
        &self,
        ownership_token: &str,
        reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let retry_at = now.saturating_add(retry_delay_seconds.max(0));
        let result = sqlx::query(
            r#"
UPDATE jobs
SET status = 'error',
    finished_at = ?,
    lease_until = NULL,
    retry_at = ?,
    retry_remaining = max(retry_remaining - 1, 0),
    last_error = ?
WHERE kind = ? AND job_key = ? AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(retry_at)
        .bind(reason)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(ownership_token)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn mark_global_phase2_job_failed_if_unowned(
        &self,
        ownership_token: &str,
        reason: &str,
        retry_delay_seconds: i64,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let retry_at = now.saturating_add(retry_delay_seconds.max(0));
        let result = sqlx::query(
            r#"
UPDATE jobs
SET status = 'error',
    finished_at = ?,
    lease_until = NULL,
    retry_at = ?,
    retry_remaining = max(retry_remaining - 1, 0),
    last_error = ?
WHERE kind = ? AND job_key = ? AND status = 'running'
  AND (ownership_token = ? OR ownership_token IS NULL)
            "#,
        )
        .bind(now)
        .bind(retry_at)
        .bind(reason)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(ownership_token)
        .execute(self.pool.as_ref())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn mark_global_phase2_job_succeeded(
        &self,
        ownership_token: &str,
        completion_watermark: i64,
        selected_outputs: &[Stage1Output],
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
UPDATE jobs
SET status = 'done',
    finished_at = ?,
    lease_until = NULL,
    retry_at = NULL,
    last_error = NULL,
    last_success_watermark = ?,
    input_watermark = max(COALESCE(input_watermark, 0), ?)
WHERE kind = ? AND job_key = ? AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(completion_watermark)
        .bind(completion_watermark)
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(ownership_token)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE stage1_outputs SET selected_for_phase2 = 0, selected_for_phase2_source_updated_at = NULL",
        )
        .execute(&mut *tx)
        .await?;
        for output in selected_outputs {
            sqlx::query(
                "UPDATE stage1_outputs SET selected_for_phase2 = 1, selected_for_phase2_source_updated_at = ? WHERE thread_id = ? AND source_updated_at = ?",
            )
            .bind(output.source_updated_at.timestamp())
            .bind(output.thread_id.to_string())
            .bind(output.source_updated_at.timestamp())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    pub async fn mark_thread_memory_mode_polluted(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<bool> {
        let selected_for_phase2 = sqlx::query_scalar::<_, i64>(
            "SELECT selected_for_phase2 FROM stage1_outputs WHERE thread_id = ?",
        )
        .bind(thread_id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?
        .unwrap_or(0);
        let result = sqlx::query(
            "UPDATE threads SET memory_mode = 'polluted' WHERE id = ? AND memory_mode != 'polluted'",
        )
        .bind(thread_id.to_string())
        .execute(self.state_pool.as_ref())
        .await?;
        if selected_for_phase2 != 0 {
            self.bump_phase2_input_watermark(Utc::now().timestamp())
                .await?;
        }
        Ok(result.rows_affected() > 0)
    }

    async fn bump_phase2_input_watermark(&self, watermark: i64) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        self.bump_phase2_input_watermark_tx(&mut tx, watermark)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn bump_phase2_input_watermark_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        watermark: i64,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO jobs (kind, job_key, status, retry_remaining, input_watermark, last_success_watermark)
VALUES (?, ?, 'pending', ?, ?, 0)
ON CONFLICT(kind, job_key) DO UPDATE SET
    status = CASE
        WHEN jobs.status = 'running' THEN 'running'
        ELSE 'pending'
    END,
    retry_at = CASE
        WHEN jobs.status = 'running' THEN jobs.retry_at
        ELSE NULL
    END,
    input_watermark = CASE
        WHEN excluded.input_watermark > COALESCE(jobs.input_watermark, 0)
            THEN excluded.input_watermark
        ELSE COALESCE(jobs.input_watermark, 0) + 1
    END,
    retry_remaining = max(jobs.retry_remaining, excluded.retry_remaining)
            "#,
        )
        .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
        .bind(MEMORY_CONSOLIDATION_JOB_KEY)
        .bind(DEFAULT_RETRY_REMAINING)
        .bind(watermark)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryStoreMode {
    Required,
    Disabled,
}

#[derive(Clone)]
pub struct StateRuntime {
    lha_home: PathBuf,
    default_provider: String,
    pool: Arc<sqlx::SqlitePool>,
    memories: Option<MemoryStore>,
}

impl StateRuntime {
    /// Initialize the state runtime using the provided LHA home and default provider.
    ///
    /// This opens (and migrates) the SQLite database at `lha_home/state.sqlite`.
    pub async fn init(
        lha_home: PathBuf,
        default_provider: String,
        otel: Option<OtelManager>,
    ) -> anyhow::Result<Arc<Self>> {
        Self::init_with_memory_store(lha_home, default_provider, otel, MemoryStoreMode::Required)
            .await
    }

    pub async fn init_with_memory_store(
        lha_home: PathBuf,
        default_provider: String,
        otel: Option<OtelManager>,
        memory_store_mode: MemoryStoreMode,
    ) -> anyhow::Result<Arc<Self>> {
        tokio::fs::create_dir_all(&lha_home).await?;
        let state_path = lha_home.join(STATE_DB_FILENAME);
        let memories_path = lha_home.join(MEMORIES_DB_FILENAME);
        let existed = tokio::fs::try_exists(&state_path).await.unwrap_or(false);
        let pool = match open_sqlite(&state_path).await {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!("failed to open state db at {}: {err}", state_path.display());
                if let Some(otel) = otel.as_ref() {
                    otel.counter(METRIC_DB_INIT, 1, &[("status", "open_error")]);
                }
                return Err(err);
            }
        };
        let memories = match memory_store_mode {
            MemoryStoreMode::Required => match open_memories_sqlite(&memories_path).await {
                Ok(db) => {
                    let memories_pool = Arc::new(db);
                    Some(MemoryStore::new(
                        Arc::clone(&memories_pool),
                        Arc::clone(&pool),
                    ))
                }
                Err(err) => {
                    warn!(
                        "failed to open memories db at {}: {err}",
                        memories_path.display()
                    );
                    if let Some(otel) = otel.as_ref() {
                        otel.counter(METRIC_DB_INIT, 1, &[("status", "memories_open_error")]);
                    }
                    return Err(err);
                }
            },
            MemoryStoreMode::Disabled => None,
        };
        if let Some(otel) = otel.as_ref() {
            otel.counter(METRIC_DB_INIT, 1, &[("status", "opened")]);
        }
        let runtime = Arc::new(Self {
            memories,
            pool,
            lha_home,
            default_provider,
        });
        if !existed && let Some(otel) = otel.as_ref() {
            otel.counter(METRIC_DB_INIT, 1, &[("status", "created")]);
        }
        Ok(runtime)
    }

    /// Return the configured LHA home directory for this runtime.
    pub fn lha_home(&self) -> &Path {
        self.lha_home.as_path()
    }

    pub fn memories(&self) -> Option<&MemoryStore> {
        self.memories.as_ref()
    }

    pub async fn get_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        let row = sqlx::query(
            r#"
SELECT
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
FROM thread_goals
WHERE thread_id = ?
            "#,
        )
        .bind(thread_id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?;

        row.map(|row| {
            ThreadGoalRow::try_from_row(&row).and_then(crate::product::state::ThreadGoal::try_from)
        })
        .transpose()
    }

    pub async fn replace_thread_goal(
        &self,
        thread_id: ThreadId,
        objective: &str,
        status: crate::product::state::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<crate::product::state::ThreadGoal> {
        let goal_id = Uuid::new_v4().to_string();
        let now_ms = datetime_to_epoch_millis(Utc::now());
        let status = status_after_budget_limit(status, 0, token_budget);
        let row = sqlx::query(
            r#"
INSERT INTO thread_goals (
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
) VALUES (?, ?, ?, ?, ?, 0, 0, ?, ?)
ON CONFLICT(thread_id) DO UPDATE SET
    goal_id = excluded.goal_id,
    objective = excluded.objective,
    status = excluded.status,
    token_budget = excluded.token_budget,
    tokens_used = 0,
    time_used_seconds = 0,
    created_at_ms = excluded.created_at_ms,
    updated_at_ms = excluded.updated_at_ms
RETURNING
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
            "#,
        )
        .bind(thread_id.to_string())
        .bind(goal_id)
        .bind(objective)
        .bind(status.as_str())
        .bind(token_budget)
        .bind(now_ms)
        .bind(now_ms)
        .fetch_one(self.pool.as_ref())
        .await?;

        ThreadGoalRow::try_from_row(&row).and_then(crate::product::state::ThreadGoal::try_from)
    }

    pub async fn replace_thread_goal_if_goal_id(
        &self,
        thread_id: ThreadId,
        expected_goal_id: &str,
        objective: &str,
        status: crate::product::state::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        self.replace_thread_goal_if_goal_id_excluding_status(
            thread_id,
            expected_goal_id,
            objective,
            status,
            token_budget,
            None,
        )
        .await
    }

    pub async fn replace_unfinished_thread_goal_if_goal_id(
        &self,
        thread_id: ThreadId,
        expected_goal_id: &str,
        objective: &str,
        status: crate::product::state::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        self.replace_thread_goal_if_goal_id_excluding_status(
            thread_id,
            expected_goal_id,
            objective,
            status,
            token_budget,
            Some(crate::product::state::ThreadGoalStatus::Complete),
        )
        .await
    }

    async fn replace_thread_goal_if_goal_id_excluding_status(
        &self,
        thread_id: ThreadId,
        expected_goal_id: &str,
        objective: &str,
        status: crate::product::state::ThreadGoalStatus,
        token_budget: Option<i64>,
        excluded_status: Option<crate::product::state::ThreadGoalStatus>,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        let goal_id = Uuid::new_v4().to_string();
        let now_ms = datetime_to_epoch_millis(Utc::now());
        let status = status_after_budget_limit(status, 0, token_budget);
        let excluded_status = excluded_status.map(crate::product::state::ThreadGoalStatus::as_str);
        let row = sqlx::query(
            r#"
UPDATE thread_goals
SET
    goal_id = ?,
    objective = ?,
    status = ?,
    token_budget = ?,
    tokens_used = 0,
    time_used_seconds = 0,
    created_at_ms = ?,
    updated_at_ms = ?
WHERE thread_id = ?
  AND goal_id = ?
  AND (? IS NULL OR status != ?)
RETURNING
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
            "#,
        )
        .bind(goal_id)
        .bind(objective)
        .bind(status.as_str())
        .bind(token_budget)
        .bind(now_ms)
        .bind(now_ms)
        .bind(thread_id.to_string())
        .bind(expected_goal_id)
        .bind(excluded_status)
        .bind(excluded_status)
        .fetch_optional(self.pool.as_ref())
        .await?;

        row.map(|row| {
            ThreadGoalRow::try_from_row(&row).and_then(crate::product::state::ThreadGoal::try_from)
        })
        .transpose()
    }

    pub async fn insert_thread_goal(
        &self,
        thread_id: ThreadId,
        objective: &str,
        status: crate::product::state::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        let goal_id = Uuid::new_v4().to_string();
        let now_ms = datetime_to_epoch_millis(Utc::now());
        let status = status_after_budget_limit(status, 0, token_budget);
        let row = sqlx::query(
            r#"
INSERT INTO thread_goals (
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
) VALUES (?, ?, ?, ?, ?, 0, 0, ?, ?)
ON CONFLICT(thread_id) DO NOTHING
RETURNING
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
            "#,
        )
        .bind(thread_id.to_string())
        .bind(goal_id)
        .bind(objective)
        .bind(status.as_str())
        .bind(token_budget)
        .bind(now_ms)
        .bind(now_ms)
        .fetch_optional(self.pool.as_ref())
        .await?;

        row.map(|row| {
            ThreadGoalRow::try_from_row(&row).and_then(crate::product::state::ThreadGoal::try_from)
        })
        .transpose()
    }

    pub async fn insert_thread_goal_or_replace_completed(
        &self,
        thread_id: ThreadId,
        objective: &str,
        status: crate::product::state::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        let goal_id = Uuid::new_v4().to_string();
        let now_ms = datetime_to_epoch_millis(Utc::now());
        let status = status_after_budget_limit(status, 0, token_budget);
        let row = sqlx::query(
            r#"
INSERT INTO thread_goals (
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
) VALUES (?, ?, ?, ?, ?, 0, 0, ?, ?)
ON CONFLICT(thread_id) DO UPDATE SET
    goal_id = excluded.goal_id,
    objective = excluded.objective,
    status = excluded.status,
    token_budget = excluded.token_budget,
    tokens_used = 0,
    time_used_seconds = 0,
    created_at_ms = excluded.created_at_ms,
    updated_at_ms = excluded.updated_at_ms
WHERE thread_goals.status = ?
RETURNING
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
            "#,
        )
        .bind(thread_id.to_string())
        .bind(goal_id)
        .bind(objective)
        .bind(status.as_str())
        .bind(token_budget)
        .bind(now_ms)
        .bind(now_ms)
        .bind(crate::product::state::ThreadGoalStatus::Complete.as_str())
        .fetch_optional(self.pool.as_ref())
        .await?;

        row.map(|row| {
            ThreadGoalRow::try_from_row(&row).and_then(crate::product::state::ThreadGoal::try_from)
        })
        .transpose()
    }

    pub async fn seed_thread_goal(
        &self,
        thread_id: ThreadId,
        seed: ThreadGoalSeed,
    ) -> anyhow::Result<crate::product::state::ThreadGoal> {
        let goal_id = Uuid::new_v4().to_string();
        let tokens_used = seed.tokens_used.max(0);
        let time_used_seconds = seed.time_used_seconds.max(0);
        let created_at_ms = datetime_to_epoch_millis(seed.created_at);
        let updated_at_ms = datetime_to_epoch_millis(seed.updated_at);
        let status = status_after_budget_limit(seed.status, tokens_used, seed.token_budget);
        let row = sqlx::query(
            r#"
INSERT INTO thread_goals (
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(thread_id) DO UPDATE SET
    goal_id = excluded.goal_id,
    objective = excluded.objective,
    status = excluded.status,
    token_budget = excluded.token_budget,
    tokens_used = excluded.tokens_used,
    time_used_seconds = excluded.time_used_seconds,
    created_at_ms = excluded.created_at_ms,
    updated_at_ms = excluded.updated_at_ms
RETURNING
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
            "#,
        )
        .bind(thread_id.to_string())
        .bind(goal_id)
        .bind(seed.objective)
        .bind(status.as_str())
        .bind(seed.token_budget)
        .bind(tokens_used)
        .bind(time_used_seconds)
        .bind(created_at_ms)
        .bind(updated_at_ms)
        .fetch_one(self.pool.as_ref())
        .await?;

        ThreadGoalRow::try_from_row(&row).and_then(crate::product::state::ThreadGoal::try_from)
    }

    pub async fn update_thread_goal(
        &self,
        thread_id: ThreadId,
        update: GoalUpdate,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        let GoalUpdate {
            objective,
            status,
            token_budget,
            expected_goal_id,
        } = update;
        let objective = objective.as_deref();
        let expected_goal_id = expected_goal_id.as_deref();
        let now_ms = datetime_to_epoch_millis(Utc::now());
        let result = match (status, token_budget) {
            (Some(status), Some(token_budget)) => {
                sqlx::query(
                    r#"
UPDATE thread_goals
SET
    objective = COALESCE(?, objective),
    status = CASE
        WHEN status = ? AND ? IN (?, ?) THEN status
        WHEN ? = 'active' AND ? IS NOT NULL AND tokens_used >= ? THEN ?
        ELSE ?
    END,
    token_budget = ?,
    updated_at_ms = ?
WHERE thread_id = ?
  AND (? IS NULL OR goal_id = ?)
            "#,
                )
                .bind(objective)
                .bind(crate::product::state::ThreadGoalStatus::BudgetLimited.as_str())
                .bind(status.as_str())
                .bind(crate::product::state::ThreadGoalStatus::Paused.as_str())
                .bind(crate::product::state::ThreadGoalStatus::Blocked.as_str())
                .bind(status.as_str())
                .bind(token_budget)
                .bind(token_budget)
                .bind(crate::product::state::ThreadGoalStatus::BudgetLimited.as_str())
                .bind(status.as_str())
                .bind(token_budget)
                .bind(now_ms)
                .bind(thread_id.to_string())
                .bind(expected_goal_id)
                .bind(expected_goal_id)
                .execute(self.pool.as_ref())
                .await?
            }
            (Some(status), None) => {
                sqlx::query(
                    r#"
UPDATE thread_goals
SET
    objective = COALESCE(?, objective),
    status = CASE
        WHEN status = ? AND ? IN (?, ?) THEN status
        WHEN ? = 'active' AND token_budget IS NOT NULL AND tokens_used >= token_budget THEN ?
        ELSE ?
    END,
    updated_at_ms = ?
WHERE thread_id = ?
  AND (? IS NULL OR goal_id = ?)
            "#,
                )
                .bind(objective)
                .bind(crate::product::state::ThreadGoalStatus::BudgetLimited.as_str())
                .bind(status.as_str())
                .bind(crate::product::state::ThreadGoalStatus::Paused.as_str())
                .bind(crate::product::state::ThreadGoalStatus::Blocked.as_str())
                .bind(status.as_str())
                .bind(crate::product::state::ThreadGoalStatus::BudgetLimited.as_str())
                .bind(status.as_str())
                .bind(now_ms)
                .bind(thread_id.to_string())
                .bind(expected_goal_id)
                .bind(expected_goal_id)
                .execute(self.pool.as_ref())
                .await?
            }
            (None, Some(token_budget)) => {
                sqlx::query(
                    r#"
UPDATE thread_goals
SET
    objective = COALESCE(?, objective),
    token_budget = ?,
    status = CASE
        WHEN status = 'active' AND ? IS NOT NULL AND tokens_used >= ? THEN ?
        ELSE status
    END,
    updated_at_ms = ?
WHERE thread_id = ?
  AND (? IS NULL OR goal_id = ?)
            "#,
                )
                .bind(objective)
                .bind(token_budget)
                .bind(token_budget)
                .bind(token_budget)
                .bind(crate::product::state::ThreadGoalStatus::BudgetLimited.as_str())
                .bind(now_ms)
                .bind(thread_id.to_string())
                .bind(expected_goal_id)
                .bind(expected_goal_id)
                .execute(self.pool.as_ref())
                .await?
            }
            (None, None) => {
                if let Some(objective) = objective {
                    sqlx::query(
                        r#"
UPDATE thread_goals
SET
    objective = ?,
    updated_at_ms = ?
WHERE thread_id = ?
  AND (? IS NULL OR goal_id = ?)
            "#,
                    )
                    .bind(objective)
                    .bind(now_ms)
                    .bind(thread_id.to_string())
                    .bind(expected_goal_id)
                    .bind(expected_goal_id)
                    .execute(self.pool.as_ref())
                    .await?
                } else {
                    let goal = self.get_thread_goal(thread_id).await?;
                    return Ok(match (goal, expected_goal_id) {
                        (Some(goal), Some(expected_goal_id))
                            if goal.goal_id != expected_goal_id =>
                        {
                            None
                        }
                        (goal, None) | (goal, Some(_)) => goal,
                    });
                }
            }
        };

        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_thread_goal(thread_id).await
    }

    pub async fn pause_active_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        self.pause_active_thread_goal_if_goal_id(thread_id, None)
            .await
    }

    pub async fn pause_active_thread_goal_if_goal_id(
        &self,
        thread_id: ThreadId,
        expected_goal_id: Option<&str>,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        self.update_active_thread_goal_status(
            thread_id,
            crate::product::state::ThreadGoalStatus::Paused,
            expected_goal_id,
        )
        .await
    }

    pub async fn usage_limit_active_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        self.update_active_thread_goal_status(
            thread_id,
            crate::product::state::ThreadGoalStatus::UsageLimited,
            None,
        )
        .await
    }

    async fn update_active_thread_goal_status(
        &self,
        thread_id: ThreadId,
        status: crate::product::state::ThreadGoalStatus,
        expected_goal_id: Option<&str>,
    ) -> anyhow::Result<Option<crate::product::state::ThreadGoal>> {
        let now_ms = datetime_to_epoch_millis(Utc::now());
        let result = sqlx::query(
            r#"
UPDATE thread_goals
SET
    status = ?,
    updated_at_ms = ?
WHERE thread_id = ?
  AND (
      status = 'active'
      OR (
          ? = 'usage_limited'
          AND status = 'budget_limited'
      )
  )
  AND (? IS NULL OR goal_id = ?)
            "#,
        )
        .bind(status.as_str())
        .bind(now_ms)
        .bind(thread_id.to_string())
        .bind(status.as_str())
        .bind(expected_goal_id)
        .bind(expected_goal_id)
        .execute(self.pool.as_ref())
        .await?;

        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_thread_goal(thread_id).await
    }

    pub async fn account_thread_goal_usage(
        &self,
        thread_id: ThreadId,
        time_delta_seconds: i64,
        token_delta: i64,
        mode: GoalAccountingMode,
        expected_goal_id: Option<&str>,
    ) -> anyhow::Result<GoalAccountingOutcome> {
        let time_delta_seconds = time_delta_seconds.max(0);
        let token_delta = token_delta.max(0);
        if time_delta_seconds == 0 && token_delta == 0 {
            return Ok(GoalAccountingOutcome::Unchanged(
                self.get_thread_goal(thread_id).await?,
            ));
        }

        let now_ms = datetime_to_epoch_millis(Utc::now());
        let status_filter = match mode {
            GoalAccountingMode::ActiveStatusOnly => "status = 'active'",
            GoalAccountingMode::ActiveOnly => "status IN ('active', 'budget_limited')",
            GoalAccountingMode::ActiveOrComplete => {
                "status IN ('active', 'budget_limited', 'complete')"
            }
            GoalAccountingMode::ActiveOrStopped => {
                "status IN ('active', 'paused', 'blocked', 'usage_limited', 'budget_limited')"
            }
        };
        let budget_limit_status_filter = match mode {
            GoalAccountingMode::ActiveStatusOnly
            | GoalAccountingMode::ActiveOnly
            | GoalAccountingMode::ActiveOrComplete => "status = 'active'",
            GoalAccountingMode::ActiveOrStopped => {
                "status IN ('active', 'paused', 'blocked', 'usage_limited', 'budget_limited')"
            }
        };
        let goal_id_filter = if expected_goal_id.is_some() {
            "goal_id = ?"
        } else {
            "1 = 1"
        };
        let query = format!(
            r#"
UPDATE thread_goals
SET
    time_used_seconds = time_used_seconds + ?,
    tokens_used = tokens_used + ?,
    status = CASE
        WHEN {budget_limit_status_filter} AND token_budget IS NOT NULL AND tokens_used + ? >= token_budget
            THEN ?
        ELSE status
    END,
    updated_at_ms = ?
WHERE thread_id = ?
  AND {status_filter}
  AND {goal_id_filter}
RETURNING
    thread_id,
    goal_id,
    objective,
    status,
    token_budget,
    tokens_used,
    time_used_seconds,
    created_at_ms,
    updated_at_ms
            "#,
        );

        let mut query = sqlx::query(&query)
            .bind(time_delta_seconds)
            .bind(token_delta)
            .bind(token_delta)
            .bind(crate::product::state::ThreadGoalStatus::BudgetLimited.as_str())
            .bind(now_ms)
            .bind(thread_id.to_string());
        if let Some(expected_goal_id) = expected_goal_id {
            query = query.bind(expected_goal_id);
        }

        let Some(row) = query.fetch_optional(self.pool.as_ref()).await? else {
            return Ok(GoalAccountingOutcome::Unchanged(
                self.get_thread_goal(thread_id).await?,
            ));
        };

        let updated = ThreadGoalRow::try_from_row(&row)
            .and_then(crate::product::state::ThreadGoal::try_from)?;
        Ok(GoalAccountingOutcome::Updated(updated))
    }

    pub async fn delete_thread_goal(&self, thread_id: ThreadId) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM thread_goals WHERE thread_id = ?")
            .bind(thread_id.to_string())
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Load thread metadata by id using the underlying database.
    pub async fn get_thread(
        &self,
        id: ThreadId,
    ) -> anyhow::Result<Option<crate::product::state::ThreadMetadata>> {
        let row = sqlx::query(
            r#"
SELECT
    id,
    rollout_path,
    created_at,
    updated_at,
    source,
    model_provider,
    cwd,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    has_user_event,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
FROM threads
WHERE id = ?
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(|row| ThreadRow::try_from_row(&row).and_then(ThreadMetadata::try_from))
            .transpose()
    }

    pub async fn get_thread_memory_mode(&self, id: ThreadId) -> anyhow::Result<Option<String>> {
        let row = sqlx::query("SELECT memory_mode FROM threads WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(self.pool.as_ref())
            .await?;
        Ok(row.and_then(|row| row.try_get("memory_mode").ok()))
    }

    pub async fn set_thread_memory_mode(
        &self,
        id: ThreadId,
        memory_mode: &str,
    ) -> anyhow::Result<bool> {
        let result = sqlx::query("UPDATE threads SET memory_mode = ? WHERE id = ?")
            .bind(memory_mode)
            .bind(id.to_string())
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_thread_memory_mode_from_rollout(
        &self,
        id: ThreadId,
        memory_mode: &str,
    ) -> anyhow::Result<bool> {
        let result =
            sqlx::query("UPDATE threads SET memory_mode = ? WHERE id = ? AND memory_mode != ?")
                .bind(memory_mode)
                .bind(id.to_string())
                .bind(crate::product::protocol::protocol::MEMORY_MODE_POLLUTED)
                .execute(self.pool.as_ref())
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Find a rollout path by thread id using the underlying database.
    pub async fn find_rollout_path_by_id(
        &self,
        id: ThreadId,
        archived_only: Option<bool>,
    ) -> anyhow::Result<Option<PathBuf>> {
        let mut builder =
            QueryBuilder::<Sqlite>::new("SELECT rollout_path FROM threads WHERE id = ");
        builder.push_bind(id.to_string());
        match archived_only {
            Some(true) => {
                builder.push(" AND archived = 1");
            }
            Some(false) => {
                builder.push(" AND archived = 0");
            }
            None => {}
        }
        let row = builder.build().fetch_optional(self.pool.as_ref()).await?;
        Ok(row
            .and_then(|r| r.try_get::<String, _>("rollout_path").ok())
            .map(PathBuf::from))
    }

    /// List threads using the underlying database.
    pub async fn list_threads(
        &self,
        page_size: usize,
        anchor: Option<&crate::product::state::Anchor>,
        sort_key: crate::product::state::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
    ) -> anyhow::Result<crate::product::state::ThreadsPage> {
        let limit = page_size.saturating_add(1);

        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
SELECT
    id,
    rollout_path,
    created_at,
    updated_at,
    source,
    model_provider,
    cwd,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    has_user_event,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
FROM threads
            "#,
        );
        push_thread_filters(
            &mut builder,
            archived_only,
            allowed_sources,
            model_providers,
            anchor,
            sort_key,
        );
        push_thread_order_and_limit(&mut builder, sort_key, limit);

        let rows = builder.build().fetch_all(self.pool.as_ref()).await?;
        let mut items = rows
            .into_iter()
            .map(|row| ThreadRow::try_from_row(&row).and_then(ThreadMetadata::try_from))
            .collect::<Result<Vec<_>, _>>()?;
        let num_scanned_rows = items.len();
        let next_anchor = if items.len() > page_size {
            items.pop();
            items
                .last()
                .and_then(|item| anchor_from_item(item, sort_key))
        } else {
            None
        };
        Ok(ThreadsPage {
            items,
            next_anchor,
            num_scanned_rows,
        })
    }

    /// Insert one log entry into the logs table.
    pub async fn insert_log(&self, entry: &LogEntry) -> anyhow::Result<()> {
        self.insert_logs(std::slice::from_ref(entry)).await
    }

    /// Insert a batch of log entries into the logs table.
    pub async fn insert_logs(&self, entries: &[LogEntry]) -> anyhow::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let mut builder = QueryBuilder::<Sqlite>::new(
            "INSERT INTO logs (ts, ts_nanos, level, target, message, thread_id, module_path, file, line) ",
        );
        builder.push_values(entries, |mut row, entry| {
            row.push_bind(entry.ts)
                .push_bind(entry.ts_nanos)
                .push_bind(&entry.level)
                .push_bind(&entry.target)
                .push_bind(&entry.message)
                .push_bind(&entry.thread_id)
                .push_bind(&entry.module_path)
                .push_bind(&entry.file)
                .push_bind(entry.line);
        });
        builder.build().execute(self.pool.as_ref()).await?;
        Ok(())
    }

    pub(crate) async fn delete_logs_before(&self, cutoff_ts: i64) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM logs WHERE ts < ?")
            .bind(cutoff_ts)
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected())
    }

    /// Query logs with optional filters.
    pub async fn query_logs(&self, query: &LogQuery) -> anyhow::Result<Vec<LogRow>> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, ts, ts_nanos, level, target, message, thread_id, file, line FROM logs WHERE 1 = 1",
        );
        push_log_filters(&mut builder, query);
        if query.descending {
            builder.push(" ORDER BY id DESC");
        } else {
            builder.push(" ORDER BY id ASC");
        }
        if let Some(limit) = query.limit {
            builder.push(" LIMIT ").push_bind(limit as i64);
        }

        let rows = builder
            .build_query_as::<LogRow>()
            .fetch_all(self.pool.as_ref())
            .await?;
        Ok(rows)
    }

    /// Return the max log id matching optional filters.
    pub async fn max_log_id(&self, query: &LogQuery) -> anyhow::Result<i64> {
        let mut builder =
            QueryBuilder::<Sqlite>::new("SELECT MAX(id) AS max_id FROM logs WHERE 1 = 1");
        push_log_filters(&mut builder, query);
        let row = builder.build().fetch_one(self.pool.as_ref()).await?;
        let max_id: Option<i64> = row.try_get("max_id")?;
        Ok(max_id.unwrap_or(0))
    }

    /// List thread ids using the underlying database (no rollout scanning).
    pub async fn list_thread_ids(
        &self,
        limit: usize,
        anchor: Option<&crate::product::state::Anchor>,
        sort_key: crate::product::state::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
    ) -> anyhow::Result<Vec<ThreadId>> {
        let mut builder = QueryBuilder::<Sqlite>::new("SELECT id FROM threads");
        push_thread_filters(
            &mut builder,
            archived_only,
            allowed_sources,
            model_providers,
            anchor,
            sort_key,
        );
        push_thread_order_and_limit(&mut builder, sort_key, limit);

        let rows = builder.build().fetch_all(self.pool.as_ref()).await?;
        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                Ok(ThreadId::try_from(id)?)
            })
            .collect()
    }

    /// Insert or replace thread metadata directly.
    pub async fn upsert_thread(
        &self,
        metadata: &crate::product::state::ThreadMetadata,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO threads (
    id,
    rollout_path,
    created_at,
    updated_at,
    source,
    model_provider,
    cwd,
    title,
    sandbox_policy,
    approval_mode,
    tokens_used,
    has_user_event,
    archived,
    archived_at,
    git_sha,
    git_branch,
    git_origin_url,
    memory_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO UPDATE SET
    rollout_path = excluded.rollout_path,
    created_at = excluded.created_at,
    updated_at = excluded.updated_at,
    source = excluded.source,
    model_provider = excluded.model_provider,
    cwd = excluded.cwd,
    title = excluded.title,
    sandbox_policy = excluded.sandbox_policy,
    approval_mode = excluded.approval_mode,
    tokens_used = excluded.tokens_used,
    has_user_event = excluded.has_user_event,
    archived = excluded.archived,
    archived_at = excluded.archived_at,
    git_sha = excluded.git_sha,
    git_branch = excluded.git_branch,
    git_origin_url = excluded.git_origin_url
            "#,
        )
        .bind(metadata.id.to_string())
        .bind(metadata.rollout_path.display().to_string())
        .bind(datetime_to_epoch_seconds(metadata.created_at))
        .bind(datetime_to_epoch_seconds(metadata.updated_at))
        .bind(metadata.source.as_str())
        .bind(metadata.model_provider.as_str())
        .bind(metadata.cwd.display().to_string())
        .bind(metadata.title.as_str())
        .bind(metadata.sandbox_policy.as_str())
        .bind(metadata.approval_mode.as_str())
        .bind(metadata.tokens_used)
        .bind(metadata.has_user_event)
        .bind(metadata.archived_at.is_some())
        .bind(metadata.archived_at.map(datetime_to_epoch_seconds))
        .bind(metadata.git_sha.as_deref())
        .bind(metadata.git_branch.as_deref())
        .bind(metadata.git_origin_url.as_deref())
        .bind(metadata.memory_mode.as_str())
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Apply rollout items incrementally using the underlying database.
    pub async fn apply_rollout_items(
        &self,
        builder: &ThreadMetadataBuilder,
        items: &[RolloutItem],
        otel: Option<&OtelManager>,
    ) -> anyhow::Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let mut metadata = self
            .get_thread(builder.id)
            .await?
            .unwrap_or_else(|| builder.build(&self.default_provider));
        metadata.rollout_path = builder.rollout_path.clone();
        for item in items {
            apply_rollout_item(&mut metadata, item, &self.default_provider);
        }
        if let Some(updated_at) = file_modified_time_utc(builder.rollout_path.as_path()).await {
            metadata.updated_at = updated_at;
        }
        if let Err(err) = self.upsert_thread(&metadata).await {
            if let Some(otel) = otel {
                otel.counter(DB_ERROR_METRIC, 1, &[("stage", "apply_rollout_items")]);
            }
            return Err(err);
        }
        if let Some(memory_mode) = extract_memory_mode(items) {
            self.set_thread_memory_mode_from_rollout(builder.id, memory_mode.as_str())
                .await?;
        }
        Ok(())
    }

    /// Mark a thread as archived using the underlying database.
    pub async fn mark_archived(
        &self,
        thread_id: ThreadId,
        rollout_path: &Path,
        archived_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let Some(mut metadata) = self.get_thread(thread_id).await? else {
            return Ok(());
        };
        metadata.archived_at = Some(archived_at);
        metadata.rollout_path = rollout_path.to_path_buf();
        if let Some(updated_at) = file_modified_time_utc(rollout_path).await {
            metadata.updated_at = updated_at;
        }
        if metadata.id != thread_id {
            warn!(
                "thread id mismatch during archive: expected {thread_id}, got {}",
                metadata.id
            );
        }
        self.upsert_thread(&metadata).await
    }

    /// Mark a thread as unarchived using the underlying database.
    pub async fn mark_unarchived(
        &self,
        thread_id: ThreadId,
        rollout_path: &Path,
    ) -> anyhow::Result<()> {
        let Some(mut metadata) = self.get_thread(thread_id).await? else {
            return Ok(());
        };
        metadata.archived_at = None;
        metadata.rollout_path = rollout_path.to_path_buf();
        if let Some(updated_at) = file_modified_time_utc(rollout_path).await {
            metadata.updated_at = updated_at;
        }
        if metadata.id != thread_id {
            warn!(
                "thread id mismatch during unarchive: expected {thread_id}, got {}",
                metadata.id
            );
        }
        self.upsert_thread(&metadata).await
    }
}

fn push_log_filters<'a>(builder: &mut QueryBuilder<'a, Sqlite>, query: &'a LogQuery) {
    if let Some(level_upper) = query.level_upper.as_ref() {
        builder
            .push(" AND UPPER(level) = ")
            .push_bind(level_upper.as_str());
    }
    if let Some(from_ts) = query.from_ts {
        builder.push(" AND ts >= ").push_bind(from_ts);
    }
    if let Some(to_ts) = query.to_ts {
        builder.push(" AND ts <= ").push_bind(to_ts);
    }
    push_like_filters(builder, "module_path", &query.module_like);
    push_like_filters(builder, "file", &query.file_like);
    let has_thread_filter = !query.thread_ids.is_empty() || query.include_threadless;
    if has_thread_filter {
        builder.push(" AND (");
        let mut needs_or = false;
        for thread_id in &query.thread_ids {
            if needs_or {
                builder.push(" OR ");
            }
            builder.push("thread_id = ").push_bind(thread_id.as_str());
            needs_or = true;
        }
        if query.include_threadless {
            if needs_or {
                builder.push(" OR ");
            }
            builder.push("thread_id IS NULL");
        }
        builder.push(")");
    }
    if let Some(after_id) = query.after_id {
        builder.push(" AND id > ").push_bind(after_id);
    }
}

fn push_like_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    column: &str,
    filters: &'a [String],
) {
    if filters.is_empty() {
        return;
    }
    builder.push(" AND (");
    for (idx, filter) in filters.iter().enumerate() {
        if idx > 0 {
            builder.push(" OR ");
        }
        builder
            .push(column)
            .push(" LIKE '%' || ")
            .push_bind(filter.as_str())
            .push(" || '%'");
    }
    builder.push(")");
}

async fn open_sqlite(path: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

async fn open_memories_sqlite(path: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    MEMORIES_MIGRATOR.run(&pool).await?;
    Ok(pool)
}

fn stage1_output_from_row_and_thread(
    row: sqlx::sqlite::SqliteRow,
    thread: ThreadMetadata,
) -> anyhow::Result<Stage1Output> {
    let source_updated_at: i64 = row.try_get("source_updated_at")?;
    let generated_at: i64 = row.try_get("generated_at")?;
    Ok(Stage1Output {
        thread_id: thread.id,
        rollout_path: thread.rollout_path,
        source_updated_at: epoch_seconds_to_datetime(source_updated_at)?,
        raw_memory: row.try_get("raw_memory")?,
        rollout_summary: row.try_get("rollout_summary")?,
        rollout_slug: row.try_get("rollout_slug")?,
        cwd: thread.cwd,
        git_branch: thread.git_branch,
        generated_at: epoch_seconds_to_datetime(generated_at)?,
    })
}

fn extract_memory_mode(items: &[RolloutItem]) -> Option<String> {
    items.iter().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => meta_line.meta.memory_mode.clone(),
        RolloutItem::TranscriptItem(_)
        | RolloutItem::GhostSnapshot(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::InputSlimmingStoredInput(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::Workflow(_)
        | RolloutItem::EventMsg(_) => None,
    })
}

fn status_after_budget_limit(
    status: crate::product::state::ThreadGoalStatus,
    tokens_used: i64,
    token_budget: Option<i64>,
) -> crate::product::state::ThreadGoalStatus {
    if status == crate::product::state::ThreadGoalStatus::Active
        && token_budget.is_some_and(|budget| tokens_used >= budget)
    {
        crate::product::state::ThreadGoalStatus::BudgetLimited
    } else {
        status
    }
}

fn push_thread_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    archived_only: bool,
    allowed_sources: &'a [String],
    model_providers: Option<&'a [String]>,
    anchor: Option<&crate::product::state::Anchor>,
    sort_key: SortKey,
) {
    builder.push(" WHERE 1 = 1");
    if archived_only {
        builder.push(" AND archived = 1");
    } else {
        builder.push(" AND archived = 0");
    }
    builder.push(" AND has_user_event = 1");
    if !allowed_sources.is_empty() {
        builder.push(" AND source IN (");
        let mut separated = builder.separated(", ");
        for source in allowed_sources {
            separated.push_bind(source);
        }
        separated.push_unseparated(")");
    }
    if let Some(model_providers) = model_providers
        && !model_providers.is_empty()
    {
        builder.push(" AND model_provider IN (");
        let mut separated = builder.separated(", ");
        for provider in model_providers {
            separated.push_bind(provider);
        }
        separated.push_unseparated(")");
    }
    if let Some(anchor) = anchor {
        let anchor_ts = datetime_to_epoch_seconds(anchor.ts);
        let column = match sort_key {
            SortKey::CreatedAt => "created_at",
            SortKey::UpdatedAt => "updated_at",
        };
        builder.push(" AND (");
        builder.push(column);
        builder.push(" < ");
        builder.push_bind(anchor_ts);
        builder.push(" OR (");
        builder.push(column);
        builder.push(" = ");
        builder.push_bind(anchor_ts);
        builder.push(" AND id < ");
        builder.push_bind(anchor.id.to_string());
        builder.push("))");
    }
}

fn push_thread_order_and_limit(
    builder: &mut QueryBuilder<'_, Sqlite>,
    sort_key: SortKey,
    limit: usize,
) {
    let order_column = match sort_key {
        SortKey::CreatedAt => "created_at",
        SortKey::UpdatedAt => "updated_at",
    };
    builder.push(" ORDER BY ");
    builder.push(order_column);
    builder.push(" DESC, id DESC");
    builder.push(" LIMIT ");
    builder.push_bind(limit as i64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    async fn test_runtime() -> Arc<StateRuntime> {
        let path = std::env::temp_dir().join(format!("lha-state-test-{}", Uuid::new_v4()));
        StateRuntime::init(path, "test-provider".to_string(), None)
            .await
            .expect("state runtime should initialize")
    }

    fn test_thread_id() -> ThreadId {
        ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id")
    }

    fn other_thread_id() -> ThreadId {
        ThreadId::from_string("00000000-0000-0000-0000-000000000124").expect("valid thread id")
    }

    fn thread_id(n: u128) -> ThreadId {
        ThreadId::from_string(&Uuid::from_u128(n).to_string()).expect("valid thread id")
    }

    fn memory_thread(
        id: ThreadId,
        source: &str,
        updated_at: DateTime<Utc>,
        memory_mode: &str,
        has_user_event: bool,
    ) -> ThreadMetadata {
        ThreadMetadata {
            id,
            rollout_path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            created_at: updated_at - chrono::Duration::hours(1),
            updated_at,
            source: source.to_string(),
            model_provider: "test-provider".to_string(),
            cwd: PathBuf::from("/tmp/project"),
            title: format!("thread {id}"),
            sandbox_policy: "read-only".to_string(),
            approval_mode: "on-request".to_string(),
            tokens_used: 0,
            has_user_event,
            archived_at: None,
            git_sha: None,
            git_branch: Some("main".to_string()),
            git_origin_url: None,
            memory_mode: memory_mode.to_string(),
        }
    }

    fn session_meta_item(
        id: ThreadId,
        created_at: DateTime<Utc>,
        cwd: PathBuf,
        memory_mode: &str,
    ) -> RolloutItem {
        RolloutItem::SessionMeta(crate::product::protocol::protocol::SessionMetaLine {
            meta: crate::product::protocol::protocol::SessionMeta {
                id,
                forked_from_id: None,
                timestamp: created_at.to_rfc3339(),
                cwd,
                originator: "test".to_string(),
                cli_version: "test".to_string(),
                rollout_schema_version:
                    crate::product::protocol::protocol::ROLLOUT_SCHEMA_VERSION_V3,
                source: crate::product::protocol::protocol::SessionSource::Cli,
                model_provider: Some("test-provider".to_string()),
                base_instructions: None,
                dynamic_tools: None,
                memory_mode: Some(memory_mode.to_string()),
            },
            git: None,
        })
    }

    async fn insert_memory_output(
        runtime: &StateRuntime,
        thread: ThreadMetadata,
        source_updated_at: DateTime<Utc>,
        usage_count: i64,
        last_usage: Option<DateTime<Utc>>,
        selected_for_phase2: bool,
    ) {
        runtime.upsert_thread(&thread).await.expect("upsert thread");
        sqlx::query(
            r#"
INSERT INTO stage1_outputs (
    thread_id, source_updated_at, raw_memory, rollout_summary, rollout_slug, generated_at,
    usage_count, last_usage, selected_for_phase2, selected_for_phase2_source_updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(thread.id.to_string())
        .bind(datetime_to_epoch_seconds(source_updated_at))
        .bind(format!("raw {}", thread.id))
        .bind(format!("summary {}", thread.id))
        .bind(None::<String>)
        .bind(datetime_to_epoch_seconds(source_updated_at))
        .bind(usage_count)
        .bind(last_usage.map(datetime_to_epoch_seconds))
        .bind(selected_for_phase2)
        .bind(selected_for_phase2.then_some(datetime_to_epoch_seconds(source_updated_at)))
        .execute(runtime.memories().expect("memories").pool.as_ref())
        .await
        .expect("insert stage1 output");
    }

    fn memory_claim_params<'a>(allowed_sources: &'a [String]) -> Stage1StartupClaimParams<'a> {
        Stage1StartupClaimParams {
            scan_limit: 100,
            max_claimed: 10,
            max_age_days: 10,
            min_rollout_idle_hours: 6,
            allowed_sources,
            lease_seconds: 3_600,
        }
    }

    #[tokio::test]
    async fn memories_migrations_create_tables_and_memory_mode_column() {
        let runtime = test_runtime().await;

        let stage1_table: String = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'stage1_outputs'",
        )
        .fetch_one(runtime.memories().expect("memories").pool.as_ref())
        .await
        .expect("stage1 table");
        let jobs_table: String = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'jobs'",
        )
        .fetch_one(runtime.memories().expect("memories").pool.as_ref())
        .await
        .expect("jobs table");
        let memory_mode_column: String = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('threads') WHERE name = 'memory_mode'",
        )
        .fetch_one(runtime.pool.as_ref())
        .await
        .expect("memory_mode column");

        assert_eq!(stage1_table, "stage1_outputs");
        assert_eq!(jobs_table, "jobs");
        assert_eq!(memory_mode_column, "memory_mode");
    }

    #[tokio::test]
    async fn state_runtime_without_memories_ignores_corrupt_memory_db() {
        let path = std::env::temp_dir().join(format!("lha-state-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&path).await.expect("mkdir");
        tokio::fs::write(path.join(MEMORIES_DB_FILENAME), "not sqlite")
            .await
            .expect("write corrupt memories db");

        let runtime = StateRuntime::init_with_memory_store(
            path,
            "test-provider".to_string(),
            None,
            MemoryStoreMode::Disabled,
        )
        .await
        .expect("state runtime should initialize without memories");

        assert!(runtime.memories().is_none());
    }

    #[tokio::test]
    async fn state_runtime_requires_memories_when_mode_required() {
        let path = std::env::temp_dir().join(format!("lha-state-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&path).await.expect("mkdir");
        tokio::fs::write(path.join(MEMORIES_DB_FILENAME), "not sqlite")
            .await
            .expect("write corrupt memories db");

        let result = StateRuntime::init_with_memory_store(
            path,
            "test-provider".to_string(),
            None,
            MemoryStoreMode::Required,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn thread_upsert_preserves_existing_memory_mode() {
        let runtime = test_runtime().await;
        let id = test_thread_id();
        let updated_at = Utc::now() - chrono::Duration::hours(8);
        let mut metadata = memory_thread(id, "cli", updated_at, "disabled", true);
        runtime
            .upsert_thread(&metadata)
            .await
            .expect("insert thread");

        metadata.title = "updated".to_string();
        metadata.memory_mode = "enabled".to_string();
        runtime
            .upsert_thread(&metadata)
            .await
            .expect("refresh thread");

        assert_eq!(
            runtime
                .get_thread_memory_mode(id)
                .await
                .expect("memory mode"),
            Some("disabled".to_string())
        );
    }

    #[tokio::test]
    async fn rollout_disabled_memory_mode_remains_disabled_after_apply() {
        let runtime = test_runtime().await;
        let id = test_thread_id();
        let created_at = Utc::now() - chrono::Duration::hours(1);
        let rollout_path = runtime
            .lha_home()
            .join("sessions/2026/06/01/rollout-disabled.jsonl");
        let builder = ThreadMetadataBuilder::new(
            id,
            rollout_path.clone(),
            created_at,
            crate::product::protocol::protocol::SessionSource::Cli,
        );
        let items = vec![session_meta_item(
            id,
            created_at,
            runtime.lha_home().to_path_buf(),
            crate::product::protocol::protocol::MEMORY_MODE_DISABLED,
        )];

        runtime
            .apply_rollout_items(&builder, &items, None)
            .await
            .expect("apply rollout items");

        assert_eq!(
            runtime
                .get_thread_memory_mode(id)
                .await
                .expect("memory mode"),
            Some(crate::product::protocol::protocol::MEMORY_MODE_DISABLED.to_string())
        );
    }

    #[tokio::test]
    async fn rollout_apply_updates_existing_non_polluted_memory_mode() {
        let runtime = test_runtime().await;
        let id = test_thread_id();
        let created_at = Utc::now() - chrono::Duration::hours(1);
        let rollout_path = runtime
            .lha_home()
            .join("sessions/2026/06/01/rollout-existing-disabled.jsonl");
        let builder = ThreadMetadataBuilder::new(
            id,
            rollout_path.clone(),
            created_at,
            crate::product::protocol::protocol::SessionSource::Cli,
        );
        runtime
            .upsert_thread(&memory_thread(
                id,
                "cli",
                created_at,
                crate::product::protocol::protocol::MEMORY_MODE_ENABLED,
                true,
            ))
            .await
            .expect("insert thread");
        let items = vec![session_meta_item(
            id,
            created_at,
            runtime.lha_home().to_path_buf(),
            crate::product::protocol::protocol::MEMORY_MODE_DISABLED,
        )];

        runtime
            .apply_rollout_items(&builder, &items, None)
            .await
            .expect("apply rollout items");

        assert_eq!(
            runtime
                .get_thread_memory_mode(id)
                .await
                .expect("memory mode"),
            Some(crate::product::protocol::protocol::MEMORY_MODE_DISABLED.to_string())
        );
    }

    #[tokio::test]
    async fn rollout_apply_preserves_polluted_memory_mode() {
        let runtime = test_runtime().await;
        let id = test_thread_id();
        let created_at = Utc::now() - chrono::Duration::hours(1);
        let rollout_path = runtime
            .lha_home()
            .join("sessions/2026/06/01/rollout-polluted.jsonl");
        let builder = ThreadMetadataBuilder::new(
            id,
            rollout_path.clone(),
            created_at,
            crate::product::protocol::protocol::SessionSource::Cli,
        );
        runtime
            .upsert_thread(&memory_thread(
                id,
                "cli",
                created_at,
                crate::product::protocol::protocol::MEMORY_MODE_ENABLED,
                true,
            ))
            .await
            .expect("insert thread");
        runtime
            .memories()
            .expect("memories")
            .mark_thread_memory_mode_polluted(id)
            .await
            .expect("mark polluted");
        let items = vec![session_meta_item(
            id,
            created_at,
            runtime.lha_home().to_path_buf(),
            crate::product::protocol::protocol::MEMORY_MODE_ENABLED,
        )];

        runtime
            .apply_rollout_items(&builder, &items, None)
            .await
            .expect("apply rollout items");

        assert_eq!(
            runtime
                .get_thread_memory_mode(id)
                .await
                .expect("memory mode"),
            Some(crate::product::protocol::protocol::MEMORY_MODE_POLLUTED.to_string())
        );
    }

    #[tokio::test]
    async fn stage1_claim_skips_ineligible_threads() {
        let runtime = test_runtime().await;
        let now = Utc::now();
        let eligible = thread_id(1);
        let cases = [
            (
                eligible,
                "cli",
                now - chrono::Duration::hours(8),
                "enabled",
                true,
            ),
            (
                thread_id(2),
                "cli",
                now - chrono::Duration::hours(8),
                "disabled",
                true,
            ),
            (
                thread_id(3),
                "cli",
                now - chrono::Duration::hours(8),
                "polluted",
                true,
            ),
            (
                thread_id(4),
                "cli",
                now - chrono::Duration::hours(1),
                "enabled",
                true,
            ),
            (
                thread_id(5),
                "cli",
                now - chrono::Duration::days(20),
                "enabled",
                true,
            ),
            (
                thread_id(6),
                "exec",
                now - chrono::Duration::hours(8),
                "enabled",
                true,
            ),
            (
                thread_id(7),
                "cli",
                now - chrono::Duration::hours(8),
                "enabled",
                false,
            ),
            (
                thread_id(8),
                "vscode",
                now - chrono::Duration::hours(8),
                "enabled",
                true,
            ),
            (
                thread_id(9),
                "cli",
                now - chrono::Duration::hours(8),
                "enabled",
                true,
            ),
        ];
        for (id, source, updated_at, memory_mode, has_user_event) in cases {
            runtime
                .upsert_thread(&memory_thread(
                    id,
                    source,
                    updated_at,
                    memory_mode,
                    has_user_event,
                ))
                .await
                .expect("upsert thread");
        }
        let allowed = vec!["cli".to_string()];

        let claims = runtime
            .memories()
            .expect("memories")
            .claim_stage1_jobs_for_startup(thread_id(9), memory_claim_params(&allowed))
            .await
            .expect("claims");

        assert_eq!(
            claims
                .into_iter()
                .map(|claim| claim.thread.id)
                .collect::<Vec<_>>(),
            vec![eligible]
        );
    }

    #[tokio::test]
    async fn stage1_jobs_lease_retry_and_exhaust() {
        let runtime = test_runtime().await;
        let id = test_thread_id();
        let now = Utc::now();
        runtime
            .upsert_thread(&memory_thread(
                id,
                "cli",
                now - chrono::Duration::hours(8),
                "enabled",
                true,
            ))
            .await
            .expect("upsert thread");
        let allowed = vec!["cli".to_string()];

        let claims = runtime
            .memories()
            .expect("memories")
            .claim_stage1_jobs_for_startup(thread_id(99), memory_claim_params(&allowed))
            .await
            .expect("claims");
        assert_eq!(claims.len(), 1);
        let mut ownership_token = claims[0].ownership_token.clone();
        let second_claims = runtime
            .memories()
            .expect("memories")
            .claim_stage1_jobs_for_startup(thread_id(98), memory_claim_params(&allowed))
            .await
            .expect("second claims");
        assert!(second_claims.is_empty());

        for _ in 0..3 {
            runtime
                .memories()
                .expect("memories")
                .mark_stage1_job_failed(id, &ownership_token, "boom", 3_600)
                .await
                .expect("mark failed");
            sqlx::query(
                "UPDATE jobs SET retry_at = 0, status = 'error', lease_until = NULL WHERE kind = ? AND job_key = ?",
            )
            .bind(JOB_KIND_MEMORY_STAGE1)
            .bind(id.to_string())
            .execute(runtime.memories().expect("memories").pool.as_ref())
            .await
            .expect("reset retry");
            let row =
                sqlx::query("SELECT retry_remaining FROM jobs WHERE kind = ? AND job_key = ?")
                    .bind(JOB_KIND_MEMORY_STAGE1)
                    .bind(id.to_string())
                    .fetch_one(runtime.memories().expect("memories").pool.as_ref())
                    .await
                    .expect("job row");
            let retry_remaining: i64 = row.try_get("retry_remaining").expect("retry remaining");
            if retry_remaining > 0 {
                let claims = runtime
                    .memories()
                    .expect("memories")
                    .claim_stage1_jobs_for_startup(thread_id(97), memory_claim_params(&allowed))
                    .await
                    .expect("retry claims");
                assert_eq!(claims.len(), 1);
                ownership_token = claims[0].ownership_token.clone();
            }
        }

        let exhausted = runtime
            .memories()
            .expect("memories")
            .claim_stage1_jobs_for_startup(thread_id(96), memory_claim_params(&allowed))
            .await
            .expect("exhausted claims");
        assert!(exhausted.is_empty());
    }

    #[tokio::test]
    async fn phase2_global_lock_and_cooldown() {
        let runtime = test_runtime().await;
        let first = runtime
            .memories()
            .expect("memories")
            .try_claim_global_phase2_job(test_thread_id(), 3_600)
            .await
            .expect("first claim");
        let Phase2JobClaimOutcome::Claimed {
            ownership_token, ..
        } = first
        else {
            panic!("expected claim");
        };
        let second = runtime
            .memories()
            .expect("memories")
            .try_claim_global_phase2_job(other_thread_id(), 3_600)
            .await
            .expect("second claim");
        assert_eq!(second, Phase2JobClaimOutcome::SkippedRunning);

        assert!(
            runtime
                .memories()
                .expect("memories")
                .mark_global_phase2_job_succeeded(&ownership_token, 1, &[])
                .await
                .expect("succeed")
        );

        let cooldown = runtime
            .memories()
            .expect("memories")
            .try_claim_global_phase2_job(other_thread_id(), 3_600)
            .await
            .expect("cooldown claim");
        assert_eq!(cooldown, Phase2JobClaimOutcome::SkippedCooldown);
    }

    #[tokio::test]
    async fn phase2_selection_usage_recording_and_pollution() {
        let runtime = test_runtime().await;
        let now = Utc::now();
        let old_high_usage = memory_thread(
            thread_id(1),
            "cli",
            now - chrono::Duration::days(2),
            "enabled",
            true,
        );
        let recent_low_usage = memory_thread(
            thread_id(2),
            "cli",
            now - chrono::Duration::hours(8),
            "enabled",
            true,
        );
        let selected_polluted = memory_thread(
            thread_id(3),
            "cli",
            now - chrono::Duration::hours(9),
            "enabled",
            true,
        );
        insert_memory_output(
            runtime.as_ref(),
            old_high_usage.clone(),
            old_high_usage.updated_at,
            5,
            Some(now - chrono::Duration::hours(1)),
            false,
        )
        .await;
        insert_memory_output(
            runtime.as_ref(),
            recent_low_usage.clone(),
            recent_low_usage.updated_at,
            0,
            None,
            false,
        )
        .await;
        insert_memory_output(
            runtime.as_ref(),
            selected_polluted.clone(),
            selected_polluted.updated_at,
            0,
            None,
            true,
        )
        .await;

        let selected = runtime
            .memories()
            .expect("memories")
            .get_phase2_input_selection(2, 30)
            .await
            .expect("selection");
        assert_eq!(
            selected
                .iter()
                .map(|output| output.thread_id)
                .collect::<Vec<_>>(),
            vec![old_high_usage.id, recent_low_usage.id]
        );

        assert_eq!(
            runtime
                .memories()
                .expect("memories")
                .record_stage1_output_usage(&[recent_low_usage.id])
                .await
                .expect("usage"),
            1
        );
        let usage_count: i64 =
            sqlx::query_scalar("SELECT usage_count FROM stage1_outputs WHERE thread_id = ?")
                .bind(recent_low_usage.id.to_string())
                .fetch_one(runtime.memories().expect("memories").pool.as_ref())
                .await
                .expect("usage count");
        assert_eq!(usage_count, 1);

        assert!(
            runtime
                .memories()
                .expect("memories")
                .mark_thread_memory_mode_polluted(selected_polluted.id)
                .await
                .expect("polluted")
        );
        assert_eq!(
            runtime
                .get_thread_memory_mode(selected_polluted.id)
                .await
                .expect("memory mode"),
            Some("polluted".to_string())
        );
        let watermark: Option<i64> =
            sqlx::query_scalar("SELECT input_watermark FROM jobs WHERE kind = ? AND job_key = ?")
                .bind(JOB_KIND_MEMORY_CONSOLIDATE_GLOBAL)
                .bind(MEMORY_CONSOLIDATION_JOB_KEY)
                .fetch_one(runtime.memories().expect("memories").pool.as_ref())
                .await
                .expect("watermark");
        assert!(watermark.is_some_and(|value| value > 0));
    }

    #[tokio::test]
    async fn replace_update_insert_and_delete_thread_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();

        let goal = runtime
            .replace_thread_goal(
                thread_id,
                "optimize the benchmark",
                crate::product::state::ThreadGoalStatus::Active,
                Some(100_000),
            )
            .await
            .expect("goal replacement should succeed");
        assert_eq!(
            Some(goal.clone()),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );

        let duplicate = runtime
            .insert_thread_goal(
                thread_id,
                "replace the benchmark",
                crate::product::state::ThreadGoalStatus::Active,
                Some(200_000),
            )
            .await
            .expect("duplicate insert should not fail");
        assert_eq!(None, duplicate);

        let updated = runtime
            .update_thread_goal(
                thread_id,
                GoalUpdate {
                    objective: Some("optimize the benchmark carefully".to_string()),
                    status: Some(crate::product::state::ThreadGoalStatus::Paused),
                    token_budget: Some(Some(200_000)),
                    expected_goal_id: Some(goal.goal_id.clone()),
                },
            )
            .await
            .expect("goal update should succeed")
            .expect("goal should exist");
        assert_eq!(
            crate::product::state::ThreadGoal {
                objective: "optimize the benchmark carefully".to_string(),
                status: crate::product::state::ThreadGoalStatus::Paused,
                token_budget: Some(200_000),
                updated_at: updated.updated_at,
                ..goal
            },
            updated
        );

        assert!(
            runtime
                .delete_thread_goal(thread_id)
                .await
                .expect("goal delete should succeed")
        );
        assert_eq!(
            None,
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn insert_thread_goal_or_replace_completed_blocks_unfinished_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();

        let original = runtime
            .replace_thread_goal(
                thread_id,
                "finish current work",
                crate::product::state::ThreadGoalStatus::Active,
                Some(100_000),
            )
            .await
            .expect("goal replacement should succeed");

        let replacement = runtime
            .insert_thread_goal_or_replace_completed(
                thread_id,
                "start new work",
                crate::product::state::ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("conditional insert should not fail");

        assert_eq!(None, replacement);
        assert_eq!(
            Some(original),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn insert_thread_goal_or_replace_completed_replaces_completed_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();

        let original = runtime
            .replace_thread_goal(
                thread_id,
                "finish current work",
                crate::product::state::ThreadGoalStatus::Active,
                Some(100_000),
            )
            .await
            .expect("goal replacement should succeed");
        runtime
            .update_thread_goal(
                thread_id,
                GoalUpdate {
                    objective: None,
                    status: Some(crate::product::state::ThreadGoalStatus::Complete),
                    token_budget: None,
                    expected_goal_id: Some(original.goal_id.clone()),
                },
            )
            .await
            .expect("goal update should succeed")
            .expect("goal should exist");

        let replacement = runtime
            .insert_thread_goal_or_replace_completed(
                thread_id,
                "start new work",
                crate::product::state::ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("conditional insert should not fail")
            .expect("completed goal should be replaced");

        assert_ne!(original.goal_id, replacement.goal_id);
        let expected = crate::product::state::ThreadGoal {
            thread_id,
            goal_id: replacement.goal_id.clone(),
            objective: "start new work".to_string(),
            status: crate::product::state::ThreadGoalStatus::Active,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: replacement.created_at,
            updated_at: replacement.updated_at,
        };
        assert_eq!(expected, replacement);
        assert_eq!(
            Some(expected),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn replace_thread_goal_if_goal_id_replaces_matching_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        let created_at = DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp");
        let updated_at = DateTime::from_timestamp(1_700_000_100, 0).expect("valid timestamp");

        let original = runtime
            .seed_thread_goal(
                thread_id,
                ThreadGoalSeed {
                    objective: "old objective".to_string(),
                    status: crate::product::state::ThreadGoalStatus::Active,
                    token_budget: Some(100),
                    tokens_used: 5,
                    time_used_seconds: 7,
                    created_at,
                    updated_at,
                },
            )
            .await
            .expect("goal seed should succeed");

        let replacement = runtime
            .replace_thread_goal_if_goal_id(
                thread_id,
                &original.goal_id,
                "new objective",
                crate::product::state::ThreadGoalStatus::Active,
                Some(10),
            )
            .await
            .expect("conditional replacement should succeed")
            .expect("matching goal should be replaced");

        assert_ne!(original.goal_id, replacement.goal_id);
        let expected = crate::product::state::ThreadGoal {
            thread_id,
            goal_id: replacement.goal_id.clone(),
            objective: "new objective".to_string(),
            status: crate::product::state::ThreadGoalStatus::Active,
            token_budget: Some(10),
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: replacement.created_at,
            updated_at: replacement.updated_at,
        };
        assert_eq!(expected, replacement);
        assert_eq!(
            Some(expected),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn replace_thread_goal_if_goal_id_rejects_stale_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        let original = runtime
            .replace_thread_goal(
                thread_id,
                "old objective",
                crate::product::state::ThreadGoalStatus::Active,
                Some(100),
            )
            .await
            .expect("goal replacement should succeed");
        let replacement = runtime
            .replace_thread_goal(
                thread_id,
                "new objective",
                crate::product::state::ThreadGoalStatus::Active,
                Some(10),
            )
            .await
            .expect("goal replacement should succeed");

        let stale_replacement = runtime
            .replace_thread_goal_if_goal_id(
                thread_id,
                &original.goal_id,
                "stale objective",
                crate::product::state::ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("conditional replacement should not fail");

        assert_eq!(None, stale_replacement);
        assert_eq!(
            Some(replacement),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn replace_unfinished_thread_goal_if_goal_id_rejects_completed_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        let completed = runtime
            .replace_thread_goal(
                thread_id,
                "completed objective",
                crate::product::state::ThreadGoalStatus::Complete,
                None,
            )
            .await
            .expect("goal replacement should succeed");

        let replacement = runtime
            .replace_unfinished_thread_goal_if_goal_id(
                thread_id,
                &completed.goal_id,
                "replacement objective",
                crate::product::state::ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("conditional replacement should not fail");

        assert_eq!(None, replacement);
        assert_eq!(
            Some(completed),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn replace_thread_goal_if_goal_id_does_not_recreate_cleared_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        let original = runtime
            .replace_thread_goal(
                thread_id,
                "old objective",
                crate::product::state::ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("goal replacement should succeed");
        assert!(
            runtime
                .delete_thread_goal(thread_id)
                .await
                .expect("goal delete should succeed")
        );

        let stale_replacement = runtime
            .replace_thread_goal_if_goal_id(
                thread_id,
                &original.goal_id,
                "stale objective",
                crate::product::state::ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("conditional replacement should not fail");

        assert_eq!(None, stale_replacement);
        assert_eq!(
            None,
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn seed_thread_goal_rewrites_thread_and_goal_ids() {
        let runtime = test_runtime().await;
        let thread_id = other_thread_id();
        let created_at = DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp");
        let updated_at = DateTime::from_timestamp(1_700_000_100, 0).expect("valid timestamp");

        let seeded = runtime
            .seed_thread_goal(
                thread_id,
                ThreadGoalSeed {
                    objective: "continue forked work".to_string(),
                    status: crate::product::state::ThreadGoalStatus::Active,
                    token_budget: Some(100),
                    tokens_used: 40,
                    time_used_seconds: 15,
                    created_at,
                    updated_at,
                },
            )
            .await
            .expect("goal seed should succeed");

        assert!(!seeded.goal_id.is_empty());
        let expected = crate::product::state::ThreadGoal {
            thread_id,
            goal_id: seeded.goal_id.clone(),
            objective: "continue forked work".to_string(),
            status: crate::product::state::ThreadGoalStatus::Active,
            token_budget: Some(100),
            tokens_used: 40,
            time_used_seconds: 15,
            created_at,
            updated_at,
        };
        assert_eq!(expected, seeded);
        assert_eq!(
            Some(expected),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn seed_thread_goal_applies_budget_limit() {
        let runtime = test_runtime().await;
        let thread_id = other_thread_id();
        let created_at = DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp");
        let updated_at = DateTime::from_timestamp(1_700_000_100, 0).expect("valid timestamp");

        let seeded = runtime
            .seed_thread_goal(
                thread_id,
                ThreadGoalSeed {
                    objective: "respect budget".to_string(),
                    status: crate::product::state::ThreadGoalStatus::Active,
                    token_budget: Some(100),
                    tokens_used: 100,
                    time_used_seconds: 15,
                    created_at,
                    updated_at,
                },
            )
            .await
            .expect("goal seed should succeed");

        assert_eq!(
            crate::product::state::ThreadGoalStatus::BudgetLimited,
            seeded.status
        );
    }

    #[tokio::test]
    async fn usage_accounting_transitions_to_budget_limited() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        runtime
            .replace_thread_goal(
                thread_id,
                "stay within budget",
                crate::product::state::ThreadGoalStatus::Active,
                Some(20),
            )
            .await
            .expect("goal replacement should succeed");

        let outcome = runtime
            .account_thread_goal_usage(thread_id, 7, 5, GoalAccountingMode::ActiveOnly, None)
            .await
            .expect("usage accounting should succeed");
        let GoalAccountingOutcome::Updated(goal) = outcome else {
            panic!("active goal should be updated");
        };
        assert_eq!(crate::product::state::ThreadGoalStatus::Active, goal.status);
        assert_eq!(5, goal.tokens_used);
        assert_eq!(7, goal.time_used_seconds);

        let outcome = runtime
            .account_thread_goal_usage(thread_id, 3, 15, GoalAccountingMode::ActiveOnly, None)
            .await
            .expect("usage accounting should succeed");
        let GoalAccountingOutcome::Updated(goal) = outcome else {
            panic!("budget crossing should update the goal");
        };
        assert_eq!(
            crate::product::state::ThreadGoalStatus::BudgetLimited,
            goal.status
        );
        assert_eq!(20, goal.tokens_used);
        assert_eq!(10, goal.time_used_seconds);
    }

    #[tokio::test]
    async fn pause_active_thread_goal_if_goal_id_rejects_stale_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        let original = runtime
            .replace_thread_goal(
                thread_id,
                "old objective",
                crate::product::state::ThreadGoalStatus::Active,
                Some(100),
            )
            .await
            .expect("goal replacement should succeed");

        let paused = runtime
            .pause_active_thread_goal_if_goal_id(thread_id, Some(&original.goal_id))
            .await
            .expect("goal pause should succeed")
            .expect("active goal should pause");
        assert_eq!(
            crate::product::state::ThreadGoal {
                status: crate::product::state::ThreadGoalStatus::Paused,
                updated_at: paused.updated_at,
                ..original
            },
            paused
        );

        let replacement = runtime
            .replace_thread_goal(
                thread_id,
                "new objective",
                crate::product::state::ThreadGoalStatus::Active,
                Some(10),
            )
            .await
            .expect("goal replacement should succeed");
        let stale_pause = runtime
            .pause_active_thread_goal_if_goal_id(thread_id, Some(&paused.goal_id))
            .await
            .expect("stale goal pause should not fail");

        assert_eq!(None, stale_pause);
        assert_eq!(
            Some(replacement),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }

    #[tokio::test]
    async fn expected_goal_id_rejects_stale_updates() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        let original = runtime
            .replace_thread_goal(
                thread_id,
                "old objective",
                crate::product::state::ThreadGoalStatus::Active,
                Some(100),
            )
            .await
            .expect("goal replacement should succeed");
        let replacement = runtime
            .replace_thread_goal(
                thread_id,
                "new objective",
                crate::product::state::ThreadGoalStatus::Active,
                Some(10),
            )
            .await
            .expect("goal replacement should succeed");

        let stale_update = runtime
            .update_thread_goal(
                thread_id,
                GoalUpdate {
                    objective: None,
                    status: Some(crate::product::state::ThreadGoalStatus::Complete),
                    token_budget: None,
                    expected_goal_id: Some(original.goal_id),
                },
            )
            .await
            .expect("goal update should succeed");
        assert_eq!(None, stale_update);
        assert_eq!(
            Some(replacement),
            runtime
                .get_thread_goal(thread_id)
                .await
                .expect("goal read should succeed")
        );
    }
}
