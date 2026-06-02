use crate::config::Config;
use crate::features::Feature;
use crate::path_utils;
use crate::rollout::list::Cursor;
use crate::rollout::list::ThreadSortKey;
use crate::rollout::metadata;
use crate::rollout::recorder::is_unsupported_rollout_schema_anyhow;
use chrono::DateTime;
use chrono::NaiveDateTime;
use chrono::Timelike;
use chrono::Utc;
use lha_otel::OtelManager;
use lha_protocol::ThreadId;
use lha_protocol::protocol::RolloutItem;
use lha_protocol::protocol::SessionSource;
use lha_state::DB_METRIC_COMPARE_ERROR;
pub use lha_state::LogEntry;
use lha_state::MemoryStoreMode;
use lha_state::STATE_DB_FILENAME;
use lha_state::ThreadMetadataBuilder;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

/// Core-facing handle to the optional SQLite-backed state runtime.
pub type StateDbHandle = Arc<lha_state::StateRuntime>;

/// Initialize the state runtime when persistence-backed features are enabled. To only be used
/// inside `core`. The initialization should not be done anywhere else.
pub(crate) async fn init_if_enabled(
    config: &Config,
    otel: Option<&OtelManager>,
) -> Option<StateDbHandle> {
    let state_path = config.lha_home.join(STATE_DB_FILENAME);
    if !state_db_feature_enabled(config) {
        return None;
    }
    let existed = tokio::fs::try_exists(&state_path).await.unwrap_or(false);
    let memory_store_mode = if config.features.enabled(Feature::MemoryTool) {
        MemoryStoreMode::Required
    } else {
        MemoryStoreMode::Disabled
    };
    let runtime = match lha_state::StateRuntime::init_with_memory_store(
        config.lha_home.clone(),
        config.model_provider_id.clone(),
        otel.cloned(),
        memory_store_mode,
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(
                "failed to initialize state runtime at {}: {err}",
                config.lha_home.display()
            );
            if let Some(otel) = otel {
                otel.counter("lha.db.init", 1, &[("status", "init_error")]);
            }
            return None;
        }
    };
    if !existed {
        let runtime_for_backfill = Arc::clone(&runtime);
        let config_for_backfill = config.clone();
        let otel_for_backfill = otel.cloned();
        tokio::task::spawn(async move {
            metadata::backfill_sessions(
                runtime_for_backfill.as_ref(),
                &config_for_backfill,
                otel_for_backfill.as_ref(),
            )
            .await;
        });
    }
    Some(runtime)
}

/// Get the DB if the feature is enabled and the DB exists.
pub async fn get_state_db(config: &Config, otel: Option<&OtelManager>) -> Option<StateDbHandle> {
    let state_path = config.lha_home.join(STATE_DB_FILENAME);
    if !state_db_feature_enabled(config)
        || !tokio::fs::try_exists(&state_path).await.unwrap_or(false)
    {
        return None;
    }
    lha_state::StateRuntime::init_with_memory_store(
        config.lha_home.clone(),
        config.model_provider_id.clone(),
        otel.cloned(),
        MemoryStoreMode::Disabled,
    )
    .await
    .ok()
}

fn state_db_feature_enabled(config: &Config) -> bool {
    config.features.enabled(Feature::Sqlite)
        || config.features.enabled(Feature::Goals)
        || config.features.enabled(Feature::PlanCompletion)
        || config.features.enabled(Feature::MemoryTool)
}

/// Open the state runtime when the SQLite file exists, without feature gating.
///
/// This is used for parity checks during the SQLite migration phase.
pub async fn open_if_present(lha_home: &Path, default_provider: &str) -> Option<StateDbHandle> {
    let db_path = lha_home.join(STATE_DB_FILENAME);
    if !tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
        return None;
    }
    let runtime = lha_state::StateRuntime::init_with_memory_store(
        lha_home.to_path_buf(),
        default_provider.to_string(),
        None,
        MemoryStoreMode::Disabled,
    )
    .await
    .ok()?;
    Some(runtime)
}

fn cursor_to_anchor(cursor: Option<&Cursor>) -> Option<lha_state::Anchor> {
    let cursor = cursor?;
    let value = serde_json::to_value(cursor).ok()?;
    let cursor_str = value.as_str()?;
    let (ts_str, id_str) = cursor_str.split_once('|')?;
    if id_str.contains('|') {
        return None;
    }
    let id = Uuid::parse_str(id_str).ok()?;
    let ts = if let Ok(naive) = NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%dT%H-%M-%S") {
        DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc)
    } else if let Ok(dt) = DateTime::parse_from_rfc3339(ts_str) {
        dt.with_timezone(&Utc)
    } else {
        return None;
    }
    .with_nanosecond(0)?;
    Some(lha_state::Anchor { ts, id })
}

/// List thread ids from SQLite for parity checks without rollout scanning.
#[allow(clippy::too_many_arguments)]
pub async fn list_thread_ids_db(
    context: Option<&lha_state::StateRuntime>,
    lha_home: &Path,
    page_size: usize,
    cursor: Option<&Cursor>,
    sort_key: ThreadSortKey,
    allowed_sources: &[SessionSource],
    model_providers: Option<&[String]>,
    cwd_filter: Option<&Path>,
    archived_only: bool,
    stage: &str,
) -> Option<Vec<ThreadId>> {
    let ctx = context?;
    if ctx.lha_home() != lha_home {
        warn!(
            "state db lha_home mismatch: expected {}, got {}",
            ctx.lha_home().display(),
            lha_home.display()
        );
    }

    let anchor = cursor_to_anchor(cursor);
    let allowed_sources: Vec<String> = allowed_sources
        .iter()
        .map(|value| match serde_json::to_value(value) {
            Ok(Value::String(s)) => s,
            Ok(other) => other.to_string(),
            Err(_) => String::new(),
        })
        .collect();
    let model_providers = model_providers.map(<[String]>::to_vec);
    let sort_key = match sort_key {
        ThreadSortKey::CreatedAt => lha_state::SortKey::CreatedAt,
        ThreadSortKey::UpdatedAt => lha_state::SortKey::UpdatedAt,
    };
    let result = if let Some(cwd_filter) = cwd_filter {
        collect_thread_ids_with_cwd_filter(
            ctx,
            page_size,
            anchor.as_ref(),
            sort_key,
            allowed_sources.as_slice(),
            model_providers.as_deref(),
            cwd_filter,
            archived_only,
        )
        .await
    } else {
        ctx.list_thread_ids(
            page_size,
            anchor.as_ref(),
            sort_key,
            allowed_sources.as_slice(),
            model_providers.as_deref(),
            archived_only,
        )
        .await
    };
    match result {
        Ok(ids) => Some(ids),
        Err(err) => {
            warn!("state db list_thread_ids failed during {stage}: {err}");
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn collect_thread_ids_with_cwd_filter(
    context: &lha_state::StateRuntime,
    page_size: usize,
    anchor: Option<&lha_state::Anchor>,
    sort_key: lha_state::SortKey,
    allowed_sources: &[String],
    model_providers: Option<&[String]>,
    cwd_filter: &Path,
    archived_only: bool,
) -> anyhow::Result<Vec<ThreadId>> {
    let mut ids = Vec::with_capacity(page_size);
    let mut next_anchor = anchor.cloned();

    while ids.len() < page_size {
        let page = context
            .list_threads(
                page_size,
                next_anchor.as_ref(),
                sort_key,
                allowed_sources,
                model_providers,
                archived_only,
            )
            .await?;
        if page.items.is_empty() {
            break;
        }

        for item in page.items {
            if paths_match(item.cwd.as_path(), cwd_filter) {
                ids.push(item.id);
                if ids.len() == page_size {
                    break;
                }
            }
        }

        let Some(anchor) = page.next_anchor else {
            break;
        };
        next_anchor = Some(anchor);
    }

    Ok(ids)
}

fn paths_match(a: &Path, b: &Path) -> bool {
    if let (Ok(canonical_a), Ok(canonical_b)) = (
        path_utils::normalize_for_path_comparison(a),
        path_utils::normalize_for_path_comparison(b),
    ) {
        return canonical_a == canonical_b;
    }
    a == b
}

/// Look up the rollout path for a thread id using SQLite.
pub async fn find_rollout_path_by_id(
    context: Option<&lha_state::StateRuntime>,
    thread_id: ThreadId,
    archived_only: Option<bool>,
    stage: &str,
) -> Option<PathBuf> {
    let ctx = context?;
    ctx.find_rollout_path_by_id(thread_id, archived_only)
        .await
        .unwrap_or_else(|err| {
            warn!("state db find_rollout_path_by_id failed during {stage}: {err}");
            None
        })
}

/// Reconcile rollout items into SQLite, falling back to scanning the rollout file.
pub async fn reconcile_rollout(
    context: Option<&lha_state::StateRuntime>,
    rollout_path: &Path,
    default_provider: &str,
    builder: Option<&ThreadMetadataBuilder>,
    items: &[RolloutItem],
) {
    let Some(ctx) = context else {
        return;
    };
    if builder.is_some() || !items.is_empty() {
        apply_rollout_items(
            Some(ctx),
            rollout_path,
            default_provider,
            builder,
            items,
            "reconcile_rollout",
        )
        .await;
        return;
    }
    let outcome =
        match metadata::extract_metadata_from_rollout(rollout_path, default_provider, None).await {
            Ok(outcome) => outcome,
            Err(err) => {
                if is_unsupported_rollout_schema_anyhow(&err) {
                    warn!(
                        "skipping unsupported legacy rollout {}",
                        rollout_path.display()
                    );
                    return;
                }
                warn!(
                    "state db reconcile_rollout extraction failed {}: {err}",
                    rollout_path.display()
                );
                return;
            }
        };
    if let Err(err) = ctx.upsert_thread(&outcome.metadata).await {
        warn!(
            "state db reconcile_rollout upsert failed {}: {err}",
            rollout_path.display()
        );
    }
}

/// Apply rollout items incrementally to SQLite.
pub async fn apply_rollout_items(
    context: Option<&lha_state::StateRuntime>,
    rollout_path: &Path,
    _default_provider: &str,
    builder: Option<&ThreadMetadataBuilder>,
    items: &[RolloutItem],
    stage: &str,
) {
    let Some(ctx) = context else {
        return;
    };
    let mut builder = match builder {
        Some(builder) => builder.clone(),
        None => match metadata::builder_from_items(items, rollout_path) {
            Some(builder) => builder,
            None => {
                warn!(
                    "state db apply_rollout_items missing builder during {stage}: {}",
                    rollout_path.display()
                );
                record_discrepancy(stage, "missing_builder");
                return;
            }
        },
    };
    builder.rollout_path = rollout_path.to_path_buf();
    if let Err(err) = ctx.apply_rollout_items(&builder, items, None).await {
        warn!(
            "state db apply_rollout_items failed during {stage} for {}: {err}",
            rollout_path.display()
        );
    }
}

/// Record a state discrepancy metric with a stage and reason tag.
pub fn record_discrepancy(stage: &str, reason: &str) {
    // We access the global metric because the call sites might not have access to the broader
    // OtelManager.
    tracing::warn!("state db record_discrepancy: {stage}{reason}");
    if let Some(metric) = lha_otel::metrics::global() {
        let _ = metric.counter(
            DB_METRIC_COMPARE_ERROR,
            1,
            &[("stage", stage), ("reason", reason)],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollout::list::parse_cursor;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn init_if_enabled_without_memory_feature_ignores_corrupt_memory_db() {
        let home = tempfile::tempdir().expect("tempdir");
        tokio::fs::write(
            home.path().join(lha_state::MEMORIES_DB_FILENAME),
            "not sqlite",
        )
        .await
        .expect("write corrupt memories db");
        let mut config = crate::config::test_config();
        config.lha_home = home.path().to_path_buf();
        config.features.enable(Feature::Goals);
        config.features.disable(Feature::MemoryTool);

        let runtime = init_if_enabled(&config, None)
            .await
            .expect("state runtime should initialize");

        assert!(runtime.memories().is_none());
    }

    #[tokio::test]
    async fn init_if_enabled_with_memory_feature_rejects_corrupt_memory_db() {
        let home = tempfile::tempdir().expect("tempdir");
        tokio::fs::write(
            home.path().join(lha_state::MEMORIES_DB_FILENAME),
            "not sqlite",
        )
        .await
        .expect("write corrupt memories db");
        let mut config = crate::config::test_config();
        config.lha_home = home.path().to_path_buf();
        config.features.enable(Feature::MemoryTool);

        assert!(init_if_enabled(&config, None).await.is_none());
    }

    #[test]
    fn cursor_to_anchor_normalizes_timestamp_format() {
        let uuid = Uuid::new_v4();
        let ts_str = "2026-01-27T12-34-56";
        let token = format!("{ts_str}|{uuid}");
        let cursor = parse_cursor(token.as_str()).expect("cursor should parse");
        let anchor = cursor_to_anchor(Some(&cursor)).expect("anchor should parse");

        let naive =
            NaiveDateTime::parse_from_str(ts_str, "%Y-%m-%dT%H-%M-%S").expect("ts should parse");
        let expected_ts = DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc)
            .with_nanosecond(0)
            .expect("nanosecond");

        assert_eq!(anchor.id, uuid);
        assert_eq!(anchor.ts, expected_ts);
    }
}
