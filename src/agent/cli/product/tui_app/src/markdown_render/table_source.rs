use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
struct BacktickRun {
    range: Range<usize>,
    len: usize,
}

#[derive(Clone, Debug)]
struct CodePipeCandidate {
    range: Range<usize>,
    pipe_offsets: Vec<usize>,
    cell_aligned: bool,
    substantive_chars: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SelectionScore {
    cell_aligned: usize,
    candidates: usize,
    substantive_chars: usize,
    pipe_count: usize,
    source_len: usize,
}

impl SelectionScore {
    fn with_candidate(self, candidate: &CodePipeCandidate) -> Self {
        Self {
            cell_aligned: self.cell_aligned + usize::from(candidate.cell_aligned),
            candidates: self.candidates + 1,
            substantive_chars: self.substantive_chars + candidate.substantive_chars,
            pipe_count: self.pipe_count + candidate.pipe_offsets.len(),
            source_len: self.source_len + candidate.range.len(),
        }
    }
}

impl Ord for SelectionScore {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cell_aligned
            .cmp(&other.cell_aligned)
            .then_with(|| self.candidates.cmp(&other.candidates))
            .then_with(|| self.substantive_chars.cmp(&other.substantive_chars))
            .then_with(|| self.pipe_count.cmp(&other.pipe_count))
            .then_with(|| other.source_len.cmp(&self.source_len))
    }
}

impl PartialOrd for SelectionScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Default)]
struct OptimalSelection {
    score: SelectionScore,
    guaranteed_candidates: BTreeSet<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MaskOffset {
    offset: usize,
    line: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TableHeaderLocation {
    line: usize,
    content_offset_in_line: usize,
}

#[derive(Clone, Debug)]
struct TableSource {
    header: TableHeaderLocation,
    lines: BTreeSet<usize>,
}

#[derive(Debug)]
struct MaskedMarkdown {
    source: String,
    sentinel_offsets: Vec<usize>,
}

#[derive(Debug, Default)]
struct MarkdownAnalysis {
    tables: Vec<TableSource>,
    code_ranges: Vec<Range<usize>>,
}

pub(super) fn prepare_markdown<'a>(
    input: &'a str,
    options: Options,
) -> (Cow<'a, str>, Option<char>) {
    let line_ranges = physical_line_ranges(input);
    let mut masks = Vec::new();
    for (line, range) in line_ranges.iter().enumerate() {
        let source = &input[range.clone()];
        masks.extend(
            selected_code_pipe_offsets(source, options)
                .into_iter()
                .map(|offset| MaskOffset {
                    offset: range.start + offset,
                    line,
                }),
        );
    }
    if masks.is_empty() {
        return (Cow::Borrowed(input), None);
    }

    masks.sort_unstable_by_key(|mask| mask.offset);
    masks.dedup_by_key(|mask| mask.offset);

    let sentinel = (0xE000..=0xF8FF)
        .chain(0xF0000..=0xFFFFD)
        .filter_map(char::from_u32)
        .find(|candidate| !input.contains(*candidate));
    let Some(sentinel) = sentinel else {
        return (Cow::Borrowed(input), None);
    };

    // Parse once with every credible mask, then retain masks only in accepted table scopes.
    let provisional = mask_offsets(input, &masks, sentinel);
    let original_headers = table_sources(input, options)
        .into_iter()
        .map(|table| table.header)
        .collect::<HashSet<_>>();
    let accepted_lines = table_sources(&provisional.source, options)
        .into_iter()
        .filter(|table| {
            original_headers.contains(&table.header)
                || table_header_source(input, &line_ranges, table.header)
                    .is_some_and(has_explicit_row_boundaries)
        })
        .flat_map(|table| table.lines)
        .collect::<HashSet<_>>();
    masks.retain(|mask| accepted_lines.contains(&mask.line));

    loop {
        if masks.is_empty() {
            return (Cow::Borrowed(input), None);
        }
        let masked = mask_offsets(input, &masks, sentinel);
        let analysis = analyze_markdown(&masked.source, options);
        let final_lines = analysis
            .tables
            .into_iter()
            .flat_map(|table| table.lines)
            .collect::<HashSet<_>>();
        let previous_len = masks.len();
        masks = masks
            .into_iter()
            .zip(masked.sentinel_offsets.iter().copied())
            .filter_map(|(mask, sentinel_offset)| {
                (final_lines.contains(&mask.line)
                    && analysis
                        .code_ranges
                        .iter()
                        .any(|range| range.contains(&sentinel_offset)))
                .then_some(mask)
            })
            .collect();
        if masks.len() == previous_len {
            return (Cow::Owned(masked.source), Some(sentinel));
        }
    }
}

pub(super) fn is_backslash_escaped(source: &str, offset: usize) -> bool {
    source
        .as_bytes()
        .get(..offset)
        .unwrap_or_default()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn selected_code_pipe_offsets(source: &str, options: Options) -> Vec<usize> {
    select_candidate_pipe_offsets(code_pipe_candidates(source, options))
}

fn select_candidate_pipe_offsets(mut candidates: Vec<CodePipeCandidate>) -> Vec<usize> {
    candidates.sort_unstable_by_key(|candidate| (candidate.range.end, candidate.range.start));
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut selections = Vec::with_capacity(candidates.len() + 1);
    selections.push(OptimalSelection::default());
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        let compatible_count = candidates[..candidate_index]
            .partition_point(|other| other.range.end <= candidate.range.start);
        let mut take = selections[compatible_count].clone();
        take.score = take.score.with_candidate(candidate);
        take.guaranteed_candidates.insert(candidate_index);
        let skip = &selections[candidate_index];
        let selection = match take.score.cmp(&skip.score) {
            Ordering::Greater => take,
            Ordering::Less => skip.clone(),
            // Only candidates shared by every optimal selection are safe to mask.
            Ordering::Equal => OptimalSelection {
                score: take.score,
                guaranteed_candidates: take
                    .guaranteed_candidates
                    .intersection(&skip.guaranteed_candidates)
                    .copied()
                    .collect(),
            },
        };
        selections.push(selection);
    }

    let mut offsets = selections
        .last()
        .into_iter()
        .flat_map(|selection| &selection.guaranteed_candidates)
        .flat_map(|candidate_index| &candidates[*candidate_index].pipe_offsets)
        .copied()
        .collect::<Vec<_>>();
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

fn code_pipe_candidates(source: &str, options: Options) -> Vec<CodePipeCandidate> {
    let runs = backtick_runs(source);
    let mut candidates = Vec::new();
    for (run_index, opener) in runs.iter().enumerate() {
        let Some(closer) = runs[run_index + 1..]
            .iter()
            .find(|closer| closer.len == opener.len)
        else {
            continue;
        };
        let range = opener.range.start..closer.range.end;
        let Some(code) = parsed_code_content(&source[range.clone()], options) else {
            continue;
        };
        let substantive_chars = code
            .chars()
            .filter(|character| !character.is_whitespace() && !matches!(character, '\\' | '|'))
            .count();
        if substantive_chars == 0 {
            continue;
        }
        let pipe_offsets = source[opener.range.end..closer.range.start]
            .match_indices('|')
            .map(|(offset, _)| opener.range.end + offset)
            .filter(|offset| source.as_bytes()[offset - 1] != b'\\')
            .collect::<Vec<_>>();
        if pipe_offsets.is_empty() {
            continue;
        }
        candidates.push(CodePipeCandidate {
            cell_aligned: candidate_is_cell_aligned(source, &range),
            range,
            pipe_offsets,
            substantive_chars,
        });
    }
    candidates
}

fn backtick_runs(source: &str) -> Vec<BacktickRun> {
    let bytes = source.as_bytes();
    let mut runs = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] != b'`' {
            offset += 1;
            continue;
        }
        let start = offset;
        while offset < bytes.len() && bytes[offset] == b'`' {
            offset += 1;
        }
        let effective_start = start + usize::from(is_backslash_escaped(source, start));
        if effective_start < offset {
            runs.push(BacktickRun {
                range: effective_start..offset,
                len: offset - effective_start,
            });
        }
    }
    runs
}

fn parsed_code_content(source: &str, options: Options) -> Option<String> {
    let mut inline_options = options;
    inline_options.remove(Options::ENABLE_TABLES);
    let mut code = None;
    for (event, range) in Parser::new_ext(source, inline_options).into_offset_iter() {
        match event {
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
            Event::Code(content) if code.is_none() && range == (0..source.len()) => {
                code = Some(content.into_string());
            }
            Event::Code(_)
            | Event::Start(_)
            | Event::End(_)
            | Event::Text(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::Rule
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
            | Event::TaskListMarker(_) => return None,
        }
    }
    code
}

fn candidate_is_cell_aligned(source: &str, range: &Range<usize>) -> bool {
    let separators = source
        .match_indices('|')
        .map(|(offset, _)| offset)
        .filter(|offset| !is_backslash_escaped(source, *offset))
        .collect::<Vec<_>>();
    let before = separators
        .iter()
        .copied()
        .take_while(|offset| *offset < range.start)
        .last()
        .map_or(0, |offset| offset + 1);
    let after = separators
        .iter()
        .copied()
        .find(|offset| *offset >= range.end)
        .unwrap_or(source.len());
    source[before..range.start].chars().all(char::is_whitespace)
        && source[range.end..after].chars().all(char::is_whitespace)
}

fn physical_line_ranges(input: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (offset, character) in input.char_indices() {
        if character == '\n' {
            ranges.push(start..offset);
            start = offset + character.len_utf8();
        }
    }
    if start < input.len() {
        ranges.push(start..input.len());
    }
    ranges
}

fn line_starts(input: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        input
            .match_indices('\n')
            .map(|(offset, _)| offset + '\n'.len_utf8()),
    );
    starts
}

fn table_header_source<'a>(
    input: &'a str,
    line_ranges: &[Range<usize>],
    header: TableHeaderLocation,
) -> Option<&'a str> {
    let line_range = line_ranges.get(header.line)?;
    let start = line_range
        .start
        .checked_add(header.content_offset_in_line)?;
    input.get(start..line_range.end)
}

fn table_sources(input: &str, options: Options) -> Vec<TableSource> {
    analyze_markdown(input, options).tables
}

fn analyze_markdown(input: &str, options: Options) -> MarkdownAnalysis {
    if input.is_empty() {
        return MarkdownAnalysis::default();
    }
    let starts = line_starts(input);
    let mut table_options = options;
    table_options.insert(Options::ENABLE_TABLES);
    let mut active = None;
    let mut analysis = MarkdownAnalysis::default();
    for (event, range) in Parser::new_ext(input, table_options).into_offset_iter() {
        if matches!(&event, Event::Start(Tag::Table(_))) {
            let header_line = line_number_at(&starts, range.start);
            active = Some(TableSource {
                header: TableHeaderLocation {
                    line: header_line,
                    content_offset_in_line: range.start.saturating_sub(starts[header_line]),
                },
                lines: BTreeSet::new(),
            });
        }
        if matches!(&event, Event::Code(_)) {
            analysis.code_ranges.push(range.clone());
        }
        if let Some(table) = active.as_mut() {
            add_range_lines(table, &starts, input.len(), &range);
        }
        if matches!(&event, Event::End(TagEnd::Table))
            && let Some(table) = active.take()
        {
            analysis.tables.push(table);
        }
    }
    analysis
}

fn add_range_lines(
    table: &mut TableSource,
    line_starts: &[usize],
    input_len: usize,
    range: &Range<usize>,
) {
    let start = line_number_at(line_starts, range.start.min(input_len.saturating_sub(1)));
    let last_offset = range
        .end
        .saturating_sub(1)
        .max(range.start)
        .min(input_len.saturating_sub(1));
    let end = line_number_at(line_starts, last_offset);
    table.lines.extend(start..=end);
}

fn line_number_at(line_starts: &[usize], offset: usize) -> usize {
    line_starts
        .partition_point(|line_start| *line_start <= offset)
        .saturating_sub(1)
}

fn has_explicit_row_boundaries(source: &str) -> bool {
    let mut source = source.trim_start();
    while let Some(quoted) = source.strip_prefix('>') {
        source = quoted.trim_start();
    }
    let source = source.trim_end();
    let Some(trailing_offset) = source.len().checked_sub(1) else {
        return false;
    };
    source.starts_with('|')
        && source.ends_with('|')
        && !is_backslash_escaped(source, trailing_offset)
}

fn mask_offsets(input: &str, masks: &[MaskOffset], sentinel: char) -> MaskedMarkdown {
    let mut output = String::with_capacity(input.len() + masks.len() * (sentinel.len_utf8() - 1));
    let mut sentinel_offsets = Vec::with_capacity(masks.len());
    let mut cursor = 0;
    for mask in masks {
        output.push_str(&input[cursor..mask.offset]);
        sentinel_offsets.push(output.len());
        output.push(sentinel);
        cursor = mask.offset + 1;
    }
    output.push_str(&input[cursor..]);
    MaskedMarkdown {
        source: output,
        sentinel_offsets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn candidate(range: Range<usize>, pipe_offset: usize, cell_aligned: bool) -> CodePipeCandidate {
        CodePipeCandidate {
            range,
            pipe_offsets: vec![pipe_offset],
            cell_aligned,
            substantive_chars: 1,
        }
    }

    #[test]
    fn equally_scored_overlapping_candidates_remain_unmasked() {
        let candidates = vec![
            candidate(0..5, 2, true),
            candidate(3..8, 5, true),
            candidate(10..15, 12, true),
        ];

        assert_eq!(select_candidate_pipe_offsets(candidates), vec![12]);
    }

    #[test]
    fn cell_aligned_candidate_outranks_embedded_candidates() {
        let candidates = vec![
            candidate(0..4, 2, false),
            candidate(4..8, 6, false),
            candidate(1..7, 3, true),
        ];

        assert_eq!(select_candidate_pipe_offsets(candidates), vec![3]);
    }

    #[test]
    fn escaped_backtick_run_preserves_the_unescaped_suffix() {
        let source = r"\``foo|bar`";
        let closer_start = source.len() - 1;

        assert_eq!(
            backtick_runs(source),
            vec![
                BacktickRun {
                    range: 2..3,
                    len: 1,
                },
                BacktickRun {
                    range: closer_start..source.len(),
                    len: 1,
                },
            ]
        );
    }

    #[test]
    fn final_masks_only_remain_inside_document_code_events() {
        let source = r#"| H1 | H2 | H3 |
| --- | --- | --- |
| prefix `x ``p|` longword|value`` suffix | tail |"#;
        let (prepared, sentinel) = prepare_markdown(source, Options::empty());
        let sentinel = sentinel.expect("code pipe should be masked");
        let prepared = prepared.into_owned();
        let analysis = analyze_markdown(&prepared, Options::empty());
        let sentinel_offsets = prepared
            .char_indices()
            .filter_map(|(offset, character)| (character == sentinel).then_some(offset))
            .collect::<Vec<_>>();

        assert_eq!(sentinel_offsets.len(), 1);
        assert!(sentinel_offsets.iter().all(|offset| {
            analysis
                .code_ranges
                .iter()
                .any(|range| range.contains(offset))
        }));
        assert!(prepared.contains("longword|value"));
    }
}
