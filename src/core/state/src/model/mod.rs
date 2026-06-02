mod log;
mod memories;
mod thread_goal;
mod thread_metadata;
mod thread_plan_run;

pub use log::LogEntry;
pub use log::LogQuery;
pub use log::LogRow;
pub use memories::Phase2JobClaimOutcome;
pub use memories::Stage1JobClaim;
pub use memories::Stage1JobClaimOutcome;
pub use memories::Stage1Output;
pub use memories::Stage1StartupClaimParams;
pub use thread_goal::ThreadGoal;
pub(crate) use thread_goal::ThreadGoalRow;
pub use thread_goal::ThreadGoalStatus;
pub use thread_metadata::Anchor;
pub use thread_metadata::BackfillStats;
pub use thread_metadata::ExtractionOutcome;
pub use thread_metadata::SortKey;
pub use thread_metadata::ThreadMetadata;
pub use thread_metadata::ThreadMetadataBuilder;
pub use thread_metadata::ThreadsPage;
pub use thread_plan_run::ThreadPlanRun;
pub(crate) use thread_plan_run::ThreadPlanRunRow;
pub use thread_plan_run::ThreadPlanRunStatus;

pub(crate) use thread_metadata::ThreadRow;
pub(crate) use thread_metadata::anchor_from_item;
pub(crate) use thread_metadata::datetime_to_epoch_seconds;
pub(crate) use thread_metadata::epoch_seconds_to_datetime;

pub(crate) fn datetime_to_epoch_millis(dt: chrono::DateTime<chrono::Utc>) -> i64 {
    dt.timestamp_millis()
}

pub(crate) fn epoch_millis_to_datetime(
    millis: i64,
) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .ok_or_else(|| anyhow::anyhow!("invalid unix timestamp millis: {millis}"))
}
