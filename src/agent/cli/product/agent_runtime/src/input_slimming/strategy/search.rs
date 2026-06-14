use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::StrategyOutput;
use crate::product::agent::input_slimming::InputSlimmingStrategy;

const MAX_FILES: usize = 40;
const MAX_HITS_PER_FILE: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchMatch<'a> {
    original_index: usize,
    path: &'a str,
    line_number: usize,
    text: &'a str,
    score: i32,
}

#[derive(Debug, Clone)]
struct FileMatches<'a> {
    path: &'a str,
    first_index: usize,
    matches: Vec<SearchMatch<'a>>,
    score: i32,
}

pub(super) fn search_result_compact(text: &str) -> Option<StrategyOutput> {
    // TODO(input-slimming): pass user query/context keywords through the runtime when
    // there is an internal request-level signal to rank search hits against.
    search_result_compact_with_context(text, &[])
}

fn search_result_compact_with_context(
    text: &str,
    context_keywords: &[&str],
) -> Option<StrategyOutput> {
    let mut groups: BTreeMap<&str, FileMatches<'_>> = BTreeMap::new();
    for (line_index, line) in text.lines().enumerate() {
        let Some(mut parsed) = parse_search_match(line_index, line) else {
            continue;
        };
        parsed.score = score_match(&parsed, context_keywords);
        let entry = groups.entry(parsed.path).or_insert_with(|| FileMatches {
            path: parsed.path,
            first_index: line_index,
            matches: Vec::new(),
            score: 0,
        });
        entry.score += parsed.score;
        entry.matches.push(parsed);
    }
    let matched = groups
        .values()
        .map(|group| group.matches.len())
        .sum::<usize>();
    if matched < 3 {
        return None;
    }

    let mut ranked_files = groups.into_values().collect::<Vec<_>>();
    for group in &mut ranked_files {
        group.matches.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.original_index.cmp(&right.original_index))
        });
    }
    ranked_files.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.first_index.cmp(&right.first_index))
    });

    let shown_files = ranked_files.len().min(MAX_FILES);
    let mut kept_hits = 0usize;
    let mut body = format!(
        "Input Slimming search result summary: original_lines={}, files={}, kept_files={}, omitted_files={}\n",
        text.lines().count(),
        ranked_files.len(),
        shown_files,
        ranked_files.len().saturating_sub(shown_files),
    );
    for group in ranked_files.iter().take(MAX_FILES) {
        body.push_str(&format!(
            "\n# {} ({} hits, showing up to {})\n",
            group.path,
            group.matches.len(),
            MAX_HITS_PER_FILE
        ));
        for hit in select_matches(group) {
            kept_hits += 1;
            body.push_str(&format!("{}:{}:{}\n", hit.path, hit.line_number, hit.text));
        }
        if group.matches.len() > MAX_HITS_PER_FILE {
            body.push_str(&format!(
                "... omitted {} hits in {} ...\n",
                group.matches.len() - MAX_HITS_PER_FILE,
                group.path
            ));
        }
    }
    if ranked_files.len() > MAX_FILES {
        body.push_str(&format!(
            "\nOmitted {} files. Retrieve the original for full results.\n",
            ranked_files.len() - MAX_FILES
        ));
    }
    body.push_str(&format!(
        "\nOmitted {} hits total. Retrieve the original for full results.",
        matched.saturating_sub(kept_hits)
    ));

    Some(StrategyOutput {
        strategy: InputSlimmingStrategy::SearchResultCompact,
        body,
    })
}

fn select_matches<'a>(group: &'a FileMatches<'a>) -> Vec<&'a SearchMatch<'a>> {
    let mut selected = BTreeSet::new();
    if let Some(first) = group.matches.iter().min_by_key(|hit| hit.original_index) {
        selected.insert(first.original_index);
    }
    if let Some(last) = group.matches.iter().max_by_key(|hit| hit.original_index) {
        selected.insert(last.original_index);
    }
    for hit in &group.matches {
        if selected.len() >= MAX_HITS_PER_FILE {
            break;
        }
        selected.insert(hit.original_index);
    }
    let by_index = group
        .matches
        .iter()
        .map(|hit| (hit.original_index, hit))
        .collect::<BTreeMap<_, _>>();
    selected
        .into_iter()
        .filter_map(|idx| by_index.get(&idx).copied())
        .collect()
}

fn parse_search_match(line_index: usize, line: &str) -> Option<SearchMatch<'_>> {
    let start = if has_windows_drive_prefix(line) { 2 } else { 0 };
    let bytes = line.as_bytes();
    for idx in start..bytes.len() {
        let sep = bytes[idx] as char;
        if sep != ':' && sep != '-' {
            continue;
        }
        let number_start = idx + 1;
        let mut number_end = number_start;
        while number_end < bytes.len() && bytes[number_end].is_ascii_digit() {
            number_end += 1;
        }
        if number_end == number_start || number_end >= bytes.len() {
            continue;
        }
        let next_sep = bytes[number_end] as char;
        if next_sep != sep {
            continue;
        }

        let mut text_start = number_end + 1;
        if sep == ':' {
            let column_start = text_start;
            let mut column_end = column_start;
            while column_end < bytes.len() && bytes[column_end].is_ascii_digit() {
                column_end += 1;
            }
            if column_end > column_start
                && column_end < bytes.len()
                && bytes[column_end] as char == ':'
            {
                text_start = column_end + 1;
            }
        }

        let path = &line[..idx];
        let text = &line[text_start..];
        if path.is_empty() || text.is_empty() {
            return None;
        }
        let line_number = line[number_start..number_end].parse().ok()?;
        return Some(SearchMatch {
            original_index: line_index,
            path,
            line_number,
            text,
            score: 0,
        });
    }
    None
}

fn has_windows_drive_prefix(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn score_match(hit: &SearchMatch<'_>, context_keywords: &[&str]) -> i32 {
    let lower = hit.text.to_lowercase();
    let mut score = 1;
    if ["error", "warn", "fail", "panic", "exception", "fatal"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        score += 100;
    }
    for keyword in context_keywords {
        if lower.contains(&keyword.to_lowercase()) {
            score += 30;
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::input_slimming::strategy::assert_strategy_retains_needles;
    use pretty_assertions::assert_eq;

    #[test]
    fn search_strategy_groups_by_file_and_caps_hits() {
        let text = (0..220)
            .map(|idx| format!("src/main.rs:{idx}:match {idx} {}", "x".repeat(80)))
            .collect::<Vec<_>>()
            .join("\n");

        let output = search_result_compact(&text).expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::SearchResultCompact);
        assert_strategy_retains_needles(
            &text,
            &output.body,
            &[
                "src/main.rs:0:match 0",
                "src/main.rs:219:match 219",
                "omitted 215 hits in src/main.rs",
                "Omitted 215 hits total",
            ],
        );
    }

    #[test]
    fn search_parser_handles_windows_paths_dash_filenames_and_context_lines() {
        let text = [
            r"C:\repo\pre-commit-config.yaml:42:error needle",
            r"C:\repo\pre-commit-config.yaml:43:normal",
            "src/pre-commit-config.yaml-7-warning dash needle",
            "src/pre-commit-config.yaml-8-context",
            "src/lib.rs:10:5:panic column needle",
            "src/lib.rs:11:normal",
        ]
        .repeat(20)
        .join("\n");

        let output = search_result_compact(&text).expect("strategy output");

        assert_strategy_retains_needles(
            &text,
            &output.body,
            &[
                r"C:\repo\pre-commit-config.yaml:42:error needle",
                "src/pre-commit-config.yaml:7:warning dash needle",
                "src/lib.rs:10:panic column needle",
            ],
        );
    }

    #[test]
    fn search_strategy_ranks_high_priority_files_before_bulk_matches() {
        let mut lines = (0..80)
            .map(|idx| format!("bulk/file_{idx}.rs:1:ordinary match"))
            .collect::<Vec<_>>();
        lines.push("critical.rs:99:fatal needle".to_string());
        lines.push("critical.rs:100:tail".to_string());
        let text = lines.join("\n");

        let output = search_result_compact(&text).expect("strategy output");

        assert!(output.body.contains("critical.rs:99:fatal needle"));
        assert!(output.body.contains("Omitted 41 files"));
    }
}
