use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::Value;
use serde_json::json;

use crate::product::agent::input_slimming::InputSlimmingOptions;
use crate::product::agent::input_slimming::InputSlimmingStrategy;
use crate::product::agent::truncate::TruncationPolicy;
use crate::product::agent::truncate::approx_token_count;
use crate::product::agent::truncate::truncate_text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StrategyOutput {
    pub(super) strategy: InputSlimmingStrategy,
    pub(super) body: String,
}

pub(super) fn slim_text(
    text: &str,
    success: Option<bool>,
    options: InputSlimmingOptions,
) -> Option<StrategyOutput> {
    if let Some(output) = json_array_sample(text) {
        return Some(output);
    }
    if let Some(output) = diff_compact(text) {
        return Some(output);
    }
    if let Some(output) = search_result_compact(text) {
        return Some(output);
    }
    if looks_like_log(text) {
        return Some(log_compact(text));
    }
    if success == Some(false) {
        return None;
    }
    plain_text_head_tail(text, options)
}

fn json_array_sample(text: &str) -> Option<StrategyOutput> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let Value::Array(items) = parsed else {
        return None;
    };
    if items.len() <= 24 {
        return None;
    }

    let mut selected = BTreeSet::new();
    selected.extend(0..items.len().min(4));
    selected.extend(items.len().saturating_sub(4)..items.len());
    for (idx, item) in items.iter().enumerate() {
        if selected.len() >= 20 {
            break;
        }
        if value_has_error_signal(item) {
            selected.insert(idx);
        }
    }

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
            "sampled_items": sampled_items,
        }
    });

    Some(StrategyOutput {
        strategy: InputSlimmingStrategy::JsonArraySample,
        body: serde_json::to_string_pretty(&body).ok()?,
    })
}

fn value_has_error_signal(value: &Value) -> bool {
    let lower = value.to_string().to_lowercase();
    ["error", "warn", "fail", "panic", "exception"]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn search_result_compact(text: &str) -> Option<StrategyOutput> {
    let mut groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut matched = 0usize;
    for line in text.lines() {
        if let Some(path) = search_result_path(line) {
            groups.entry(path.to_string()).or_default().push(line);
            matched += 1;
        }
    }
    if matched < 3 {
        return None;
    }

    let mut kept_hits = 0usize;
    let mut body = format!(
        "Input Slimming search result summary: original_lines={}, files={}, kept_files={}\n",
        text.lines().count(),
        groups.len(),
        groups.len().min(40),
    );
    for (path, hits) in groups.iter().take(40) {
        body.push_str(&format!(
            "\n# {path} ({} hits, showing up to 5)\n",
            hits.len()
        ));
        for hit in hits.iter().take(5) {
            kept_hits += 1;
            body.push_str(hit);
            body.push('\n');
        }
    }
    body.push_str(&format!(
        "\nOmitted {} hits. Retrieve the original for full results.",
        matched.saturating_sub(kept_hits)
    ));

    Some(StrategyOutput {
        strategy: InputSlimmingStrategy::SearchResultCompact,
        body,
    })
}

fn search_result_path(line: &str) -> Option<&str> {
    let mut parts = line.splitn(4, ':');
    let path = parts.next()?;
    let line_number = parts.next()?;
    if path.is_empty() || line_number.is_empty() || !line_number.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(path)
}

fn looks_like_log(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "error",
        "warning",
        "failed",
        "panic",
        "traceback",
        "stack backtrace",
        "compiling",
        "finished",
        "test result",
        "exit code",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn log_compact(text: &str) -> StrategyOutput {
    let lines = text.lines().collect::<Vec<_>>();
    let mut selected = BTreeSet::new();
    selected.extend(0..lines.len().min(40));
    selected.extend(lines.len().saturating_sub(80)..lines.len());

    for (idx, line) in lines.iter().enumerate() {
        if line_is_important_log(line) {
            let start = idx.saturating_sub(2);
            let end = (idx + 3).min(lines.len());
            selected.extend(start..end);
        }
    }

    let mut body = format!(
        "Input Slimming log summary: original_lines={}, kept_lines={}\n",
        lines.len(),
        selected.len()
    );
    append_selected_lines(&mut body, &lines, selected);
    StrategyOutput {
        strategy: InputSlimmingStrategy::LogCompact,
        body,
    }
}

fn line_is_important_log(line: &str) -> bool {
    let lower = line.to_lowercase();
    [
        "error",
        "warning",
        "failed",
        "panic",
        "exception",
        "traceback",
        "stack backtrace",
        "caused by",
        "test result",
        "exit code",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn diff_compact(text: &str) -> Option<StrategyOutput> {
    if approx_token_count(text) < 2_048
        || text.contains("GIT binary patch")
        || text.contains("Binary files")
    {
        return None;
    }
    let has_diff_signal = text.contains("diff --git")
        || (text.contains("--- ") && text.contains("+++ ") && text.contains("@@"));
    if !has_diff_signal {
        return None;
    }

    let lines = text.lines().collect::<Vec<_>>();
    let mut selected = BTreeSet::new();
    let mut current_hunk_changed = Vec::new();
    let mut saw_hunk = false;

    for (idx, line) in lines.iter().enumerate() {
        if line.starts_with("diff --git ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            selected.insert(idx);
        }
        if line.starts_with("@@") {
            flush_hunk_changed(&mut selected, &current_hunk_changed);
            current_hunk_changed.clear();
            saw_hunk = true;
            selected.insert(idx);
        } else if (line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("+++")
            && !line.starts_with("---")
        {
            current_hunk_changed.push(idx);
        }
    }
    flush_hunk_changed(&mut selected, &current_hunk_changed);

    if !saw_hunk {
        return None;
    }

    let mut body = format!(
        "Input Slimming diff summary: original_lines={}, kept_lines={}\n",
        lines.len(),
        selected.len()
    );
    append_selected_lines(&mut body, &lines, selected);
    Some(StrategyOutput {
        strategy: InputSlimmingStrategy::DiffCompact,
        body,
    })
}

fn flush_hunk_changed(selected: &mut BTreeSet<usize>, indices: &[usize]) {
    for idx in indices.iter().take(6) {
        selected.insert(*idx);
    }
    for idx in indices.iter().rev().take(6) {
        selected.insert(*idx);
    }
}

fn plain_text_head_tail(text: &str, options: InputSlimmingOptions) -> Option<StrategyOutput> {
    let truncated = truncate_text(text, TruncationPolicy::Tokens(options.target_tokens));
    if truncated == text {
        return None;
    }
    Some(StrategyOutput {
        strategy: InputSlimmingStrategy::PlainTextHeadTail,
        body: truncated,
    })
}

fn append_selected_lines(body: &mut String, lines: &[&str], selected: BTreeSet<usize>) {
    let mut last = None;
    for idx in selected {
        if let Some(prev) = last
            && idx > prev + 1
        {
            body.push_str(&format!("... omitted {} lines ...\n", idx - prev - 1));
        }
        last = Some(idx);
        body.push_str(lines[idx]);
        body.push('\n');
    }
    if let Some(last_idx) = last
        && last_idx + 1 < lines.len()
    {
        body.push_str(&format!(
            "... omitted {} lines ...\n",
            lines.len() - last_idx - 1
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn json_array_strategy_preserves_first_last_and_error_rows() {
        let items = (0..30)
            .map(|idx| {
                if idx == 10 {
                    json!({"id": idx, "status": "error", "message": "failed"})
                } else {
                    json!({"id": idx, "status": "ok"})
                }
            })
            .collect::<Vec<_>>();
        let text = serde_json::to_string(&items).expect("json");

        let output = json_array_sample(&text).expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::JsonArraySample);
        assert!(output.body.contains("\"original_items\": 30"));
        assert!(output.body.contains("\"id\": 0"));
        assert!(output.body.contains("\"id\": 29"));
        assert!(output.body.contains("\"id\": 10"));
    }

    #[test]
    fn json_array_strategy_skips_small_arrays() {
        let text = serde_json::to_string(&vec![json!({"id": 1}); 24]).expect("json");
        assert_eq!(json_array_sample(&text), None);
    }

    #[test]
    fn search_strategy_groups_by_file_and_caps_hits() {
        let text = (0..10)
            .map(|idx| format!("src/main.rs:{idx}:match {idx}"))
            .collect::<Vec<_>>()
            .join("\n");

        let output = search_result_compact(&text).expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::SearchResultCompact);
        assert!(output.body.contains("src/main.rs:0:match 0"));
        assert!(output.body.contains("Omitted 5 hits"));
        assert!(!output.body.contains("src/main.rs:9:match 9"));
    }

    #[test]
    fn log_strategy_preserves_errors_and_tail() {
        let mut lines = (0..150)
            .map(|idx| format!("line {idx}"))
            .collect::<Vec<_>>();
        lines[75] = "ERROR: failed".to_string();
        let text = lines.join("\n");

        let output = log_compact(&text);

        assert_eq!(output.strategy, InputSlimmingStrategy::LogCompact);
        assert!(output.body.contains("ERROR: failed"));
        assert!(output.body.contains("line 149"));
    }

    #[test]
    fn failed_non_log_has_no_strategy() {
        assert_eq!(
            slim_text(
                "x".repeat(5_000).as_str(),
                Some(false),
                InputSlimmingOptions::default()
            ),
            None
        );
    }

    #[test]
    fn diff_strategy_preserves_headers_and_hunks() {
        let mut text = String::from("diff --git a/a b/a\n--- a/a\n+++ b/a\n");
        for hunk in 0..400 {
            text.push_str(&format!("@@ -{hunk},1 +{hunk},1 @@\n"));
            text.push_str("-old line\n+new line\n context\n");
        }

        let output = diff_compact(&text).expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::DiffCompact);
        assert!(output.body.contains("diff --git a/a b/a"));
        assert!(output.body.contains("@@ -0,1 +0,1 @@"));
    }

    #[test]
    fn plain_text_strategy_emits_head_tail_body() {
        let text = "abcdef".repeat(2_000);

        let output =
            plain_text_head_tail(&text, InputSlimmingOptions::default()).expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::PlainTextHeadTail);
        assert!(output.body.contains("truncated"));
    }
}
