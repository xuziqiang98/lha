use crate::product::agent::input_slimming::InputSlimmingMetrics;
use crate::product::otel::OtelManager;

pub(super) fn emit_metrics(metrics: &InputSlimmingMetrics, otel: &OtelManager, model: &str) {
    otel.counter(
        "lha.input_slimming.candidate",
        i64::try_from(metrics.candidates).unwrap_or(i64::MAX),
        &[("model", model), ("feature_enabled", "true")],
    );
    for reference in &metrics.refs {
        otel.counter(
            "lha.input_slimming.slimmed",
            1,
            &[
                ("model", model),
                ("feature_enabled", "true"),
                ("strategy", reference.strategy.as_str()),
                ("tool_name", reference.tool_name.as_str()),
            ],
        );
    }
    for skipped in &metrics.skipped {
        otel.counter(
            "lha.input_slimming.skipped",
            1,
            &[
                ("model", model),
                ("feature_enabled", "true"),
                ("reason", skipped.reason.as_str()),
                (
                    "tool_name",
                    skipped.tool_name.as_deref().unwrap_or("unknown"),
                ),
            ],
        );
    }
    otel.histogram(
        "lha.input_slimming.tokens_before",
        i64::try_from(metrics.approx_tokens_before).unwrap_or(i64::MAX),
        &[("model", model), ("feature_enabled", "true")],
    );
    otel.histogram(
        "lha.input_slimming.tokens_after",
        i64::try_from(metrics.approx_tokens_after).unwrap_or(i64::MAX),
        &[("model", model), ("feature_enabled", "true")],
    );
    otel.histogram(
        "lha.input_slimming.tokens_saved",
        i64::try_from(metrics.approx_tokens_saved).unwrap_or(i64::MAX),
        &[("model", model), ("feature_enabled", "true")],
    );
}
