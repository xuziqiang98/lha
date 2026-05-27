use crate::DB_ERROR_METRIC;
use crate::LogEntry;
use crate::LogQuery;
use crate::LogRow;
use crate::SortKey;
use crate::ThreadMetadata;
use crate::ThreadMetadataBuilder;
use crate::ThreadsPage;
use crate::apply_rollout_item;
use crate::migrations::MIGRATOR;
use crate::model::ThreadGoalRow;
use crate::model::ThreadRow;
use crate::model::anchor_from_item;
use crate::model::datetime_to_epoch_millis;
use crate::model::datetime_to_epoch_seconds;
use crate::paths::file_modified_time_utc;
use adam_otel::OtelManager;
use adam_protocol::ThreadId;
use adam_protocol::protocol::RolloutItem;
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

const METRIC_DB_INIT: &str = "adam.db.init";

pub struct GoalUpdate {
    pub objective: Option<String>,
    pub status: Option<crate::ThreadGoalStatus>,
    pub token_budget: Option<Option<i64>>,
    pub expected_goal_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadGoalSeed {
    pub objective: String,
    pub status: crate::ThreadGoalStatus,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum GoalAccountingOutcome {
    Unchanged(Option<crate::ThreadGoal>),
    Updated(crate::ThreadGoal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoalAccountingMode {
    ActiveStatusOnly,
    ActiveOnly,
    ActiveOrComplete,
    ActiveOrStopped,
}

#[derive(Clone)]
pub struct StateRuntime {
    adam_home: PathBuf,
    default_provider: String,
    pool: Arc<sqlx::SqlitePool>,
}

impl StateRuntime {
    /// Initialize the state runtime using the provided Adam home and default provider.
    ///
    /// This opens (and migrates) the SQLite database at `adam_home/state.sqlite`.
    pub async fn init(
        adam_home: PathBuf,
        default_provider: String,
        otel: Option<OtelManager>,
    ) -> anyhow::Result<Arc<Self>> {
        tokio::fs::create_dir_all(&adam_home).await?;
        let state_path = adam_home.join(STATE_DB_FILENAME);
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
        if let Some(otel) = otel.as_ref() {
            otel.counter(METRIC_DB_INIT, 1, &[("status", "opened")]);
        }
        let runtime = Arc::new(Self {
            pool,
            adam_home,
            default_provider,
        });
        if !existed && let Some(otel) = otel.as_ref() {
            otel.counter(METRIC_DB_INIT, 1, &[("status", "created")]);
        }
        Ok(runtime)
    }

    /// Return the configured Adam home directory for this runtime.
    pub fn adam_home(&self) -> &Path {
        self.adam_home.as_path()
    }

    pub async fn get_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
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

        row.map(|row| ThreadGoalRow::try_from_row(&row).and_then(crate::ThreadGoal::try_from))
            .transpose()
    }

    pub async fn replace_thread_goal(
        &self,
        thread_id: ThreadId,
        objective: &str,
        status: crate::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<crate::ThreadGoal> {
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

        ThreadGoalRow::try_from_row(&row).and_then(crate::ThreadGoal::try_from)
    }

    pub async fn replace_thread_goal_if_goal_id(
        &self,
        thread_id: ThreadId,
        expected_goal_id: &str,
        objective: &str,
        status: crate::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
        let goal_id = Uuid::new_v4().to_string();
        let now_ms = datetime_to_epoch_millis(Utc::now());
        let status = status_after_budget_limit(status, 0, token_budget);
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
        .fetch_optional(self.pool.as_ref())
        .await?;

        row.map(|row| ThreadGoalRow::try_from_row(&row).and_then(crate::ThreadGoal::try_from))
            .transpose()
    }

    pub async fn insert_thread_goal(
        &self,
        thread_id: ThreadId,
        objective: &str,
        status: crate::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
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

        row.map(|row| ThreadGoalRow::try_from_row(&row).and_then(crate::ThreadGoal::try_from))
            .transpose()
    }

    pub async fn insert_thread_goal_or_replace_completed(
        &self,
        thread_id: ThreadId,
        objective: &str,
        status: crate::ThreadGoalStatus,
        token_budget: Option<i64>,
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
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
        .bind(crate::ThreadGoalStatus::Complete.as_str())
        .fetch_optional(self.pool.as_ref())
        .await?;

        row.map(|row| ThreadGoalRow::try_from_row(&row).and_then(crate::ThreadGoal::try_from))
            .transpose()
    }

    pub async fn seed_thread_goal(
        &self,
        thread_id: ThreadId,
        seed: ThreadGoalSeed,
    ) -> anyhow::Result<crate::ThreadGoal> {
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

        ThreadGoalRow::try_from_row(&row).and_then(crate::ThreadGoal::try_from)
    }

    pub async fn update_thread_goal(
        &self,
        thread_id: ThreadId,
        update: GoalUpdate,
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
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
                .bind(crate::ThreadGoalStatus::BudgetLimited.as_str())
                .bind(status.as_str())
                .bind(crate::ThreadGoalStatus::Paused.as_str())
                .bind(crate::ThreadGoalStatus::Blocked.as_str())
                .bind(status.as_str())
                .bind(token_budget)
                .bind(token_budget)
                .bind(crate::ThreadGoalStatus::BudgetLimited.as_str())
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
                .bind(crate::ThreadGoalStatus::BudgetLimited.as_str())
                .bind(status.as_str())
                .bind(crate::ThreadGoalStatus::Paused.as_str())
                .bind(crate::ThreadGoalStatus::Blocked.as_str())
                .bind(status.as_str())
                .bind(crate::ThreadGoalStatus::BudgetLimited.as_str())
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
                .bind(crate::ThreadGoalStatus::BudgetLimited.as_str())
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
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
        self.pause_active_thread_goal_if_goal_id(thread_id, None)
            .await
    }

    pub async fn pause_active_thread_goal_if_goal_id(
        &self,
        thread_id: ThreadId,
        expected_goal_id: Option<&str>,
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
        self.update_active_thread_goal_status(
            thread_id,
            crate::ThreadGoalStatus::Paused,
            expected_goal_id,
        )
        .await
    }

    pub async fn usage_limit_active_thread_goal(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
        self.update_active_thread_goal_status(
            thread_id,
            crate::ThreadGoalStatus::UsageLimited,
            None,
        )
        .await
    }

    async fn update_active_thread_goal_status(
        &self,
        thread_id: ThreadId,
        status: crate::ThreadGoalStatus,
        expected_goal_id: Option<&str>,
    ) -> anyhow::Result<Option<crate::ThreadGoal>> {
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
            .bind(crate::ThreadGoalStatus::BudgetLimited.as_str())
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

        let updated = ThreadGoalRow::try_from_row(&row).and_then(crate::ThreadGoal::try_from)?;
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
    pub async fn get_thread(&self, id: ThreadId) -> anyhow::Result<Option<crate::ThreadMetadata>> {
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
    git_origin_url
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
        anchor: Option<&crate::Anchor>,
        sort_key: crate::SortKey,
        allowed_sources: &[String],
        model_providers: Option<&[String]>,
        archived_only: bool,
    ) -> anyhow::Result<crate::ThreadsPage> {
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
    git_origin_url
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
        anchor: Option<&crate::Anchor>,
        sort_key: crate::SortKey,
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
    pub async fn upsert_thread(&self, metadata: &crate::ThreadMetadata) -> anyhow::Result<()> {
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
    git_origin_url
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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

fn status_after_budget_limit(
    status: crate::ThreadGoalStatus,
    tokens_used: i64,
    token_budget: Option<i64>,
) -> crate::ThreadGoalStatus {
    if status == crate::ThreadGoalStatus::Active
        && token_budget.is_some_and(|budget| tokens_used >= budget)
    {
        crate::ThreadGoalStatus::BudgetLimited
    } else {
        status
    }
}

fn push_thread_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    archived_only: bool,
    allowed_sources: &'a [String],
    model_providers: Option<&'a [String]>,
    anchor: Option<&crate::Anchor>,
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
        let path = std::env::temp_dir().join(format!("adam-state-test-{}", Uuid::new_v4()));
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

    #[tokio::test]
    async fn replace_update_insert_and_delete_thread_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();

        let goal = runtime
            .replace_thread_goal(
                thread_id,
                "optimize the benchmark",
                crate::ThreadGoalStatus::Active,
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
                crate::ThreadGoalStatus::Active,
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
                    status: Some(crate::ThreadGoalStatus::Paused),
                    token_budget: Some(Some(200_000)),
                    expected_goal_id: Some(goal.goal_id.clone()),
                },
            )
            .await
            .expect("goal update should succeed")
            .expect("goal should exist");
        assert_eq!(
            crate::ThreadGoal {
                objective: "optimize the benchmark carefully".to_string(),
                status: crate::ThreadGoalStatus::Paused,
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
                crate::ThreadGoalStatus::Active,
                Some(100_000),
            )
            .await
            .expect("goal replacement should succeed");

        let replacement = runtime
            .insert_thread_goal_or_replace_completed(
                thread_id,
                "start new work",
                crate::ThreadGoalStatus::Active,
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
                crate::ThreadGoalStatus::Active,
                Some(100_000),
            )
            .await
            .expect("goal replacement should succeed");
        runtime
            .update_thread_goal(
                thread_id,
                GoalUpdate {
                    objective: None,
                    status: Some(crate::ThreadGoalStatus::Complete),
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
                crate::ThreadGoalStatus::Active,
                None,
            )
            .await
            .expect("conditional insert should not fail")
            .expect("completed goal should be replaced");

        assert_ne!(original.goal_id, replacement.goal_id);
        let expected = crate::ThreadGoal {
            thread_id,
            goal_id: replacement.goal_id.clone(),
            objective: "start new work".to_string(),
            status: crate::ThreadGoalStatus::Active,
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
                    status: crate::ThreadGoalStatus::Active,
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
                crate::ThreadGoalStatus::Active,
                Some(10),
            )
            .await
            .expect("conditional replacement should succeed")
            .expect("matching goal should be replaced");

        assert_ne!(original.goal_id, replacement.goal_id);
        let expected = crate::ThreadGoal {
            thread_id,
            goal_id: replacement.goal_id.clone(),
            objective: "new objective".to_string(),
            status: crate::ThreadGoalStatus::Active,
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
                crate::ThreadGoalStatus::Active,
                Some(100),
            )
            .await
            .expect("goal replacement should succeed");
        let replacement = runtime
            .replace_thread_goal(
                thread_id,
                "new objective",
                crate::ThreadGoalStatus::Active,
                Some(10),
            )
            .await
            .expect("goal replacement should succeed");

        let stale_replacement = runtime
            .replace_thread_goal_if_goal_id(
                thread_id,
                &original.goal_id,
                "stale objective",
                crate::ThreadGoalStatus::Active,
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
    async fn replace_thread_goal_if_goal_id_does_not_recreate_cleared_goal() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        let original = runtime
            .replace_thread_goal(
                thread_id,
                "old objective",
                crate::ThreadGoalStatus::Active,
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
                crate::ThreadGoalStatus::Active,
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
                    status: crate::ThreadGoalStatus::Active,
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
        let expected = crate::ThreadGoal {
            thread_id,
            goal_id: seeded.goal_id.clone(),
            objective: "continue forked work".to_string(),
            status: crate::ThreadGoalStatus::Active,
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
                    status: crate::ThreadGoalStatus::Active,
                    token_budget: Some(100),
                    tokens_used: 100,
                    time_used_seconds: 15,
                    created_at,
                    updated_at,
                },
            )
            .await
            .expect("goal seed should succeed");

        assert_eq!(crate::ThreadGoalStatus::BudgetLimited, seeded.status);
    }

    #[tokio::test]
    async fn usage_accounting_transitions_to_budget_limited() {
        let runtime = test_runtime().await;
        let thread_id = test_thread_id();
        runtime
            .replace_thread_goal(
                thread_id,
                "stay within budget",
                crate::ThreadGoalStatus::Active,
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
        assert_eq!(crate::ThreadGoalStatus::Active, goal.status);
        assert_eq!(5, goal.tokens_used);
        assert_eq!(7, goal.time_used_seconds);

        let outcome = runtime
            .account_thread_goal_usage(thread_id, 3, 15, GoalAccountingMode::ActiveOnly, None)
            .await
            .expect("usage accounting should succeed");
        let GoalAccountingOutcome::Updated(goal) = outcome else {
            panic!("budget crossing should update the goal");
        };
        assert_eq!(crate::ThreadGoalStatus::BudgetLimited, goal.status);
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
                crate::ThreadGoalStatus::Active,
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
            crate::ThreadGoal {
                status: crate::ThreadGoalStatus::Paused,
                updated_at: paused.updated_at,
                ..original
            },
            paused
        );

        let replacement = runtime
            .replace_thread_goal(
                thread_id,
                "new objective",
                crate::ThreadGoalStatus::Active,
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
                crate::ThreadGoalStatus::Active,
                Some(100),
            )
            .await
            .expect("goal replacement should succeed");
        let replacement = runtime
            .replace_thread_goal(
                thread_id,
                "new objective",
                crate::ThreadGoalStatus::Active,
                Some(10),
            )
            .await
            .expect("goal replacement should succeed");

        let stale_update = runtime
            .update_thread_goal(
                thread_id,
                GoalUpdate {
                    objective: None,
                    status: Some(crate::ThreadGoalStatus::Complete),
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
