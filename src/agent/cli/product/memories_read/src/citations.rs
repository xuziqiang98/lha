use crate::product::protocol::ThreadId;
use crate::product::protocol::memory_citation::MemoryCitation;
use crate::product::protocol::memory_citation::MemoryCitationEntry;
use std::collections::HashSet;

const OPEN_TAG: &str = "<oai-mem-citation>";
const CLOSE_TAG: &str = "</oai-mem-citation>";
const CITATION_ENTRIES_OPEN_TAG: &str = "<citation_entries>";
const ROLLOUT_IDS_OPEN_TAG: &str = "<rollout_ids>";
const THREAD_IDS_OPEN_TAG: &str = "<thread_ids>";
const STRUCTURED_INNER_OPEN_TAGS: [&str; 3] = [
    CITATION_ENTRIES_OPEN_TAG,
    ROLLOUT_IDS_OPEN_TAG,
    THREAD_IDS_OPEN_TAG,
];

#[derive(Debug, PartialEq, Eq)]
enum MemoryCitationPrefix {
    Complete { end: usize },
    Incomplete,
    Literal,
}

#[derive(Debug, Default)]
pub(crate) struct MemoryCitationDeltaFilter {
    pending: String,
    trailing_whitespace: String,
}

impl MemoryCitationDeltaFilter {
    pub(crate) fn reset(&mut self) {
        self.pending.clear();
        self.trailing_whitespace.clear();
    }

    pub(crate) fn push(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        self.process(/* finish */ false)
    }

    pub(crate) fn finish(&mut self) -> String {
        self.process(/* finish */ true)
    }

    fn process(&mut self, finish: bool) -> String {
        let mut output = String::new();

        loop {
            let Some(open_idx) = self.pending.find(OPEN_TAG) else {
                let keep = if finish {
                    0
                } else {
                    longest_suffix_matching_prefix(&self.pending, OPEN_TAG)
                };
                let emit_end = self.pending.len() - keep;
                self.emit_pending_prefix(emit_end, &mut output);
                if finish {
                    self.trailing_whitespace.clear();
                }
                return output;
            };

            self.emit_pending_prefix(open_idx, &mut output);

            match classify_memory_citation_prefix(&self.pending) {
                MemoryCitationPrefix::Complete { end } => {
                    self.pending.drain(..end);
                }
                MemoryCitationPrefix::Literal => {
                    self.emit_pending_prefix(OPEN_TAG.len(), &mut output);
                }
                MemoryCitationPrefix::Incomplete if finish => {
                    if !has_structured_inner_open_tag(&self.pending) {
                        let pending_len = self.pending.len();
                        self.emit_pending_prefix(pending_len, &mut output);
                    } else {
                        self.pending.clear();
                    }
                    self.trailing_whitespace.clear();
                    return output;
                }
                MemoryCitationPrefix::Incomplete => return output,
            }
        }
    }

    fn emit_pending_prefix(&mut self, end: usize, output: &mut String) {
        let visible_end = self.pending[..end].trim_end().len();
        if visible_end == 0 {
            self.trailing_whitespace.push_str(&self.pending[..end]);
        } else {
            output.push_str(&self.trailing_whitespace);
            self.trailing_whitespace.clear();
            output.push_str(&self.pending[..visible_end]);
            self.trailing_whitespace
                .push_str(&self.pending[visible_end..end]);
        }
        self.pending.drain(..end);
    }
}

fn classify_memory_citation_prefix(text: &str) -> MemoryCitationPrefix {
    let Some(after_open) = text.strip_prefix(OPEN_TAG) else {
        return MemoryCitationPrefix::Literal;
    };
    let body = after_open.trim_start();

    if has_supported_inner_open_tag(body) {
        return text
            .find(CLOSE_TAG)
            .map(|close_idx| MemoryCitationPrefix::Complete {
                end: close_idx + CLOSE_TAG.len(),
            })
            .unwrap_or(MemoryCitationPrefix::Incomplete);
    }

    if body.is_empty()
        || STRUCTURED_INNER_OPEN_TAGS
            .iter()
            .any(|tag| tag.starts_with(body))
    {
        MemoryCitationPrefix::Incomplete
    } else {
        MemoryCitationPrefix::Literal
    }
}

fn has_supported_inner_open_tag(body: &str) -> bool {
    STRUCTURED_INNER_OPEN_TAGS
        .iter()
        .any(|tag| body.starts_with(tag))
}

fn has_structured_inner_open_tag(text: &str) -> bool {
    text.strip_prefix(OPEN_TAG)
        .is_some_and(|after_open| has_supported_inner_open_tag(after_open.trim_start()))
}

fn longest_suffix_matching_prefix(input: &str, pattern: &str) -> usize {
    let max_len = input.len().min(pattern.len().saturating_sub(1));
    (1..=max_len)
        .rev()
        .find(|&len| input.ends_with(&pattern[..len]))
        .unwrap_or(0)
}

pub fn strip_memory_citation_block(text: &str) -> (String, Vec<String>) {
    let mut stripped = String::with_capacity(text.len());
    let mut citations = Vec::new();
    let mut rest = text;

    while let Some(open_idx) = rest.find(OPEN_TAG) {
        stripped.push_str(&rest[..open_idx]);
        let candidate = &rest[open_idx..];
        match classify_memory_citation_prefix(candidate) {
            MemoryCitationPrefix::Complete { end } => {
                citations.push(candidate[..end].to_string());
                rest = &candidate[end..];
            }
            MemoryCitationPrefix::Incomplete if has_structured_inner_open_tag(candidate) => {
                return (stripped.trim_end().to_string(), citations);
            }
            MemoryCitationPrefix::Incomplete | MemoryCitationPrefix::Literal => {
                stripped.push_str(OPEN_TAG);
                rest = &candidate[OPEN_TAG.len()..];
            }
        }
    }

    stripped.push_str(rest);
    (stripped.trim_end().to_string(), citations)
}

pub fn parse_memory_citation(citations: Vec<String>) -> Option<MemoryCitation> {
    let mut entries = Vec::new();
    let mut rollout_ids = Vec::new();
    let mut seen_rollout_ids = HashSet::new();

    for citation in citations {
        if let Some(entries_block) =
            extract_block(&citation, "<citation_entries>", "</citation_entries>")
        {
            entries.extend(
                entries_block
                    .lines()
                    .filter_map(parse_memory_citation_entry),
            );
        }

        if let Some(ids_block) = extract_ids_block(&citation) {
            for id in ids_block
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                if seen_rollout_ids.insert(id.to_string()) {
                    rollout_ids.push(id.to_string());
                }
            }
        }
    }

    if entries.is_empty() && rollout_ids.is_empty() {
        None
    } else {
        Some(MemoryCitation {
            entries,
            rollout_ids,
        })
    }
}

pub fn thread_ids_from_memory_citation(memory_citation: &MemoryCitation) -> Vec<ThreadId> {
    memory_citation
        .rollout_ids
        .iter()
        .filter_map(|id| ThreadId::try_from(id.as_str()).ok())
        .collect()
}

fn parse_memory_citation_entry(line: &str) -> Option<MemoryCitationEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let (location, note) = line.rsplit_once("|note=[")?;
    let note = note.strip_suffix(']')?.trim().to_string();
    let (path, line_range) = location.rsplit_once(':')?;
    let (line_start, line_end) = line_range.split_once('-')?;

    Some(MemoryCitationEntry {
        path: path.trim().to_string(),
        line_start: line_start.trim().parse().ok()?,
        line_end: line_end.trim().parse().ok()?,
        note,
    })
}

fn extract_block<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(open)?;
    let (body, _) = rest.split_once(close)?;
    Some(body)
}

fn extract_ids_block(text: &str) -> Option<&str> {
    extract_block(text, "<rollout_ids>", "</rollout_ids>")
        .or_else(|| extract_block(text, "<thread_ids>", "</thread_ids>"))
}

#[cfg(test)]
mod memory_citation_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const STRUCTURED_CITATION: &str = "<oai-mem-citation>\n<citation_entries>\nMEMORY.md:1-2|note=[used preference]\n</citation_entries>\n<rollout_ids>\n00000000-0000-0000-0000-000000000001\n</rollout_ids>\n</oai-mem-citation>";

    #[test]
    fn strips_and_parses_citation_block() {
        let text = format!("answer\n{STRUCTURED_CITATION}");
        let (stripped, blocks) = strip_memory_citation_block(&text);
        assert_eq!(stripped, "answer");
        let citation = parse_memory_citation(blocks).expect("citation");
        assert_eq!(citation.entries.len(), 1);
        assert_eq!(
            citation.rollout_ids,
            vec!["00000000-0000-0000-0000-000000000001"]
        );
    }

    #[test]
    fn preserves_literal_open_tag() {
        let text = "answer mentions <oai-mem-citation> literally";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, text);
        assert_eq!(blocks, Vec::<String>::new());
    }

    #[test]
    fn preserves_non_structured_complete_tag_pair() {
        let text = "answer\n<oai-mem-citation>\nordinary text\n</oai-mem-citation>";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, text);
        assert_eq!(blocks, Vec::<String>::new());
    }

    #[test]
    fn strips_structured_citation_inside_proposed_plan() {
        let text = format!(
            "<proposed_plan>\nKeep this\n{STRUCTURED_CITATION}\nAnd this\n</proposed_plan>"
        );
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(
            stripped,
            "<proposed_plan>\nKeep this\n\nAnd this\n</proposed_plan>"
        );
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);
    }

    #[test]
    fn strips_malformed_structured_citation_tail() {
        let text = "answer\n<oai-mem-citation>\n<citation_entries>\nhidden";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, "answer");
        assert_eq!(blocks, Vec::<String>::new());
        assert_eq!(parse_memory_citation(blocks), None);
    }

    #[test]
    fn handles_literal_and_structured_candidates_in_order() {
        let literal = "<oai-mem-citation>`literal`";
        let text = format!("before {literal} middle {STRUCTURED_CITATION} after");
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, format!("before {literal} middle  after"));
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);
    }

    #[test]
    fn parses_legacy_thread_ids_block() {
        let text = "<oai-mem-citation>\n<thread_ids>\n00000000-0000-0000-0000-000000000001\n</thread_ids>\n</oai-mem-citation>";
        let (stripped, blocks) = strip_memory_citation_block(text);
        let citation = parse_memory_citation(blocks).expect("citation");

        assert_eq!(stripped, "");
        assert_eq!(
            citation.rollout_ids,
            vec!["00000000-0000-0000-0000-000000000001"]
        );
    }

    #[test]
    fn deduplicates_rollout_ids() {
        let text = "<oai-mem-citation>\n<rollout_ids>\n00000000-0000-0000-0000-000000000001\n00000000-0000-0000-0000-000000000001\n00000000-0000-0000-0000-000000000002\n</rollout_ids>\n</oai-mem-citation>";
        let (_stripped, blocks) = strip_memory_citation_block(text);
        let citation = parse_memory_citation(blocks).expect("citation");

        assert_eq!(
            citation.rollout_ids,
            vec![
                "00000000-0000-0000-0000-000000000001",
                "00000000-0000-0000-0000-000000000002"
            ]
        );
    }

    #[test]
    fn ignores_malformed_citation_entries() {
        let text = "<oai-mem-citation>\n<citation_entries>\nnot-a-citation\nMEMORY.md:3-4|note=[valid]\n</citation_entries>\n</oai-mem-citation>";
        let (_stripped, blocks) = strip_memory_citation_block(text);
        let citation = parse_memory_citation(blocks).expect("citation");

        assert_eq!(
            citation.entries,
            vec![MemoryCitationEntry {
                path: "MEMORY.md".to_string(),
                line_start: 3,
                line_end: 4,
                note: "valid".to_string(),
            }]
        );
    }

    #[test]
    fn leaves_text_without_citation_unchanged() {
        let text = "answer without hidden citation";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, text);
        assert_eq!(blocks, Vec::<String>::new());
    }

    #[test]
    fn delta_filter_suppresses_complete_citation_block() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push(&format!("answer \n{STRUCTURED_CITATION}tail")),
            "answer \ntail"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_trims_final_whitespace_before_citation() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push(&format!("answer \n{STRUCTURED_CITATION}")),
            "answer"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_buffers_open_tag_split_across_deltas() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("answer<oai"), "answer");
        assert_eq!(filter.push("-mem-citation>\n<citation_entries>"), "");
        assert_eq!(
            filter
                .push("\nMEMORY.md:1-2|note=[used]\n</citation_entries>\n</oai-mem-citation>tail"),
            "tail"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_buffers_inner_tag_split_across_deltas() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push("answer<oai-mem-citation>\n<citation_"),
            "answer"
        );
        assert_eq!(filter.push("entries>\nhidden"), "");
        assert_eq!(
            filter.push("</citation_entries></oai-mem-citation>tail"),
            "tail"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_buffers_close_tag_split_across_deltas() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push("answer<oai-mem-citation><rollout_ids>id</rollout_ids></oai-mem"),
            "answer"
        );
        assert_eq!(filter.push("-citation>tail"), "tail");
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_releases_literal_candidate_when_disambiguated() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("answer <oai-mem-citation>"), "answer");
        assert_eq!(
            filter.push("`literal` tail"),
            " <oai-mem-citation>`literal` tail"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_handles_literal_before_structured_citation() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push(&format!(
                "before <oai-mem-citation>\"literal\" middle {STRUCTURED_CITATION} after"
            )),
            "before <oai-mem-citation>\"literal\" middle  after"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_finish_flushes_partial_open_tag() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("literal <oai"), "literal");
        assert_eq!(filter.finish(), " <oai");
    }

    #[test]
    fn delta_filter_finish_flushes_literal_tag() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("literal <oai-mem-citation>"), "literal");
        assert_eq!(filter.finish(), " <oai-mem-citation>");
    }

    #[test]
    fn delta_filter_finish_discards_unclosed_structured_citation() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push("answer<oai-mem-citation>\n<citation_entries>\nhidden"),
            "answer"
        );
        assert_eq!(filter.finish(), "");
    }
}
