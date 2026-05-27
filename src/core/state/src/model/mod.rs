mod log;
mod thread_goal;
mod thread_metadata;

pub use log::LogEntry;
pub use log::LogQuery;
pub use log::LogRow;
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

pub(crate) use thread_metadata::ThreadRow;
pub(crate) use thread_metadata::anchor_from_item;
pub(crate) use thread_metadata::datetime_to_epoch_seconds;

pub(crate) fn datetime_to_epoch_millis(dt: chrono::DateTime<chrono::Utc>) -> i64 {
    dt.timestamp_millis()
}

pub(crate) fn epoch_millis_to_datetime(
    millis: i64,
) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .ok_or_else(|| anyhow::anyhow!("invalid unix timestamp millis: {millis}"))
}
