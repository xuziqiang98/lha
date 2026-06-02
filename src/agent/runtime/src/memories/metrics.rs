pub(crate) const STARTUP: &str = "lha.memory.startup";
pub(crate) const PHASE1_JOBS: &str = "lha.memory.phase1.jobs";
pub(crate) const PHASE1_TOKEN_USAGE: &str = "lha.memory.phase1.token_usage";
pub(crate) const PHASE2_JOBS: &str = "lha.memory.phase2.jobs";
pub(crate) const PHASE2_TOKEN_USAGE: &str = "lha.memory.phase2.token_usage";
pub(crate) const MODEL_FALLBACK: &str = "lha.memory.model_fallback";

pub(crate) fn counter(name: &'static str, value: u64, tags: &[(&str, &str)]) {
    let Some(metrics) = lha_otel::metrics::global() else {
        return;
    };
    if let Err(err) = metrics.counter(name, i64::try_from(value).unwrap_or(i64::MAX), tags) {
        tracing::warn!("memory metric counter [{name}] failed: {err}");
    }
}
