//! SQLite-backed state for rollout metadata.
//!
//! This crate is intentionally small and focused: it extracts rollout metadata
//! from JSONL rollouts and mirrors it into a local SQLite database. Backfill
//! orchestration and rollout scanning live in `lha-agent`.

mod extract;
pub mod log_db;
mod migrations;
mod model;
mod paths;
mod runtime;

pub use model::LogEntry;
pub use model::LogQuery;
pub use model::LogRow;
pub use model::ThreadGoal;
pub use model::ThreadGoalStatus;
pub use model::ThreadPlanRun;
pub use model::ThreadPlanRunStatus;
/// Preferred entrypoint: owns configuration and metrics.
pub use runtime::GoalAccountingMode;
pub use runtime::GoalAccountingOutcome;
pub use runtime::GoalUpdate;
pub use runtime::PlanRunAccountingMode;
pub use runtime::PlanRunAccountingOutcome;
pub use runtime::PlanRunUpdate;
pub use runtime::StateRuntime;
pub use runtime::ThreadGoalSeed;
pub use runtime::ThreadPlanRunSeed;

/// Low-level storage engine: useful for focused tests.
///
/// Most consumers should prefer [`StateRuntime`].
pub use extract::apply_rollout_item;
pub use model::Anchor;
pub use model::BackfillStats;
pub use model::ExtractionOutcome;
pub use model::SortKey;
pub use model::ThreadMetadata;
pub use model::ThreadMetadataBuilder;
pub use model::ThreadsPage;
pub use runtime::STATE_DB_FILENAME;

/// Errors encountered during DB operations. Tags: [stage]
pub const DB_ERROR_METRIC: &str = "lha.db.error";
/// Metrics on backfill process during first init of the db. Tags: [status]
pub const DB_METRIC_BACKFILL: &str = "lha.db.backfill";
/// Metrics on backfill duration during first init of the db. Tags: [status]
pub const DB_METRIC_BACKFILL_DURATION_MS: &str = "lha.db.backfill.duration_ms";
/// Metrics on errors during comparison between DB and rollout file. Tags: [stage]
pub const DB_METRIC_COMPARE_ERROR: &str = "lha.db.compare_error";
