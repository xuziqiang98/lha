mod client;
mod config;
mod error;
pub(crate) mod names;
pub(crate) mod runtime_metrics;
pub(crate) mod timer;
pub(crate) mod validation;

pub use crate::product::otel::metrics::client::MetricsClient;
pub use crate::product::otel::metrics::config::MetricsConfig;
pub use crate::product::otel::metrics::config::MetricsExporter;
pub use crate::product::otel::metrics::error::MetricsError;
pub use crate::product::otel::metrics::error::Result;
use std::sync::OnceLock;

static GLOBAL_METRICS: OnceLock<MetricsClient> = OnceLock::new();

pub(crate) fn install_global(metrics: MetricsClient) {
    let _ = GLOBAL_METRICS.set(metrics);
}

pub fn global() -> Option<MetricsClient> {
    GLOBAL_METRICS.get().cloned()
}
