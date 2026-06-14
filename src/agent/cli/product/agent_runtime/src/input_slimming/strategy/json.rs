use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::Value;
use serde_json::json;

use super::StrategyOutput;
use crate::product::agent::input_slimming::InputSlimmingStrategy;

const EDGE_SAMPLE_COUNT: usize = 4;
const MAX_SAMPLED_ITEMS: usize = 32;
const SHAPE_REPRESENTATIVE_SAMPLES: usize = 8;

#[derive(Debug, Clone)]
struct JsonRowScore {
    index: usize,
    score: i32,
    reasons: BTreeSet<JsonRowReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum JsonRowReason {
    Head,
    Tail,
    ErrorSignal,
    RareKey,
    RareScalar,
    NumericOutlier,
    ChangePoint,
    DuplicateRepresentative,
    ShapeRepresentative,
}

impl JsonRowReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Tail => "tail",
            Self::ErrorSignal => "error_signal",
            Self::RareKey => "rare_key",
            Self::RareScalar => "rare_scalar",
            Self::NumericOutlier => "numeric_outlier",
            Self::ChangePoint => "change_point",
            Self::DuplicateRepresentative => "duplicate_representative",
            Self::ShapeRepresentative => "shape_representative",
        }
    }

    fn weight(self) -> i32 {
        match self {
            Self::Head | Self::Tail => 100,
            Self::ErrorSignal => 90,
            Self::RareKey => 80,
            Self::RareScalar => 70,
            Self::NumericOutlier => 65,
            Self::ChangePoint => 60,
            Self::DuplicateRepresentative => 30,
            Self::ShapeRepresentative => 20,
        }
    }
}

pub(super) fn json_array_sample(text: &str) -> Option<StrategyOutput> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let Value::Array(items) = parsed else {
        return None;
    };
    if items.len() <= 24 {
        return None;
    }

    let schema = summarize_schema(&items);
    let row_scores = score_rows(&items);
    let selected = select_rows(&row_scores);
    let selection_reasons = selection_reasons(&row_scores, &selected);

    let sampled_items = selected
        .iter()
        .map(|idx| items[*idx].clone())
        .collect::<Vec<_>>();
    let body = json!({
        "input_slimming": {
            "strategy": "json_array_sample",
            "original_items": items.len(),
            "kept_items": sampled_items.len(),
            "omitted_items": items.len().saturating_sub(sampled_items.len()),
            "schema": schema,
            "selection_reasons": selection_reasons,
            "sampled_items": sampled_items,
        }
    });

    Some(StrategyOutput {
        strategy: InputSlimmingStrategy::JsonArraySample,
        body: serde_json::to_string(&body).ok()?,
    })
}

fn summarize_schema(items: &[Value]) -> Value {
    let mut keys: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    let mut object_rows = 0usize;
    let mut shape_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut duplicate_counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in items {
        let Value::Object(map) = item else {
            *duplicate_counts.entry(item.to_string()).or_default() += 1;
            continue;
        };
        object_rows += 1;
        *duplicate_counts.entry(item.to_string()).or_default() += 1;
        *shape_counts.entry(shape_signature(item)).or_default() += 1;
        for (key, value) in map {
            keys.entry(key.clone())
                .or_default()
                .insert(value_kind(value));
        }
    }

    let fields = keys
        .into_iter()
        .map(|(key, kinds)| {
            json!({
                "key": key,
                "value_kinds": kinds.into_iter().collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "object_rows": object_rows,
        "distinct_shapes": shape_counts.len(),
        "duplicate_rows": duplicate_counts
            .values()
            .map(|count| count.saturating_sub(1))
            .sum::<usize>(),
        "fields": fields,
    })
}

fn score_rows(items: &[Value]) -> Vec<JsonRowScore> {
    let rare_key_indices = rare_key_rows(items);
    let rare_scalar_indices = rare_scalar_rows(items);
    let numeric_outliers = numeric_outlier_rows(items);
    let change_points = change_point_rows(items);
    let duplicate_representatives = duplicate_representative_rows(items);
    let shape_representatives = shape_representative_rows(items);

    let mut rows = (0..items.len())
        .map(|index| JsonRowScore {
            index,
            score: 0,
            reasons: BTreeSet::new(),
        })
        .collect::<Vec<_>>();

    for row in rows.iter_mut().take(items.len().min(EDGE_SAMPLE_COUNT)) {
        add_reason(row, JsonRowReason::Head);
    }
    for row in rows
        .iter_mut()
        .skip(items.len().saturating_sub(EDGE_SAMPLE_COUNT))
    {
        add_reason(row, JsonRowReason::Tail);
    }
    for (index, item) in items.iter().enumerate() {
        if value_has_error_signal(item) {
            add_reason(&mut rows[index], JsonRowReason::ErrorSignal);
        }
    }
    for index in rare_key_indices {
        add_reason(&mut rows[index], JsonRowReason::RareKey);
    }
    for index in rare_scalar_indices {
        add_reason(&mut rows[index], JsonRowReason::RareScalar);
    }
    for index in numeric_outliers {
        add_reason(&mut rows[index], JsonRowReason::NumericOutlier);
    }
    for index in change_points {
        add_reason(&mut rows[index], JsonRowReason::ChangePoint);
    }
    for index in duplicate_representatives {
        add_reason(&mut rows[index], JsonRowReason::DuplicateRepresentative);
    }
    for index in shape_representatives {
        add_reason(&mut rows[index], JsonRowReason::ShapeRepresentative);
    }

    rows
}

fn add_reason(row: &mut JsonRowScore, reason: JsonRowReason) {
    if row.reasons.insert(reason) {
        row.score += reason.weight();
    }
}

fn select_rows(row_scores: &[JsonRowScore]) -> BTreeSet<usize> {
    let mut ranked = row_scores
        .iter()
        .filter(|row| row.score > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.index.cmp(&right.index))
    });

    ranked
        .into_iter()
        .take(MAX_SAMPLED_ITEMS)
        .map(|row| row.index)
        .collect()
}

fn selection_reasons(row_scores: &[JsonRowScore], selected: &BTreeSet<usize>) -> Value {
    let rows = selected
        .iter()
        .filter_map(|index| row_scores.get(*index))
        .map(|row| {
            json!({
                "index": row.index,
                "score": row.score,
                "reasons": row
                    .reasons
                    .iter()
                    .map(|reason| reason.as_str())
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    Value::Array(rows)
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn value_has_error_signal(value: &Value) -> bool {
    let lower = value.to_string().to_lowercase();
    ["error", "warn", "fail", "panic", "exception", "fatal"]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn rare_key_rows(items: &[Value]) -> Vec<usize> {
    let mut key_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in items {
        if let Value::Object(map) = item {
            for key in map.keys() {
                *key_counts.entry(key.as_str()).or_default() += 1;
            }
        }
    }

    items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let Value::Object(map) = item else {
                return None;
            };
            map.keys()
                .any(|key| key_counts.get(key.as_str()) == Some(&1))
                .then_some(idx)
        })
        .collect()
}

fn rare_scalar_rows(items: &[Value]) -> Vec<usize> {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for item in items {
        let Value::Object(map) = item else {
            continue;
        };
        for (key, value) in map {
            if let Some(scalar) = enum_like_scalar(value) {
                *counts.entry((key.clone(), scalar)).or_default() += 1;
            }
        }
    }

    items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let Value::Object(map) = item else {
                return None;
            };
            map.iter()
                .any(|(key, value)| {
                    enum_like_scalar(value)
                        .and_then(|scalar| counts.get(&(key.clone(), scalar)).copied())
                        == Some(1)
                })
                .then_some(idx)
        })
        .collect()
}

fn numeric_outlier_rows(items: &[Value]) -> BTreeSet<usize> {
    let mut by_key: BTreeMap<String, Vec<(usize, f64)>> = BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        let Value::Object(map) = item else {
            continue;
        };
        for (key, value) in map {
            if let Some(number) = value.as_f64() {
                by_key.entry(key.clone()).or_default().push((idx, number));
            }
        }
    }

    let mut selected = BTreeSet::new();
    for values in by_key.values() {
        if values.len() < 5 {
            continue;
        }
        let mean = values.iter().map(|(_, value)| value).sum::<f64>() / values.len() as f64;
        let variance = values
            .iter()
            .map(|(_, value)| {
                let delta = value - mean;
                delta * delta
            })
            .sum::<f64>()
            / values.len() as f64;
        let std_dev = variance.sqrt();

        if let Some((idx, _)) = values
            .iter()
            .min_by(|left, right| left.1.total_cmp(&right.1))
        {
            selected.insert(*idx);
        }
        if let Some((idx, _)) = values
            .iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
        {
            selected.insert(*idx);
        }
        if std_dev > 0.0 {
            for (idx, value) in values {
                if ((*value - mean) / std_dev).abs() >= 2.5 {
                    selected.insert(*idx);
                }
            }
        }
    }
    selected
}

fn change_point_rows(items: &[Value]) -> BTreeSet<usize> {
    let mut selected = BTreeSet::new();
    let mut previous: BTreeMap<String, String> = BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        let Value::Object(map) = item else {
            previous.clear();
            continue;
        };
        for (key, value) in map {
            let Some(scalar) = change_point_scalar(value) else {
                continue;
            };
            if previous
                .get(key)
                .is_some_and(|previous_scalar| previous_scalar != &scalar)
            {
                selected.insert(idx.saturating_sub(1));
                selected.insert(idx);
            }
            previous.insert(key.clone(), scalar);
        }
    }
    selected
}

fn change_point_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() && s.len() <= 80 => Some(s.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => value.as_f64().map(|number| {
            if number == 0.0 {
                "0".to_string()
            } else {
                format!("{:.0}", number.log10().floor())
            }
        }),
        Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

fn duplicate_representative_rows(items: &[Value]) -> BTreeSet<usize> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        groups.entry(item.to_string()).or_default().push(idx);
    }
    groups
        .into_values()
        .filter(|indices| indices.len() > 1)
        .filter_map(|indices| indices.first().copied())
        .collect()
}

fn shape_representative_rows(items: &[Value]) -> BTreeSet<usize> {
    let mut representatives = BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        representatives
            .entry(shape_signature(item))
            .or_insert_with(Vec::new)
            .push(idx);
    }

    representatives
        .into_values()
        .flat_map(|indices| {
            let step = (indices.len() / SHAPE_REPRESENTATIVE_SAMPLES.max(1)).max(1);
            indices
                .into_iter()
                .step_by(step)
                .take(SHAPE_REPRESENTATIVE_SAMPLES)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn shape_signature(value: &Value) -> String {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}:{}", value_kind(value)))
            .collect::<Vec<_>>()
            .join("|"),
        Value::Array(items) => format!("array:{}", items.len()),
        Value::Null => "null".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(_) => "string".to_string(),
    }
}

fn enum_like_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() && s.len() <= 80 => Some(s.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::input_slimming::strategy::assert_strategy_retains_needles;
    use pretty_assertions::assert_eq;

    #[test]
    fn json_array_strategy_preserves_schema_errors_rare_outliers_and_change_points() {
        let items = (0..220)
            .map(|idx| {
                if idx == 10 {
                    json!({"id": idx, "status": "error", "message": "failed", "payload": "x".repeat(80)})
                } else if idx == 20 {
                    json!({"id": idx, "status": "ok", "rare_key": "needle", "payload": "x".repeat(80)})
                } else if idx == 25 {
                    json!({"id": idx, "status": "odd_status", "payload": "x".repeat(80)})
                } else if idx == 30 {
                    json!({"id": idx, "status": "ok", "latency_ms": 10_000, "payload": "x".repeat(80)})
                } else if idx >= 180 {
                    json!({"id": idx, "status": "degraded", "latency_ms": idx, "payload": "x".repeat(80)})
                } else {
                    json!({"id": idx, "status": "ok", "latency_ms": idx, "payload": "x".repeat(80)})
                }
            })
            .collect::<Vec<_>>();
        let text = serde_json::to_string(&items).expect("json");

        let output = json_array_sample(&text).expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::JsonArraySample);
        assert_strategy_retains_needles(
            &text,
            &output.body,
            &[
                "\"original_items\":220",
                "\"key\":\"status\"",
                "\"id\":10",
                "\"rare_key\"",
                "odd_status",
                "10000",
                "degraded",
                "selection_reasons",
                "numeric_outlier",
                "change_point",
            ],
        );
    }

    #[test]
    fn json_array_strategy_skips_small_arrays() {
        let text = serde_json::to_string(&vec![json!({"id": 1}); 24]).expect("json");
        assert_eq!(json_array_sample(&text), None);
    }

    #[test]
    fn json_array_strategy_dedupes_identical_rows_with_representative() {
        let mut items = vec![json!({"kind": "same", "value": 1, "payload": "x".repeat(120)}); 180];
        items[90] = json!({"kind": "same", "value": 999, "message": "panic needle", "payload": "x".repeat(120)});
        let text = serde_json::to_string(&items).expect("json");

        let output = json_array_sample(&text).expect("strategy output");

        assert_strategy_retains_needles(
            &text,
            &output.body,
            &[
                "duplicate_representative",
                "panic needle",
                "\"omitted_items\"",
            ],
        );
    }

    #[test]
    fn json_array_strategy_handles_mixed_arrays() {
        let items = (0..40)
            .map(|idx| {
                if idx == 20 {
                    json!("fatal needle")
                } else if idx % 2 == 0 {
                    json!({"id": idx, "status": "ok"})
                } else {
                    json!(idx)
                }
            })
            .collect::<Vec<_>>();
        let text = serde_json::to_string(&items).expect("json");

        let output = json_array_sample(&text).expect("strategy output");

        assert!(output.body.contains("fatal needle"));
    }
}
