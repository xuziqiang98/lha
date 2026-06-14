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
                ("zone", reference.zone.as_str()),
            ],
        );
    }
    otel.counter(
        "lha.input_slimming.measured_only",
        i64::try_from(metrics.measured_only).unwrap_or(i64::MAX),
        &[("model", model), ("feature_enabled", "true")],
    );
    otel.counter(
        "lha.input_slimming.token_gate_fallback",
        i64::try_from(metrics.token_gate_fallbacks).unwrap_or(i64::MAX),
        &[("model", model), ("feature_enabled", "true")],
    );
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
    for metric in &metrics.strategy_metrics {
        let labels = &[
            ("model", model),
            ("feature_enabled", "true"),
            ("strategy", metric.strategy.as_str()),
            ("tool_name", metric.tool_name.as_str()),
            ("zone", metric.zone.as_str()),
            ("gate_method", metric.gate_method.as_str()),
        ];
        otel.histogram(
            "lha.input_slimming.strategy.tokens_before",
            i64::try_from(metric.tokens_before).unwrap_or(i64::MAX),
            labels,
        );
        otel.histogram(
            "lha.input_slimming.strategy.tokens_after",
            i64::try_from(metric.tokens_after).unwrap_or(i64::MAX),
            labels,
        );
        otel.histogram(
            "lha.input_slimming.strategy.tokens_saved",
            i64::try_from(metric.tokens_saved).unwrap_or(i64::MAX),
            labels,
        );
        let ratio_bps = if metric.tokens_before == 0 {
            0
        } else {
            metric.tokens_after.saturating_mul(10_000) / metric.tokens_before
        };
        otel.histogram(
            "lha.input_slimming.strategy.compression_ratio_bps",
            i64::try_from(ratio_bps).unwrap_or(i64::MAX),
            labels,
        );
    }
}
