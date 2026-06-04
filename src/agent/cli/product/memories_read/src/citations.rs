use crate::product::protocol::ThreadId;
use crate::product::protocol::memory_citation::MemoryCitation;
use crate::product::protocol::memory_citation::MemoryCitationEntry;
use std::collections::HashSet;

const OPEN_TAG: &str = "<oai-mem-citation>";
const CLOSE_TAG: &str = "</oai-mem-citation>";

pub fn strip_memory_citation_block(text: &str) -> (String, Vec<String>) {
    let mut stripped = String::with_capacity(text.len());
    let mut citations = Vec::new();
    let mut rest = text;

    while let Some(open_idx) = rest.find(OPEN_TAG) {
        stripped.push_str(&rest[..open_idx]);
        let after_open = &rest[open_idx + OPEN_TAG.len()..];
        let Some(close_idx) = after_open.find(CLOSE_TAG) else {
            return (stripped.trim_end().to_string(), citations);
        };
        let close_end = open_idx + OPEN_TAG.len() + close_idx + CLOSE_TAG.len();
        citations.push(rest[open_idx..close_end].to_string());
        rest = &rest[close_end..];
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
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn strips_and_parses_citation_block() {
        let text = "answer\n<oai-mem-citation>\n<citation_entries>\nMEMORY.md:1-2|note=[used preference]\n</citation_entries>\n<rollout_ids>\n00000000-0000-0000-0000-000000000001\n</rollout_ids>\n</oai-mem-citation>";
        let (stripped, blocks) = strip_memory_citation_block(text);
        assert_eq!(stripped, "answer");
        let citation = parse_memory_citation(blocks).expect("citation");
        assert_eq!(citation.entries.len(), 1);
        assert_eq!(
            citation.rollout_ids,
            vec!["00000000-0000-0000-0000-000000000001"]
        );
    }

    #[test]
    fn strips_malformed_unclosed_citation_tail() {
        let text = "answer\n<oai-mem-citation>\nhidden";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, "answer");
        assert_eq!(blocks, Vec::<String>::new());
        assert_eq!(parse_memory_citation(blocks), None);
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
}
