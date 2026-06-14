use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::StrategyOutput;
use super::append_selected_lines_collapsed;
use crate::product::agent::input_slimming::InputSlimmingStrategy;

const HEAD_LINES: usize = 32;
const TAIL_LINES: usize = 64;
const ERROR_CONTEXT_LINES: usize = 3;
const MAX_WARNING_GROUPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogFormat {
    Pytest,
    Cargo,
    Npm,
    Jest,
    Make,
    Generic,
}

impl LogFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pytest => "pytest",
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Jest => "jest",
            Self::Make => "make",
            Self::Generic => "generic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogLevel {
    Error,
    Fail,
    Warn,
    Info,
    Debug,
    Trace,
    Unknown,
}

#[derive(Debug, Clone)]
struct LogLine<'a> {
    index: usize,
    text: &'a str,
    level: LogLevel,
    is_stack_trace: bool,
    is_summary: bool,
    score: i32,
}

pub(super) fn looks_like_log(text: &str) -> bool {
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
        "npm err!",
        "make: ***",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(super) fn log_compact(text: &str) -> StrategyOutput {
    let lines = text.lines().collect::<Vec<_>>();
    let format = detect_format(&lines);
    let parsed = parse_lines(&lines);
    let warning_representatives = warning_representatives(&parsed);
    let warnings_deduped = parsed
        .iter()
        .filter(|line| line.level == LogLevel::Warn)
        .count()
        .saturating_sub(warning_representatives.len());

    let mut selected = BTreeSet::new();
    selected.extend(0..lines.len().min(HEAD_LINES));
    selected.extend(lines.len().saturating_sub(TAIL_LINES)..lines.len());
    for line in &parsed {
        if line.score > 0 || line.is_stack_trace || line.is_summary {
            let context = if matches!(line.level, LogLevel::Error | LogLevel::Fail) {
                ERROR_CONTEXT_LINES
            } else {
                1
            };
            let start = line.index.saturating_sub(context);
            let end = (line.index + context + 1).min(lines.len());
            selected.extend(start..end);
        }
    }
    selected.extend(warning_representatives);

    let mut body = format!(
        "Input Slimming log summary: format_detected={}, original_lines={}, kept_lines={}, omitted_lines={}, warnings_deduped={}\n",
        format.as_str(),
        lines.len(),
        selected.len(),
        lines.len().saturating_sub(selected.len()),
        warnings_deduped
    );
    append_selected_lines_collapsed(&mut body, &lines, selected);
    StrategyOutput {
        strategy: InputSlimmingStrategy::LogCompact,
        body,
    }
}

fn detect_format(lines: &[&str]) -> LogFormat {
    let joined = lines.join("\n").to_lowercase();
    if joined.contains("short test summary info")
        || joined.contains("traceback (most recent call last)")
        || joined.contains("= failures =")
    {
        LogFormat::Pytest
    } else if joined.contains("error[e")
        || joined.contains("compiling ")
        || joined.contains("test result:")
        || joined.contains("stack backtrace:")
    {
        LogFormat::Cargo
    } else if joined.contains("npm err!") {
        LogFormat::Npm
    } else if joined.contains("\nfail ") && joined.contains("\n    at ") {
        LogFormat::Jest
    } else if joined.contains("make: ***") || joined.contains("entering directory") {
        LogFormat::Make
    } else {
        LogFormat::Generic
    }
}

fn parse_lines<'a>(lines: &'a [&'a str]) -> Vec<LogLine<'a>> {
    let mut parsed = Vec::with_capacity(lines.len());
    let mut python_trace_remaining_blank_lines = 0usize;
    let mut rust_backtrace = false;
    let mut js_stack = false;

    for (idx, text) in lines.iter().enumerate() {
        let lower = text.to_lowercase();
        let level = classify_level(&lower);
        let is_summary = is_summary_line(&lower);

        if lower.contains("traceback (most recent call last)") {
            python_trace_remaining_blank_lines = 2;
        }
        if lower.contains("stack backtrace:") {
            rust_backtrace = true;
        }
        if text.trim_start().starts_with("at ") {
            js_stack = true;
        }

        let is_python_trace = python_trace_remaining_blank_lines > 0
            || text.trim_start().starts_with("File \"")
            || lower.contains("during handling of the above exception")
            || lower.contains("the above exception was the direct cause");
        let is_rust_trace = rust_backtrace
            && (text
                .trim_start()
                .starts_with(|ch: char| ch.is_ascii_digit())
                || lower.contains("stack backtrace:")
                || text.trim().is_empty());
        let is_js_trace = js_stack
            && (text.trim_start().starts_with("at ")
                || lower.contains("error:")
                || lower.contains("typeerror:")
                || lower.contains("referenceerror:"));
        let is_stack_trace = is_python_trace || is_rust_trace || is_js_trace;

        if python_trace_remaining_blank_lines > 0 {
            if text.trim().is_empty() {
                python_trace_remaining_blank_lines -= 1;
            } else if !is_python_trace && !text.starts_with(char::is_whitespace) {
                python_trace_remaining_blank_lines = 0;
            }
        }
        if rust_backtrace && !is_rust_trace && !text.trim().is_empty() {
            rust_backtrace = false;
        }
        if js_stack && !is_js_trace && !text.trim().is_empty() {
            js_stack = false;
        }

        let mut score = 0;
        score += match level {
            LogLevel::Error | LogLevel::Fail => 100,
            LogLevel::Warn => 60,
            LogLevel::Trace => 40,
            LogLevel::Info | LogLevel::Debug | LogLevel::Unknown => 0,
        };
        if is_stack_trace {
            score += 80;
        }
        if is_summary {
            score += 70;
        }

        parsed.push(LogLine {
            index: idx,
            text,
            level,
            is_stack_trace,
            is_summary,
            score,
        });
    }

    parsed
}

fn classify_level(lower: &str) -> LogLevel {
    if contains_word(lower, &["fatal", "critical", "error", "exception", "panic"]) {
        LogLevel::Error
    } else if contains_word(lower, &["failed", "fail"]) {
        LogLevel::Fail
    } else if contains_word(lower, &["warning", "warn"]) {
        LogLevel::Warn
    } else if contains_word(lower, &["trace", "backtrace"]) {
        LogLevel::Trace
    } else if contains_word(lower, &["debug"]) {
        LogLevel::Debug
    } else if contains_word(lower, &["info", "compiling", "finished"]) {
        LogLevel::Info
    } else {
        LogLevel::Unknown
    }
}

fn contains_word(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn is_summary_line(lower: &str) -> bool {
    lower.starts_with("====")
        || lower.starts_with("----")
        || lower.contains("test result:")
        || lower.contains("short test summary info")
        || lower.contains("failures:")
        || lower.contains("tests:")
        || lower.contains("suites:")
        || lower.contains("exit code")
        || lower.contains("finished ")
}

fn warning_representatives(lines: &[LogLine<'_>]) -> BTreeSet<usize> {
    let mut by_signature: BTreeMap<String, usize> = BTreeMap::new();
    for line in lines {
        if line.level != LogLevel::Warn {
            continue;
        }
        by_signature
            .entry(warning_signature(line.text))
            .or_insert(line.index);
    }
    by_signature
        .into_values()
        .take(MAX_WARNING_GROUPS)
        .collect()
}

fn warning_signature(text: &str) -> String {
    let mut split_at = text.len();
    for marker in [": ", " = ", " at ", " in "] {
        if let Some(idx) = text.find(marker) {
            split_at = split_at.min(idx + marker.len());
        }
    }
    let (prefix, suffix) = text.split_at(split_at);
    let normalized_suffix = suffix
        .chars()
        .map(|ch| {
            if ch.is_ascii_digit() || ch == '/' || ch == '\\' || ch == ':' {
                '#'
            } else {
                ch
            }
        })
        .collect::<String>();
    format!("{prefix}{normalized_suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::input_slimming::strategy::assert_strategy_retains_needles;
    use pretty_assertions::assert_eq;

    #[test]
    fn log_strategy_preserves_errors_tail_and_collapses_repeats() {
        let mut lines = (0..160)
            .map(|idx| format!("line {idx}"))
            .collect::<Vec<_>>();
        lines.splice(
            45..70,
            std::iter::repeat_n("warning: same path /tmp/a123".to_string(), 25),
        );
        lines[95] = "ERROR: failed".to_string();
        lines[96] = "stack backtrace:".to_string();
        lines[97] = "   0: crate::thing".to_string();
        lines[159] = "exit code: 101".to_string();
        let text = lines.join("\n");

        let output = log_compact(&text);

        assert_eq!(output.strategy, InputSlimmingStrategy::LogCompact);
        assert_strategy_retains_needles(
            &text,
            &output.body,
            &[
                "format_detected=cargo",
                "ERROR: failed",
                "stack backtrace:",
                "crate::thing",
                "exit code: 101",
                "warnings_deduped=",
            ],
        );
    }

    #[test]
    fn log_strategy_keeps_chained_python_traceback_after_blank_line() {
        let mut lines = (0..100)
            .map(|idx| format!("noise {idx}"))
            .collect::<Vec<_>>();
        lines.splice(
            40..40,
            [
                "Traceback (most recent call last):",
                "  File \"app.py\", line 1, in <module>",
                "    run()",
                "",
                "ValueError: first failure",
                "",
                "The above exception was the direct cause of the following exception:",
                "  File \"worker.py\", line 9, in main",
                "    raise RuntimeError('needle chained')",
                "RuntimeError: needle chained",
            ]
            .into_iter()
            .map(str::to_string),
        );
        let text = lines.join("\n");

        let output = log_compact(&text);

        assert!(output.body.contains("format_detected=pytest"));
        assert!(output.body.contains("needle chained"));
        assert!(output.body.contains("worker.py"));
    }

    #[test]
    fn log_strategy_keeps_jest_stack_frames_and_distinct_warnings() {
        let text = [
            "PASS one.test.js",
            "warning: timeout at src/a.js:10",
            "warning: timeout at src/b.js:20",
            "FAIL two.test.js",
            "TypeError: cannot read property",
            "    at render (/repo/ui.tsx:42:10)",
            "    at Object.<anonymous> (/repo/ui.test.tsx:5:1)",
            "Tests: 1 failed, 1 passed",
        ]
        .repeat(40)
        .join("\n");

        let output = log_compact(&text);

        assert!(output.body.contains("format_detected=jest"));
        assert!(output.body.contains("TypeError: cannot read property"));
        assert!(output.body.contains("ui.tsx:42"));
        assert!(output.body.contains("Tests: 1 failed"));
    }
}
