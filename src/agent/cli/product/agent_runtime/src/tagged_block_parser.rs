//! Line-based tag block parsing for streamed text.
//!
//! The parser buffers each line until it can disprove that the line is a tag,
//! which is required for tags that must appear alone on a line. For example,
//! Proposed Plan output uses `<proposed_plan>` and `</proposed_plan>` tags
//! on their own lines so clients can stream plan content separately.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TagSpec<T> {
    pub(crate) open: &'static str,
    pub(crate) close: &'static str,
    pub(crate) tag: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaggedLineSegment<T> {
    Normal(String),
    TagStart(T),
    TagDelta(T, String),
    TagEnd(T),
}

/// Stateful line parser that splits input into normal text vs tag blocks.
///
/// How it works:
/// - While reading a line, we buffer characters until the line either finishes
///   (`\n`) or stops matching any tag prefix (after `trim_start`) or Markdown
///   fence prefix.
/// - If it stops matching a tag prefix, the buffered line is immediately
///   emitted as text and we continue in "plain text" mode until the next
///   newline.
/// - When a full line is available, we compare it to the open/close tags; tag
///   lines emit TagStart/TagEnd, otherwise the line is emitted as text. Markdown
///   fenced code blocks are treated as text so examples can contain literal tag
///   lines without closing the surrounding block.
/// - `finish()` flushes any buffered line and auto-closes an unterminated tag,
///   which keeps streaming resilient to missing closing tags.
#[derive(Debug, Default)]
pub(crate) struct TaggedLineParser<T>
where
    T: Copy + Eq,
{
    specs: Vec<TagSpec<T>>,
    active_tag: Option<T>,
    active_tag_indent: String,
    nested_tag_depth: usize,
    detect_tag: bool,
    line_buffer: String,
    markdown_fence: Option<MarkdownFence>,
}

impl<T> TaggedLineParser<T>
where
    T: Copy + Eq,
{
    pub(crate) fn new(specs: Vec<TagSpec<T>>) -> Self {
        Self {
            specs,
            active_tag: None,
            active_tag_indent: String::new(),
            nested_tag_depth: 0,
            detect_tag: true,
            line_buffer: String::new(),
            markdown_fence: None,
        }
    }

    /// Parse a streamed delta into line-aware segments.
    pub(crate) fn parse(&mut self, delta: &str) -> Vec<TaggedLineSegment<T>> {
        let mut segments = Vec::new();
        let mut run = String::new();

        for ch in delta.chars() {
            if self.detect_tag {
                if !run.is_empty() {
                    self.push_text(std::mem::take(&mut run), &mut segments);
                }
                self.line_buffer.push(ch);
                if ch == '\n' {
                    self.finish_line(&mut segments);
                    continue;
                }
                let tag_slug = self.line_buffer.trim_start();
                let fence_slug = markdown_fence_slug(&self.line_buffer);
                if self.markdown_fence.is_some()
                    || tag_slug.is_empty()
                    || self.is_tag_prefix(tag_slug)
                    || fence_slug.is_some_and(|slug| slug.is_empty() || is_fence_prefix(slug))
                {
                    continue;
                }
                // This line cannot be a tag line, so flush it immediately.
                let buffered = std::mem::take(&mut self.line_buffer);
                self.detect_tag = false;
                self.push_text(buffered, &mut segments);
                continue;
            }

            run.push(ch);
            if ch == '\n' {
                self.push_text(std::mem::take(&mut run), &mut segments);
                self.detect_tag = true;
            }
        }

        if !run.is_empty() {
            self.push_text(run, &mut segments);
        }

        segments
    }

    /// Flush any buffered text and close an unterminated tag block.
    pub(crate) fn finish(&mut self) -> Vec<TaggedLineSegment<T>> {
        let mut segments = Vec::new();
        if !self.line_buffer.is_empty() {
            let buffered = std::mem::take(&mut self.line_buffer);
            self.process_line(buffered, &mut segments);
        }
        if let Some(tag) = self.active_tag.take() {
            push_segment(&mut segments, TaggedLineSegment::TagEnd(tag));
        }
        self.active_tag_indent.clear();
        self.nested_tag_depth = 0;
        self.markdown_fence = None;
        self.detect_tag = true;
        segments
    }

    fn finish_line(&mut self, segments: &mut Vec<TaggedLineSegment<T>>) {
        let line = std::mem::take(&mut self.line_buffer);
        self.process_line(line, segments);
        self.detect_tag = true;
    }

    fn process_line(&mut self, line: String, segments: &mut Vec<TaggedLineSegment<T>>) {
        let without_newline = line.strip_suffix('\n').unwrap_or(&line);
        let tag_slug = tag_line_slug(without_newline);
        let fence_slug = markdown_fence_slug(without_newline);

        if self.update_markdown_fence(fence_slug) {
            self.push_text(line, segments);
            return;
        }

        if self.markdown_fence.is_some() {
            self.push_text(line, segments);
            return;
        }

        if let Some(active_tag) = self.active_tag {
            if self.match_open(tag_slug) == Some(active_tag) {
                self.nested_tag_depth += 1;
                self.push_text(line, segments);
                return;
            }

            if self.match_close(tag_slug) == Some(active_tag) {
                if self.nested_tag_depth > 0 {
                    self.nested_tag_depth -= 1;
                    self.push_text(line, segments);
                    return;
                }
                if self.can_close_active_tag(without_newline) {
                    push_segment(segments, TaggedLineSegment::TagEnd(active_tag));
                    self.active_tag = None;
                    self.active_tag_indent.clear();
                    return;
                }
                self.push_text(line, segments);
                return;
            }

            self.push_text(line, segments);
            return;
        }

        if let Some(tag) = self.match_open(tag_slug) {
            push_segment(segments, TaggedLineSegment::TagStart(tag));
            self.active_tag = Some(tag);
            self.active_tag_indent = tag_line_indent(without_newline).to_string();
            self.nested_tag_depth = 0;
            return;
        }

        self.push_text(line, segments);
    }

    fn push_text(&self, text: String, segments: &mut Vec<TaggedLineSegment<T>>) {
        if let Some(tag) = self.active_tag {
            push_segment(segments, TaggedLineSegment::TagDelta(tag, text));
        } else {
            push_segment(segments, TaggedLineSegment::Normal(text));
        }
    }

    fn is_tag_prefix(&self, slug: &str) -> bool {
        let slug = slug.trim_end();
        self.specs
            .iter()
            .any(|spec| spec.open.starts_with(slug) || spec.close.starts_with(slug))
    }

    fn match_open(&self, slug: &str) -> Option<T> {
        self.specs
            .iter()
            .find(|spec| spec.open == slug)
            .map(|spec| spec.tag)
    }

    fn match_close(&self, slug: &str) -> Option<T> {
        self.specs
            .iter()
            .find(|spec| spec.close == slug)
            .map(|spec| spec.tag)
    }

    fn update_markdown_fence(&mut self, slug: Option<&str>) -> bool {
        if let Some(fence) = self.markdown_fence {
            if slug.is_some_and(|slug| fence_closes(slug, fence)) {
                self.markdown_fence = None;
                return true;
            }
            return false;
        }

        if let Some(slug) = slug
            && let Some(fence) = fence_opens(slug)
        {
            self.markdown_fence = Some(fence);
            return true;
        }

        false
    }

    fn can_close_active_tag(&self, line: &str) -> bool {
        let indent = tag_line_indent(line);
        indent == self.active_tag_indent || is_markdown_control_indent(indent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkdownFence {
    marker: char,
    len: usize,
}

fn fence_opens(slug: &str) -> Option<MarkdownFence> {
    let mut chars = slug.chars();
    let marker = chars.next()?;
    if marker != '`' && marker != '~' {
        return None;
    }

    let len = 1 + chars.take_while(|ch| *ch == marker).count();
    (len >= 3).then_some(MarkdownFence { marker, len })
}

fn tag_line_slug(line: &str) -> &str {
    line.trim_start().trim_end()
}

fn tag_line_indent(line: &str) -> &str {
    let slug = line.trim_start();
    &line[..line.len() - slug.len()]
}

fn is_markdown_control_indent(indent: &str) -> bool {
    indent.len() <= 3 && indent.bytes().all(|byte| byte == b' ')
}

fn markdown_fence_slug(line: &str) -> Option<&str> {
    let mut indent = 0;
    for (idx, byte) in line.bytes().enumerate() {
        match byte {
            b' ' if indent < 3 => indent += 1,
            b' ' => return None,
            b'\t' => return None,
            _ => return Some(line[idx..].trim_end()),
        }
    }
    Some("")
}

fn is_fence_prefix(slug: &str) -> bool {
    let slug = slug.trim_end();
    let Some(marker) = slug.chars().next() else {
        return false;
    };
    if marker != '`' && marker != '~' {
        return false;
    }
    let marker_len = slug.chars().take_while(|ch| *ch == marker).count();
    if marker_len >= 3 {
        return true;
    }
    slug.chars().all(|ch| ch == marker)
}

fn fence_closes(slug: &str, fence: MarkdownFence) -> bool {
    let len = slug.chars().take_while(|ch| *ch == fence.marker).count();
    len >= fence.len && slug.chars().skip(len).all(char::is_whitespace)
}

fn push_segment<T>(segments: &mut Vec<TaggedLineSegment<T>>, segment: TaggedLineSegment<T>)
where
    T: Copy + Eq,
{
    match segment {
        TaggedLineSegment::Normal(delta) => {
            if delta.is_empty() {
                return;
            }
            if let Some(TaggedLineSegment::Normal(existing)) = segments.last_mut() {
                existing.push_str(&delta);
                return;
            }
            segments.push(TaggedLineSegment::Normal(delta));
        }
        TaggedLineSegment::TagDelta(tag, delta) => {
            if delta.is_empty() {
                return;
            }
            if let Some(TaggedLineSegment::TagDelta(existing_tag, existing)) = segments.last_mut()
                && *existing_tag == tag
            {
                existing.push_str(&delta);
                return;
            }
            segments.push(TaggedLineSegment::TagDelta(tag, delta));
        }
        TaggedLineSegment::TagStart(tag) => {
            segments.push(TaggedLineSegment::TagStart(tag));
        }
        TaggedLineSegment::TagEnd(tag) => {
            segments.push(TaggedLineSegment::TagEnd(tag));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TagSpec;
    use super::TaggedLineParser;
    use super::TaggedLineSegment;
    use pretty_assertions::assert_eq;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Tag {
        Block,
    }

    fn parser() -> TaggedLineParser<Tag> {
        TaggedLineParser::new(vec![TagSpec {
            open: "<tag>",
            close: "</tag>",
            tag: Tag::Block,
        }])
    }

    #[test]
    fn buffers_prefix_until_tag_is_decided() {
        let mut parser = parser();
        let mut segments = parser.parse("<t");
        segments.extend(parser.parse("ag>\nline\n</tag>\n"));
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "line\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn rejects_tag_lines_with_extra_text() {
        let mut parser = parser();
        let mut segments = parser.parse("<tag> extra\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![TaggedLineSegment::Normal("<tag> extra\n".to_string())]
        );
    }

    #[test]
    fn closes_unterminated_tag_on_finish() {
        let mut parser = parser();
        let mut segments = parser.parse("<tag>\nline\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "line\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn accepts_tags_with_trailing_whitespace() {
        let mut parser = parser();
        let mut segments = parser.parse("<tag>   \nline\n</tag>  \n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "line\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn passes_through_plain_text() {
        let mut parser = parser();
        let mut segments = parser.parse("plain text\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![TaggedLineSegment::Normal("plain text\n".to_string())]
        );
    }

    #[test]
    fn ignores_tag_lines_inside_backtick_code_fences() {
        let mut parser = parser();
        let mut segments = parser.parse("<tag>\n```text\n</tag>\n```\nline\n</tag>\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "```text\n</tag>\n```\nline\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn ignores_tag_lines_inside_tilde_code_fences() {
        let mut parser = parser();
        let mut segments = parser.parse("<tag>\n~~~\n</tag>\n~~~\nline\n</tag>\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "~~~\n</tag>\n~~~\nline\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn treats_four_space_indented_fence_as_plain_text() {
        let mut parser = parser();
        let mut segments = parser.parse("<tag>\n    ```\n</tag>\nnormal");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "    ```\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
                TaggedLineSegment::Normal("normal".to_string()),
            ]
        );
    }

    #[test]
    fn ignores_four_space_indented_close_tag() {
        let mut parser = parser();
        let mut segments = parser.parse("<tag>\n    </tag>\nstill plan\n</tag>\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "    </tag>\nstill plan\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn ignores_nested_list_code_block_literal_tags() {
        let mut parser = parser();
        let mut segments = parser.parse(concat!(
            "<tag>\n",
            "  - example:\n",
            "     ```text\n",
            "     <tag>\n",
            "     </tag>\n",
            "     ```\n",
            "after\n",
            "</tag>\n",
        ));
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(
                    Tag::Block,
                    concat!(
                        "  - example:\n",
                        "     ```text\n",
                        "     <tag>\n",
                        "     </tag>\n",
                        "     ```\n",
                        "after\n",
                    )
                    .to_string()
                ),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn still_accepts_three_space_indented_tags() {
        let mut parser = parser();
        let mut segments = parser.parse("   <tag>\nline\n   </tag>\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "line\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn accepts_deeply_indented_outer_tag_delimiters() {
        let mut parser = parser();
        let mut segments = parser.parse("    <tag>\nline\n    </tag>\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "line\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn accepts_tab_indented_outer_tag_delimiters() {
        let mut parser = parser();
        let mut segments = parser.parse("\t<tag>\nline\n\t</tag>\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(Tag::Block, "line\n".to_string()),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn preserves_nested_literal_tags_inside_active_block() {
        let mut parser = parser();
        let mut segments = parser.parse(concat!(
            "<tag>\n", "before\n", "<tag>\n", "inner\n", "</tag>\n", "after\n", "</tag>\n",
        ));
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(
                    Tag::Block,
                    concat!("before\n", "<tag>\n", "inner\n", "</tag>\n", "after\n",).to_string()
                ),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn accepts_three_space_indented_code_fences() {
        let mut parser = parser();
        let mut segments = parser.parse("<tag>\n   ```text\n</tag>\n   ```\nline\n</tag>\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(
                    Tag::Block,
                    "   ```text\n</tag>\n   ```\nline\n".to_string()
                ),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }

    #[test]
    fn rejects_four_space_indented_closing_fence() {
        let mut parser = parser();
        let mut segments =
            parser.parse("<tag>\n```\n</tag>\n    ```\nstill code\n```\nline\n</tag>\n");
        segments.extend(parser.finish());

        assert_eq!(
            segments,
            vec![
                TaggedLineSegment::TagStart(Tag::Block),
                TaggedLineSegment::TagDelta(
                    Tag::Block,
                    "```\n</tag>\n    ```\nstill code\n```\nline\n".to_string()
                ),
                TaggedLineSegment::TagEnd(Tag::Block),
            ]
        );
    }
}
