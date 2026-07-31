use serde_json::Value;
use std::collections::HashSet;
use url::Url;

const CITATION_OPEN: char = '\u{e200}';
const CITATION_CLOSE: char = '\u{e201}';
const CITATION_SEPARATOR: char = '\u{e202}';
const CITATION_HEADER: &str = "\u{e200}cite\u{e202}";
const MAX_CITATION_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct AnnotationSpan {
    start_index: usize,
    end_index: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UrlCitation {
    span: AnnotationSpan,
    annotation_index: usize,
    title: String,
    url: String,
}

#[derive(Clone, Debug)]
pub(crate) struct AnnotationGroup {
    span: AnnotationSpan,
    citations: Vec<UrlCitation>,
}

impl AnnotationGroup {
    fn new(span: AnnotationSpan) -> Self {
        Self {
            span,
            citations: Vec::new(),
        }
    }

    fn span(&self) -> AnnotationSpan {
        self.span
    }

    fn push(&mut self, citation: UrlCitation) {
        self.citations.push(citation);
    }

    fn extend(&mut self, citations: impl IntoIterator<Item = UrlCitation>) {
        for citation in citations {
            if let Some(existing) = self
                .citations
                .iter_mut()
                .find(|existing| urls_match(&existing.url, &citation.url))
            {
                let existing_has_title = existing.title.split_whitespace().next().is_some();
                let citation_has_title = citation.title.split_whitespace().next().is_some();
                if citation_has_title || !existing_has_title {
                    *existing = citation;
                }
            } else {
                self.citations.push(citation);
            }
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct CitationTextState {
    streamed_raw: String,
    pending: String,
    emitted: String,
    active_group: Option<AnnotationGroup>,
    pending_groups: Vec<AnnotationGroup>,
    resolved_spans: HashSet<AnnotationSpan>,
    unmatched_dropped_envelopes: usize,
}

impl CitationTextState {
    pub(crate) fn push_delta(&mut self, delta: &str) -> String {
        let mut output = String::new();
        if self.active_group.is_some() {
            self.queue_active_group();
            output.push_str(&self.resolve_pending_groups());
        }

        self.streamed_raw.push_str(delta);
        self.pending.push_str(delta);
        output.push_str(&self.drain_ready(/* drop_unannotated */ false));
        output.push_str(&self.resolve_pending_groups());
        if self.pending.len() > MAX_CITATION_BYTES {
            let (released, dropped_envelopes) =
                self.drain_ready_counted(/* drop_unannotated */ true);
            self.unmatched_dropped_envelopes += dropped_envelopes;
            output.push_str(&released);
            output.push_str(&self.resolve_pending_groups());
        }

        self.record_output(output)
    }

    pub(crate) fn push_annotation(
        &mut self,
        span: AnnotationSpan,
        citation: Option<UrlCitation>,
    ) -> String {
        let mut output = String::new();
        if self
            .active_group
            .as_ref()
            .is_some_and(|group| group.span() != span)
        {
            self.queue_active_group();
            output.push_str(&self.resolve_pending_groups());
        }

        let group = self
            .active_group
            .get_or_insert_with(|| AnnotationGroup::new(span));
        if let Some(citation) = citation {
            group.push(citation);
        }

        self.record_output(output)
    }

    pub(crate) fn boundary(&mut self) -> String {
        self.queue_active_group();
        let mut output = self.resolve_pending_groups();
        output.push_str(&self.drain_ready(/* drop_unannotated */ false));
        self.record_output(output)
    }

    pub(crate) fn finish(
        &mut self,
        final_text: Option<&str>,
        final_groups: Vec<AnnotationGroup>,
    ) -> String {
        self.append_final_text(final_text);
        self.queue_active_group();

        let mut output = String::new();
        for group in final_groups {
            self.queue_group(group);
        }

        output.push_str(&self.resolve_pending_groups());
        self.pending_groups.clear();
        output.push_str(&self.drain_terminal());
        self.record_output(output)
    }

    pub(crate) fn finish_incomplete(&mut self) -> String {
        self.queue_active_group();
        let mut output = self.resolve_pending_groups();
        self.pending_groups.clear();
        output.push_str(&self.drain_terminal());
        self.record_output(output)
    }

    pub(crate) fn text(&self) -> &str {
        &self.emitted
    }

    fn append_final_text(&mut self, final_text: Option<&str>) {
        let Some(final_text) = final_text else {
            return;
        };

        if let Some(suffix) = final_text.strip_prefix(&self.streamed_raw) {
            self.streamed_raw.push_str(suffix);
            self.pending.push_str(suffix);
        } else if self.emitted.is_empty() {
            self.streamed_raw.clear();
            self.streamed_raw.push_str(final_text);
            self.pending.clear();
            self.pending.push_str(final_text);
            self.active_group = None;
            self.pending_groups.clear();
            self.resolved_spans.clear();
            self.unmatched_dropped_envelopes = 0;
        }
    }

    fn queue_active_group(&mut self) {
        if let Some(group) = self.active_group.take() {
            self.queue_group(group);
        }
    }

    fn queue_group(&mut self, group: AnnotationGroup) {
        if self.resolved_spans.contains(&group.span()) {
            return;
        }
        if let Some(existing) = self
            .pending_groups
            .iter_mut()
            .find(|existing| existing.span() == group.span())
        {
            existing.extend(group.citations);
        } else {
            self.pending_groups.push(group);
        }
    }

    fn resolve_pending_groups(&mut self) -> String {
        let ranges = find_citation_envelopes(&self.pending);
        let expected_groups = self.unmatched_dropped_envelopes + ranges.len();
        if expected_groups == 0 || expected_groups != self.pending_groups.len() {
            return String::new();
        }

        let mut groups = std::mem::take(&mut self.pending_groups);
        groups.sort_by_key(AnnotationGroup::span);
        let mut remaining_groups =
            groups.split_off(std::mem::take(&mut self.unmatched_dropped_envelopes));
        for group in groups {
            self.resolved_spans.insert(group.span());
        }
        for group in &mut remaining_groups {
            group
                .citations
                .sort_by_key(|citation| citation.annotation_index);
            self.resolved_spans.insert(group.span());
        }
        self.pending = replace_citation_envelopes(&self.pending, &ranges, &remaining_groups);
        self.drain_ready(/* drop_unannotated */ false)
    }

    fn drain_ready(&mut self, drop_unannotated: bool) -> String {
        self.drain_ready_counted(drop_unannotated).0
    }

    fn drain_ready_counted(&mut self, drop_unannotated: bool) -> (String, usize) {
        let mut output = String::new();
        let mut dropped_envelopes = 0;

        loop {
            let Some(open_index) = self.pending.find(CITATION_OPEN) else {
                output.push_str(&self.pending);
                self.pending.clear();
                break;
            };

            output.push_str(&self.pending[..open_index]);
            self.pending.drain(..open_index);

            match classify_citation_prefix(&self.pending) {
                CitationPrefix::Complete { end } => {
                    if drop_unannotated {
                        self.pending.drain(..end);
                        dropped_envelopes += 1;
                        continue;
                    }
                    break;
                }
                CitationPrefix::Incomplete => {
                    if self.pending.len() <= MAX_CITATION_BYTES {
                        break;
                    }
                    take_open_literal(&mut self.pending, &mut output);
                }
                CitationPrefix::Literal => take_open_literal(&mut self.pending, &mut output),
            }
        }

        (output, dropped_envelopes)
    }

    fn drain_terminal(&mut self) -> String {
        let mut output = String::new();

        loop {
            let Some(open_index) = self.pending.find(CITATION_OPEN) else {
                output.push_str(&self.pending);
                self.pending.clear();
                break;
            };

            output.push_str(&self.pending[..open_index]);
            self.pending.drain(..open_index);

            match classify_citation_prefix(&self.pending) {
                CitationPrefix::Complete { end } => {
                    self.pending.drain(..end);
                }
                CitationPrefix::Incomplete => {
                    output.push_str(&self.pending);
                    self.pending.clear();
                    break;
                }
                CitationPrefix::Literal => take_open_literal(&mut self.pending, &mut output),
            }
        }

        output
    }

    fn record_output(&mut self, output: String) -> String {
        self.emitted.push_str(&output);
        output
    }
}

pub(crate) fn annotation_span(value: &Value) -> Option<AnnotationSpan> {
    let value = value.get("url_citation").unwrap_or(value);
    let start_index = usize::try_from(value.get("start_index")?.as_u64()?).ok()?;
    let end_index = usize::try_from(value.get("end_index")?.as_u64()?).ok()?;
    (end_index > start_index).then_some(AnnotationSpan {
        start_index,
        end_index,
    })
}

pub(crate) fn parse_url_citation(value: &Value, annotation_index: usize) -> Option<UrlCitation> {
    if value.get("type").and_then(Value::as_str) != Some("url_citation") {
        return None;
    }

    let citation = value.get("url_citation").unwrap_or(value);
    Some(UrlCitation {
        span: annotation_span(citation)?,
        annotation_index,
        title: citation
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        url: citation.get("url")?.as_str()?.to_string(),
    })
}

pub(crate) fn parse_output_annotation_groups(value: Option<&Value>) -> Vec<AnnotationGroup> {
    let mut groups: Vec<AnnotationGroup> = Vec::new();
    for (index, annotation) in value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(span) = annotation_span(annotation) else {
            continue;
        };
        let citation = parse_url_citation(annotation, index);
        if let Some(group) = groups.iter_mut().find(|group| group.span() == span) {
            if let Some(citation) = citation {
                group.push(citation);
            }
        } else {
            let mut group = AnnotationGroup::new(span);
            if let Some(citation) = citation {
                group.push(citation);
            }
            groups.push(group);
        }
    }
    groups.sort_by_key(AnnotationGroup::span);
    groups
}

pub(crate) fn normalize_text(text: &str, groups: Vec<AnnotationGroup>) -> String {
    let mut state = CitationTextState::default();
    state.finish(Some(text), groups);
    state.text().to_string()
}

fn render_links(citations: &[UrlCitation]) -> String {
    let mut seen_urls = HashSet::new();
    citations
        .iter()
        .filter_map(|citation| {
            let url = Url::parse(&citation.url).ok()?;
            if !matches!(url.scheme(), "http" | "https") {
                return None;
            }
            let destination = url.to_string();
            if !seen_urls.insert(destination.clone()) {
                return None;
            }

            let title = citation
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let label = if title.is_empty() {
                url.host_str().unwrap_or("source")
            } else {
                &title
            };
            Some(format!(
                "[{}](<{destination}>)",
                escape_markdown_label(label)
            ))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn urls_match(left: &str, right: &str) -> bool {
    left == right
        || matches!(
            (Url::parse(left), Url::parse(right)),
            (Ok(left), Ok(right)) if left == right
        )
}

fn escape_markdown_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('<', "\\<")
        .replace('>', "\\>")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CitationPrefix {
    Complete { end: usize },
    Incomplete,
    Literal,
}

fn classify_citation_prefix(text: &str) -> CitationPrefix {
    if !text.starts_with(CITATION_OPEN) {
        return CitationPrefix::Literal;
    }
    if text.len() < CITATION_HEADER.len() {
        return if CITATION_HEADER.starts_with(text) {
            CitationPrefix::Incomplete
        } else {
            CitationPrefix::Literal
        };
    }
    if !text.starts_with(CITATION_HEADER) {
        return CitationPrefix::Literal;
    }

    let Some(close_offset) = text[CITATION_HEADER.len()..].find(CITATION_CLOSE) else {
        return CitationPrefix::Incomplete;
    };
    let end = CITATION_HEADER.len() + close_offset + CITATION_CLOSE.len_utf8();
    if is_citation_envelope(&text[..end]) {
        CitationPrefix::Complete { end }
    } else {
        CitationPrefix::Literal
    }
}

fn is_citation_envelope(text: &str) -> bool {
    let Some(body) = text
        .strip_prefix(CITATION_HEADER)
        .and_then(|text| text.strip_suffix(CITATION_CLOSE))
    else {
        return false;
    };
    let mut references = body.split(CITATION_SEPARATOR);
    let Some(first) = references.next() else {
        return false;
    };
    is_private_reference(first) && references.all(is_private_reference)
}

fn is_private_reference(reference: &str) -> bool {
    let Some(rest) = reference.strip_prefix("turn") else {
        return false;
    };
    let digit_prefix = rest.chars().take_while(char::is_ascii_digit).count();
    if digit_prefix == 0 {
        return false;
    }
    let rest = &rest[digit_prefix..];
    let kind_len = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphabetic() || matches!(ch, '_' | '-'))
        .count();
    if kind_len == 0 {
        return false;
    }
    let suffix = &rest[kind_len..];
    !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit())
}

fn find_citation_envelopes(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut search_start = 0;
    while let Some(relative_open) = text[search_start..].find(CITATION_OPEN) {
        let open = search_start + relative_open;
        match classify_citation_prefix(&text[open..]) {
            CitationPrefix::Complete { end } => {
                ranges.push((open, open + end));
                search_start = open + end;
            }
            CitationPrefix::Incomplete => break,
            CitationPrefix::Literal => {
                search_start = open + CITATION_OPEN.len_utf8();
            }
        }
    }
    ranges
}

fn replace_citation_envelopes(
    text: &str,
    ranges: &[(usize, usize)],
    groups: &[AnnotationGroup],
) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut previous_end = 0;
    for ((start, end), group) in ranges.iter().copied().zip(groups) {
        normalized.push_str(&text[previous_end..start]);
        normalized.push_str(&render_links(&group.citations));
        previous_end = end;
    }
    normalized.push_str(&text[previous_end..]);
    normalized
}

fn take_open_literal(pending: &mut String, output: &mut String) {
    output.push(CITATION_OPEN);
    pending.drain(..CITATION_OPEN.len_utf8());
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn citation(index: usize, start: usize, end: usize, title: &str, url: &str) -> UrlCitation {
        parse_url_citation(
            &json!({
                "type": "url_citation",
                "start_index": start,
                "end_index": end,
                "title": title,
                "url": url,
            }),
            index,
        )
        .expect("valid citation")
    }

    fn normalize(text: &str, citations: Vec<UrlCitation>) -> String {
        let mut groups: Vec<AnnotationGroup> = Vec::new();
        for citation in citations {
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.span() == citation.span)
            {
                group.push(citation);
            } else {
                let mut group = AnnotationGroup::new(citation.span);
                group.push(citation);
                groups.push(group);
            }
        }
        normalize_text(text, groups)
    }

    #[test]
    fn normalizes_multiple_links_and_escapes_labels() {
        let marker = "\u{e200}cite\u{e202}turn0search0\u{e202}turn0search1\u{e201}";
        let text = format!("Result{marker}");
        assert_eq!(
            normalize(
                &text,
                vec![
                    citation(1, 6, 40, "Second", "https://second.example/path",),
                    citation(0, 6, 40, r"First [source]", "https://first.example/path",),
                ],
            ),
            "Result[First \\[source\\]](<https://first.example/path>) [Second](<https://second.example/path>)"
        );
    }

    #[test]
    fn citation_labels_escape_html_like_control_tags() {
        let marker = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        let normalized = normalize(
            &format!("Result{marker}"),
            vec![citation(
                0,
                6,
                26,
                r"Path \ [source] <oai-mem-citation>hidden</oai-mem-citation>",
                "https://example.com/source",
            )],
        );
        let expected = r"Result[Path \\ \[source\] \<oai-mem-citation\>hidden\</oai-mem-citation\>](<https://example.com/source>)";

        assert_eq!(normalized, expected);
        assert!(!normalized.contains("<oai-mem-citation>"));
        assert!(!normalized.contains("</oai-mem-citation>"));
    }

    #[test]
    fn fragmented_marker_waits_for_annotation() {
        let mut state = CitationTextState::default();
        assert_eq!(state.push_delta("前缀\u{e200}ci"), "前缀");
        assert_eq!(state.push_delta("te\u{e202}turn0search0\u{e201}后缀"), "");
        assert_eq!(
            state.push_annotation(
                AnnotationSpan {
                    start_index: 2,
                    end_index: 22,
                },
                Some(citation(0, 2, 22, "", "https://example.com/source",)),
            ),
            ""
        );
        assert_eq!(
            state.boundary(),
            "[example.com](<https://example.com/source>)后缀"
        );
    }

    #[test]
    fn delayed_annotation_keeps_following_text_in_order() {
        let marker = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        let mut state = CitationTextState::default();
        assert_eq!(state.push_delta(&format!("A{marker}")), "A");
        assert_eq!(state.push_delta(" trailing"), "");
        assert_eq!(
            state.push_annotation(
                AnnotationSpan {
                    start_index: 1,
                    end_index: 20,
                },
                Some(citation(0, 1, 20, "Delayed", "https://example.com/delayed",)),
            ),
            ""
        );
        assert_eq!(
            state.boundary(),
            "[Delayed](<https://example.com/delayed>) trailing"
        );
    }

    #[test]
    fn final_annotation_upgrades_unemitted_stream_metadata() {
        let marker = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        let span = AnnotationSpan {
            start_index: 0,
            end_index: 20,
        };
        let mut state = CitationTextState::default();

        assert_eq!(state.push_delta(marker), "");
        assert_eq!(
            state.push_annotation(
                span,
                Some(citation(0, 0, 20, "", "https://example.com/source")),
            ),
            ""
        );
        assert_eq!(
            state.finish(
                Some(marker),
                vec![{
                    let mut group = AnnotationGroup::new(span);
                    group.push(citation(
                        0,
                        0,
                        20,
                        "Final title",
                        "https://example.com/source",
                    ));
                    group
                }],
            ),
            "[Final title](<https://example.com/source>)"
        );
    }

    #[test]
    fn final_annotations_preserve_unusable_group_alignment() {
        let first = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        let second = "\u{e200}cite\u{e202}turn0search1\u{e201}";
        let text = format!("A{first} B{second} C");
        let groups = parse_output_annotation_groups(Some(&json!([
            {
                "type": "file_citation",
                "start_index": 1,
                "end_index": 20,
            },
            {
                "type": "url_citation",
                "start_index": 22,
                "end_index": 41,
                "title": "Second",
                "url": "https://example.com/second",
            }
        ])));

        assert_eq!(
            normalize_text(&text, groups),
            "A B[Second](<https://example.com/second>) C"
        );
    }

    #[test]
    fn missing_annotation_group_does_not_shift_a_later_link() {
        let first = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        let second = "\u{e200}cite\u{e202}turn0search1\u{e201}";
        let text = format!("A{first} B{second} C");
        let groups = parse_output_annotation_groups(Some(&json!([{
            "type": "url_citation",
            "start_index": 22,
            "end_index": 41,
            "title": "Second",
            "url": "https://example.com/second",
        }])));

        assert_eq!(normalize_text(&text, groups), "A B C");
    }

    #[test]
    fn drops_complete_unannotated_markers_but_preserves_malformed_text() {
        let valid = "\u{e200}cite\u{e202}turn0view0\u{e201}";
        let malformed = "\u{e200}cite\u{e202}not-private\u{e201}";
        assert_eq!(
            normalize(&format!("a{valid}b{malformed}c"), Vec::new()),
            format!("ab{malformed}c")
        );
    }

    #[test]
    fn invalid_urls_are_dropped_and_duplicate_urls_are_deduplicated() {
        let marker = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        assert_eq!(
            normalize(
                marker,
                vec![
                    citation(0, 0, 20, "bad", "file:///tmp/source"),
                    citation(1, 0, 20, "one", "https://example.com/a"),
                    citation(2, 0, 20, "two", "https://example.com/a"),
                ],
            ),
            "[one](<https://example.com/a>)"
        );
    }

    #[test]
    fn incomplete_markers_are_literal_at_terminal_finish() {
        let marker = "\u{e200}cite\u{e202}turn0search0";
        assert_eq!(normalize(marker, Vec::new()), marker);
    }

    #[test]
    fn unicode_prefix_does_not_depend_on_provider_index_units() {
        let marker = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        let text = format!("中🙂e\u{301}{marker}尾");
        assert_eq!(
            normalize(
                &text,
                vec![citation(
                    0,
                    999,
                    1_001,
                    "Unicode",
                    "https://example.com/unicode",
                )],
            ),
            "中🙂e\u{301}[Unicode](<https://example.com/unicode>)尾"
        );
    }

    #[test]
    fn overlong_unclosed_candidate_is_released_as_literal() {
        let marker = format!("{CITATION_HEADER}{}", "x".repeat(MAX_CITATION_BYTES));
        let mut state = CitationTextState::default();
        assert_eq!(state.push_delta(&marker), marker);
        assert_eq!(state.finish_incomplete(), "");
    }

    #[test]
    fn overlong_complete_candidate_drops_marker_and_releases_tail() {
        let marker = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        let tail = "x".repeat(MAX_CITATION_BYTES);
        let mut state = CitationTextState::default();
        assert_eq!(state.push_delta(&format!("{marker}{tail}")), tail);
        assert_eq!(state.finish_incomplete(), "");
    }

    #[test]
    fn late_annotation_after_forced_drop_does_not_bind_next_marker() {
        let first = "\u{e200}cite\u{e202}turn0search0\u{e201}";
        let second = "\u{e200}cite\u{e202}turn0search1\u{e201}";
        let tail = "x".repeat(MAX_CITATION_BYTES);
        let mut state = CitationTextState::default();

        assert_eq!(state.push_delta(&format!("{first}{tail}")), tail);
        assert_eq!(
            state.push_annotation(
                AnnotationSpan {
                    start_index: 0,
                    end_index: 20,
                },
                Some(citation(0, 0, 20, "First", "https://example.com/first",)),
            ),
            ""
        );
        assert_eq!(state.push_delta(second), "");
        assert_eq!(
            state.push_annotation(
                AnnotationSpan {
                    start_index: 20,
                    end_index: 40,
                },
                Some(citation(1, 20, 40, "Second", "https://example.com/second",)),
            ),
            ""
        );
        assert_eq!(state.boundary(), "[Second](<https://example.com/second>)");
    }
}
