use super::super::harness::attributes_to_map;
use super::super::harness::build_metrics_with_defaults;
use super::super::harness::find_metric;
use super::super::harness::latest_metrics;
use crate::product::app_server_protocol::AuthMode;
use crate::product::otel::OtelManager;
use crate::product::otel::metrics::Result;
use crate::product::protocol::ThreadId;
use crate::product::protocol::protocol::SessionSource;
use opentelemetry_sdk::metrics::data::AggregatedMetrics;
use opentelemetry_sdk::metrics::data::MetricData;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

// Ensures OtelManager attaches metadata tags when forwarding metrics.
#[test]
fn manager_attaches_metadata_tags_to_metrics() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[("service", "lha-cli")])?;
    let manager = OtelManager::new(
        ThreadId::new(),
        "gpt-5.1",
        "gpt-5.1",
        Some("account-id".to_string()),
        None,
        Some(AuthMode::ApiKey.to_string()),
        true,
        "tty".to_string(),
        SessionSource::Cli,
    )
    .with_metrics(metrics);

    manager.counter("codex.session_started", 1, &[("source", "tui")]);
    manager.shutdown_metrics()?;

    let resource_metrics = latest_metrics(&exporter);
    let metric =
        find_metric(&resource_metrics, "codex.session_started").expect("counter metric missing");
    let attrs = match metric.data() {
        AggregatedMetrics::U64(data) => match data {
            MetricData::Sum(sum) => {
                let points: Vec<_> = sum.data_points().collect();
                assert_eq!(points.len(), 1);
                attributes_to_map(points[0].attributes())
            }
            _ => panic!("unexpected counter aggregation"),
        },
        _ => panic!("unexpected counter data type"),
    };

    let expected = BTreeMap::from([
        (
            "app.version".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        ),
        ("auth_mode".to_string(), AuthMode::ApiKey.to_string()),
        ("model".to_string(), "gpt-5.1".to_string()),
        ("service".to_string(), "lha-cli".to_string()),
        ("session_source".to_string(), "cli".to_string()),
        ("source".to_string(), "tui".to_string()),
    ]);
    assert_eq!(attrs, expected);

    Ok(())
}

// Ensures metadata tagging can be disabled when recording via OtelManager.
#[test]
fn manager_allows_disabling_metadata_tags() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[])?;
    let manager = OtelManager::new(
        ThreadId::new(),
        "gpt-4o",
        "gpt-4o",
        Some("account-id".to_string()),
        None,
        Some(AuthMode::ApiKey.to_string()),
        true,
        "tty".to_string(),
        SessionSource::Cli,
    )
    .with_metrics_without_metadata_tags(metrics);

    manager.counter("codex.session_started", 1, &[("source", "tui")]);
    manager.shutdown_metrics()?;

    let resource_metrics = latest_metrics(&exporter);
    let metric =
        find_metric(&resource_metrics, "codex.session_started").expect("counter metric missing");
    let attrs = match metric.data() {
        AggregatedMetrics::U64(data) => match data {
            MetricData::Sum(sum) => {
                let points: Vec<_> = sum.data_points().collect();
                assert_eq!(points.len(), 1);
                attributes_to_map(points[0].attributes())
            }
            _ => panic!("unexpected counter aggregation"),
        },
        _ => panic!("unexpected counter data type"),
    };

    let expected = BTreeMap::from([("source".to_string(), "tui".to_string())]);
    assert_eq!(attrs, expected);

    Ok(())
}
