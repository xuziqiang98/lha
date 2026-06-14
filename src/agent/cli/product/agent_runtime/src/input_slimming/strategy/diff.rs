use std::collections::BTreeSet;

use super::StrategyOutput;
use crate::product::agent::input_slimming::InputSlimmingStrategy;
use crate::product::agent::truncate::approx_token_count;

const CHANGED_LINES_PER_HUNK_EDGE: usize = 4;
const MAX_KEYWORD_LINES_PER_HUNK: usize = 8;
const MAX_RENDERED_BLOCKS: usize = 120;

#[derive(Debug, Clone, Default)]
struct DiffFile {
    header_indices: Vec<usize>,
    hunks: Vec<DiffHunk>,
    is_binary: bool,
}

#[derive(Debug, Clone)]
struct DiffHunk {
    header_index: usize,
    changed_lines: Vec<ChangedLine>,
}

#[derive(Debug, Clone)]
struct ChangedLine {
    index: usize,
    is_critical: bool,
}

pub(super) fn diff_compact(text: &str) -> Option<StrategyOutput> {
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
    let files = parse_diff_files(&lines);
    if files.is_empty() || files.iter().all(|file| file.hunks.is_empty()) {
        return None;
    }
    if files.iter().any(|file| file.is_binary) {
        return None;
    }

    let mut selected = BTreeSet::new();
    let mut hunks = 0usize;
    let mut changed_lines = 0usize;
    for file in &files {
        selected.extend(file.header_indices.iter().copied());
        for hunk in &file.hunks {
            hunks += 1;
            changed_lines += hunk.changed_lines.len();
            selected.insert(hunk.header_index);
            select_hunk_lines(&mut selected, hunk);
        }
    }

    let mut body = format!(
        "Input Slimming diff summary: original_lines={}, files={}, hunks={}, changed_lines={}, kept_lines={}, omitted_lines={}\n",
        lines.len(),
        files.len(),
        hunks,
        changed_lines,
        selected.len(),
        lines.len().saturating_sub(selected.len())
    );
    append_diff_selected_lines(&mut body, &lines, &selected);
    Some(StrategyOutput {
        strategy: InputSlimmingStrategy::DiffCompact,
        body,
    })
}

fn parse_diff_files(lines: &[&str]) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut current_file: Option<DiffFile> = None;
    let mut current_hunk: Option<DiffHunk> = None;

    for (idx, line) in lines.iter().enumerate() {
        if line.starts_with("diff --git ") {
            flush_hunk(&mut current_file, &mut current_hunk);
            if let Some(file) = current_file.take() {
                files.push(file);
            }
            current_file = Some(DiffFile {
                header_indices: vec![idx],
                hunks: Vec::new(),
                is_binary: false,
            });
            continue;
        }

        let file = current_file.get_or_insert_with(DiffFile::default);
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("index ")
            || line.starts_with("rename ")
            || line.starts_with("new file mode ")
            || line.starts_with("deleted file mode ")
        {
            file.header_indices.push(idx);
            continue;
        }
        if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
            file.is_binary = true;
            continue;
        }
        if line.starts_with("@@") {
            flush_hunk(&mut current_file, &mut current_hunk);
            current_hunk = Some(DiffHunk {
                header_index: idx,
                changed_lines: Vec::new(),
            });
            continue;
        }
        if let Some(hunk) = current_hunk.as_mut()
            && (line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("+++")
            && !line.starts_with("---")
        {
            hunk.changed_lines.push(ChangedLine {
                index: idx,
                is_critical: is_critical_diff_line(line),
            });
        }
    }

    flush_hunk(&mut current_file, &mut current_hunk);
    if let Some(file) = current_file {
        files.push(file);
    }
    files
}

fn flush_hunk(file: &mut Option<DiffFile>, hunk: &mut Option<DiffHunk>) {
    let Some(hunk) = hunk.take() else {
        return;
    };
    file.get_or_insert_with(DiffFile::default).hunks.push(hunk);
}

fn select_hunk_lines(selected: &mut BTreeSet<usize>, hunk: &DiffHunk) {
    for changed in hunk.changed_lines.iter().take(CHANGED_LINES_PER_HUNK_EDGE) {
        selected.insert(changed.index);
    }
    for changed in hunk
        .changed_lines
        .iter()
        .rev()
        .take(CHANGED_LINES_PER_HUNK_EDGE)
    {
        selected.insert(changed.index);
    }
    for changed in hunk
        .changed_lines
        .iter()
        .filter(|changed| changed.is_critical)
        .take(MAX_KEYWORD_LINES_PER_HUNK)
    {
        selected.insert(changed.index);
    }
}

fn is_critical_diff_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    [
        "error", "unsafe", "security", "panic", "unwrap", "todo!", "fn ", "class ", "test", "auth",
        "password", "token",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn append_diff_selected_lines(body: &mut String, lines: &[&str], selected: &BTreeSet<usize>) {
    let mut last = None;
    for (rendered_blocks, idx) in selected.iter().copied().enumerate() {
        if rendered_blocks >= MAX_RENDERED_BLOCKS {
            body.push_str(&format!(
                "... omitted {} selected blocks ...\n",
                selected.len().saturating_sub(rendered_blocks)
            ));
            break;
        }
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
    use crate::product::agent::input_slimming::strategy::assert_strategy_retains_needles;
    use pretty_assertions::assert_eq;

    #[test]
    fn diff_strategy_preserves_headers_hunks_and_counts() {
        let mut text = String::from("diff --git a/a b/a\n--- a/a\n+++ b/a\n");
        for hunk in 0..400 {
            text.push_str(&format!("@@ -{hunk},1 +{hunk},1 @@\n"));
            text.push_str("-old line\n+new line\n context\n");
        }

        let output = diff_compact(&text).expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::DiffCompact);
        assert_strategy_retains_needles(
            &text,
            &output.body,
            &[
                "files=1",
                "hunks=400",
                "changed_lines=800",
                "diff --git a/a b/a",
                "@@ -0,1 +0,1 @@",
                "omitted",
            ],
        );
    }

    #[test]
    fn diff_strategy_keeps_multifile_headers_and_critical_middle_lines() {
        let mut text = String::new();
        text.push_str("diff --git a/a.rs b/a.rs\nindex 1..2\n--- a/a.rs\n+++ b/a.rs\n");
        text.push_str("@@ -1,200 +1,200 @@\n");
        for idx in 0..3_000 {
            if idx == 80 {
                text.push_str(
                    "+fn security_critical_token_check() { unsafe { panic!(\"needle\") } }\n",
                );
            } else {
                text.push_str(&format!("+ordinary changed line {idx}\n"));
            }
        }
        text.push_str("diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n");
        text.push_str("@@ -1,2 +1,2 @@\n-old\n+new\n");

        let output = diff_compact(&text).expect("strategy output");

        assert_strategy_retains_needles(
            &text,
            &output.body,
            &[
                "diff --git a/a.rs b/a.rs",
                "diff --git a/b.rs b/b.rs",
                "security_critical_token_check",
            ],
        );
    }

    #[test]
    fn diff_strategy_skips_binary_and_malformed_patches() {
        let binary = format!(
            "diff --git a/img.png b/img.png\nBinary files differ\n{}",
            "x".repeat(10_000)
        );
        assert_eq!(diff_compact(&binary), None);

        let malformed = format!(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n{}",
            "x".repeat(10_000)
        );
        assert_eq!(diff_compact(&malformed), None);
    }
}
