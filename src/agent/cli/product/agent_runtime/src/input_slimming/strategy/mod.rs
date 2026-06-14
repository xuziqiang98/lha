mod diff;
mod json;
mod log;
mod plain;
mod search;

use std::collections::BTreeSet;

use crate::product::agent::input_slimming::InputSlimmingOptions;
use crate::product::agent::input_slimming::InputSlimmingStrategy;

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
    slim_text_with_preference(text, success, options, None)
}

pub(super) fn slim_text_for_tool(
    text: &str,
    success: Option<bool>,
    options: InputSlimmingOptions,
    tool_name: &str,
) -> Option<StrategyOutput> {
    slim_text_with_preference(text, success, options, strategy_preference(tool_name))
}

fn slim_text_with_preference(
    text: &str,
    success: Option<bool>,
    options: InputSlimmingOptions,
    preference: Option<StrategyPreference>,
) -> Option<StrategyOutput> {
    if let Some(output) = preferred_strategy(text, preference) {
        return Some(output);
    }
    if let Some(output) = json::json_array_sample(text) {
        return Some(output);
    }
    if let Some(output) = diff::diff_compact(text) {
        return Some(output);
    }
    if let Some(output) = search::search_result_compact(text) {
        return Some(output);
    }
    if log::looks_like_log(text) {
        return Some(log::log_compact(text));
    }
    if success == Some(false) {
        return None;
    }
    plain::plain_text_head_tail(text, options)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrategyPreference {
    Log,
    Search,
    Diff,
}

fn strategy_preference(tool_name: &str) -> Option<StrategyPreference> {
    let lower_tool = tool_name.to_lowercase();
    if lower_tool.contains("rg") || lower_tool.contains("grep") || lower_tool.contains("search") {
        Some(StrategyPreference::Search)
    } else if lower_tool.contains("diff") || lower_tool.contains("apply_patch") {
        Some(StrategyPreference::Diff)
    } else if lower_tool.contains("shell")
        || lower_tool.contains("unified_exec")
        || lower_tool.contains("cargo")
        || lower_tool.contains("test")
    {
        Some(StrategyPreference::Log)
    } else {
        None
    }
}

fn preferred_strategy(
    text: &str,
    preference: Option<StrategyPreference>,
) -> Option<StrategyOutput> {
    match preference {
        Some(StrategyPreference::Log) => log::looks_like_log(text).then(|| log::log_compact(text)),
        Some(StrategyPreference::Search) => search::search_result_compact(text),
        Some(StrategyPreference::Diff) => diff::diff_compact(text),
        None => None,
    }
}

pub(super) fn append_selected_lines(body: &mut String, lines: &[&str], selected: BTreeSet<usize>) {
    append_selected_lines_with_options(body, lines, selected, false);
}

pub(super) fn append_selected_lines_collapsed(
    body: &mut String,
    lines: &[&str],
    selected: BTreeSet<usize>,
) {
    append_selected_lines_with_options(body, lines, selected, true);
}

fn append_selected_lines_with_options(
    body: &mut String,
    lines: &[&str],
    selected: BTreeSet<usize>,
    collapse_repeats: bool,
) {
    let mut last = None;
    let mut repeated_line: Option<&str> = None;
    let mut repeat_count = 0usize;

    for idx in selected {
        if let Some(prev) = last
            && idx > prev + 1
        {
            flush_repeat(body, &mut repeated_line, &mut repeat_count);
            body.push_str(&format!("... omitted {} lines ...\n", idx - prev - 1));
        }
        last = Some(idx);

        if collapse_repeats {
            match repeated_line {
                Some(line) if line == lines[idx] => {
                    repeat_count += 1;
                }
                Some(_) | None => {
                    flush_repeat(body, &mut repeated_line, &mut repeat_count);
                    repeated_line = Some(lines[idx]);
                    repeat_count = 1;
                }
            }
        } else {
            body.push_str(lines[idx]);
            body.push('\n');
        }
    }

    flush_repeat(body, &mut repeated_line, &mut repeat_count);

    if let Some(last_idx) = last
        && last_idx + 1 < lines.len()
    {
        body.push_str(&format!(
            "... omitted {} lines ...\n",
            lines.len() - last_idx - 1
        ));
    }
}

fn flush_repeat(body: &mut String, repeated_line: &mut Option<&str>, repeat_count: &mut usize) {
    let Some(line) = repeated_line.take() else {
        return;
    };
    if *repeat_count > 1 {
        body.push_str(&format!("{line} [repeated {} times]\n", *repeat_count));
    } else {
        body.push_str(line);
        body.push('\n');
    }
    *repeat_count = 0;
}

#[cfg(test)]
pub(super) fn assert_strategy_retains_needles(
    original: &str,
    compressed: &str,
    required_needles: &[&str],
) {
    assert!(
        compressed.len() < original.len(),
        "strategy output should be smaller than original: original={} compressed={}",
        original.len(),
        compressed.len()
    );
    assert!(
        compressed.to_lowercase().contains("omitted"),
        "strategy output should report omitted content: {compressed}"
    );
    for needle in required_needles {
        assert!(
            compressed.contains(needle),
            "strategy output missing required needle `{needle}`"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn tool_preference_routes_search_like_tools_to_search_strategy() {
        let text = (0..10)
            .map(|idx| format!("src/main.rs:{idx}:match {idx}"))
            .collect::<Vec<_>>()
            .join("\n");

        let output = slim_text_for_tool(&text, None, InputSlimmingOptions::default(), "rg")
            .expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::SearchResultCompact);
    }

    #[test]
    fn tool_preference_routes_shell_logs_to_log_strategy() {
        let text = (0..120)
            .map(|idx| {
                if idx == 60 {
                    "error: failed".to_string()
                } else {
                    format!("line {idx}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let output =
            slim_text_for_tool(&text, None, InputSlimmingOptions::default(), "unified_exec")
                .expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::LogCompact);
    }
}
