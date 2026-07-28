use crate::product::tui_app::render::line_utils::line_to_static;
use crate::product::tui_app::wrapping::RtOptions;
use crate::product::tui_app::wrapping::word_wrap_line;
use crate::product::tui_app::wrapping::word_wrap_line_grapheme_safe;
use pulldown_cmark::Alignment;
use pulldown_cmark::BrokenLink;
use pulldown_cmark::BrokenLinkCallback;
use pulldown_cmark::CodeBlockKind;
use pulldown_cmark::CowStr;
use pulldown_cmark::Event;
use pulldown_cmark::HeadingLevel;
use pulldown_cmark::OffsetIter;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::RefDefs;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

mod table_key_value;
mod table_source;

const TABLE_COLUMN_GAP: usize = 2;
const TABLE_CELL_PADDING: usize = 1;
const TABLE_HEADER_SEPARATOR_CHAR: char = '━';
const TABLE_BODY_SEPARATOR_CHAR: char = '─';

struct MarkdownStyles {
    h1: Style,
    h2: Style,
    h3: Style,
    h4: Style,
    h5: Style,
    h6: Style,
    code: Style,
    emphasis: Style,
    strong: Style,
    strikethrough: Style,
    ordered_list_marker: Style,
    unordered_list_marker: Style,
    link: Style,
    blockquote: Style,
    table_header: Style,
    table_separator: Style,
}

impl Default for MarkdownStyles {
    fn default() -> Self {
        use ratatui::style::Stylize;

        Self {
            h1: Style::new().bold().underlined(),
            h2: Style::new().bold(),
            h3: Style::new().bold().italic(),
            h4: Style::new().italic(),
            h5: Style::new().italic(),
            h6: Style::new().italic(),
            code: Style::new().cyan(),
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().crossed_out(),
            ordered_list_marker: Style::new().light_blue(),
            unordered_list_marker: Style::new(),
            link: Style::new().cyan().underlined(),
            blockquote: Style::new().green(),
            table_header: Style::new().bold(),
            table_separator: Style::new().dim(),
        }
    }
}

#[derive(Clone, Debug)]
struct IndentContext {
    prefix: Vec<Span<'static>>,
    marker: Option<Vec<Span<'static>>>,
    is_list: bool,
}

impl IndentContext {
    fn new(prefix: Vec<Span<'static>>, marker: Option<Vec<Span<'static>>>, is_list: bool) -> Self {
        Self {
            prefix,
            marker,
            is_list,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct TableCell {
    lines: Vec<Line<'static>>,
}

impl TableCell {
    fn ensure_line(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(Line::default());
        }
    }

    fn push_span(&mut self, span: Span<'static>) {
        self.ensure_line();
        if let Some(line) = self.lines.last_mut() {
            line.push_span(span);
        }
    }

    fn hard_break(&mut self) {
        self.lines.push(Line::default());
    }

    fn plain_text(&self) -> String {
        let mut text = String::new();
        for (line_index, line) in self.lines.iter().enumerate() {
            if line_index > 0 {
                text.push(' ');
            }
            for span in &line.spans {
                text.push_str(&span.content);
            }
        }
        text
    }
}

#[derive(Debug)]
struct TableBodyRow {
    cells: Vec<TableCell>,
    has_table_pipe_syntax: bool,
    source_range: Range<usize>,
}

#[derive(Debug)]
struct TableState {
    alignments: Vec<Alignment>,
    header: Option<Vec<TableCell>>,
    rows: Vec<TableBodyRow>,
    current_row: Option<Vec<TableCell>>,
    current_row_has_table_pipe_syntax: bool,
    current_row_source_range: Option<Range<usize>>,
    current_cell: Option<TableCell>,
    in_header: bool,
}

impl TableState {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            header: None,
            rows: Vec::new(),
            current_row: None,
            current_row_has_table_pipe_syntax: false,
            current_row_source_range: None,
            current_cell: None,
            in_header: false,
        }
    }
}

struct RenderedTableLines {
    table_lines: Vec<Line<'static>>,
    table_lines_prewrapped: bool,
    spillover_lines: Vec<Line<'static>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableColumnKind {
    Narrative,
    TokenHeavy,
    Compact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TableColumnMetrics {
    max_width: usize,
    header_token_width: usize,
    body_token_width: usize,
    kind: TableColumnKind,
}

pub fn render_markdown_text(input: &str) -> Text<'static> {
    render_markdown_text_with_width(input, None)
}

pub(crate) fn render_markdown_text_with_width(input: &str, width: Option<usize>) -> Text<'static> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let (parser_input, inline_code_pipe_sentinel) = table_source::prepare_markdown(input, options);
    options.insert(Options::ENABLE_TABLES);
    render_prepared_markdown(
        parser_input.as_ref(),
        width,
        inline_code_pipe_sentinel,
        options,
    )
}

fn render_prepared_markdown(
    input: &str,
    width: Option<usize>,
    inline_code_pipe_sentinel: Option<char>,
    options: Options,
) -> Text<'static> {
    let parser = Parser::new_ext(input, options).into_offset_iter();
    let mut w = Writer::new(input, parser, width, inline_code_pipe_sentinel);
    w.run();
    w.text
}

trait MarkdownEventIterator<'a>: Iterator<Item = (Event<'a>, Range<usize>)> {
    // RefDefs is owned by the offset iterator, so access it without a self-referential borrow.
    fn reference_definitions(&self) -> &RefDefs<'_>;
}

impl<'a, F> MarkdownEventIterator<'a> for OffsetIter<'a, F>
where
    F: BrokenLinkCallback<'a>,
{
    fn reference_definitions(&self) -> &RefDefs<'_> {
        OffsetIter::reference_definitions(self)
    }
}

struct BrokenLinkCollector<'labels> {
    labels: &'labels mut HashSet<String>,
}

impl<'input> BrokenLinkCallback<'input> for BrokenLinkCollector<'_> {
    fn handle_broken_link(
        &mut self,
        link: BrokenLink<'input>,
    ) -> Option<(CowStr<'input>, CowStr<'input>)> {
        self.labels.insert(link.reference.into_string());
        None
    }
}

#[derive(Clone, Debug, Default)]
struct FragmentReferenceResolver {
    definitions: HashMap<String, (String, String)>,
}

impl FragmentReferenceResolver {
    fn from_fragment(source: &str, options: Options, reference_definitions: &RefDefs<'_>) -> Self {
        // Collect fragment-broken labels first, then own only the matching document definitions.
        let mut labels = HashSet::new();
        let collector = BrokenLinkCollector {
            labels: &mut labels,
        };
        Parser::new_with_broken_link_callback(source, options, Some(collector)).for_each(drop);

        let definitions = labels
            .into_iter()
            .filter_map(|label| {
                let definition = reference_definitions.get(&label)?;
                let destination = definition.dest.to_string();
                let title = definition
                    .title
                    .as_ref()
                    .map_or_else(String::new, ToString::to_string);
                Some((label, (destination, title)))
            })
            .collect();
        Self { definitions }
    }
}

impl<'input> BrokenLinkCallback<'input> for FragmentReferenceResolver {
    fn handle_broken_link(
        &mut self,
        link: BrokenLink<'input>,
    ) -> Option<(CowStr<'input>, CowStr<'input>)> {
        self.definitions
            .get(link.reference.as_ref())
            .map(|(destination, title)| {
                (
                    CowStr::from(destination.clone()),
                    CowStr::from(title.clone()),
                )
            })
    }
}

struct Writer<'a, I>
where
    I: MarkdownEventIterator<'a>,
{
    input: &'a str,
    inline_code_pipe_sentinel: Option<char>,
    iter: I,
    text: Text<'static>,
    styles: MarkdownStyles,
    inline_styles: Vec<Style>,
    indent_stack: Vec<IndentContext>,
    list_indices: Vec<Option<u64>>,
    link: Option<String>,
    needs_newline: bool,
    pending_marker_line: bool,
    in_paragraph: bool,
    in_code_block: bool,
    wrap_width: Option<usize>,
    current_line_content: Option<Line<'static>>,
    current_initial_indent: Vec<Span<'static>>,
    current_subsequent_indent: Vec<Span<'static>>,
    current_line_style: Style,
    current_line_in_code_block: bool,
    table_state: Option<TableState>,
}

impl<'a, I> Writer<'a, I>
where
    I: MarkdownEventIterator<'a>,
{
    fn new(
        input: &'a str,
        iter: I,
        wrap_width: Option<usize>,
        inline_code_pipe_sentinel: Option<char>,
    ) -> Self {
        Self {
            input,
            inline_code_pipe_sentinel,
            iter,
            text: Text::default(),
            styles: MarkdownStyles::default(),
            inline_styles: Vec::new(),
            indent_stack: Vec::new(),
            list_indices: Vec::new(),
            link: None,
            needs_newline: false,
            pending_marker_line: false,
            in_paragraph: false,
            in_code_block: false,
            wrap_width,
            current_line_content: None,
            current_initial_indent: Vec::new(),
            current_subsequent_indent: Vec::new(),
            current_line_style: Style::default(),
            current_line_in_code_block: false,
            table_state: None,
        }
    }

    fn run(&mut self) {
        while let Some((event, range)) = self.iter.next() {
            self.handle_event(event, range);
        }
        self.flush_current_line();
    }

    fn handle_event(&mut self, event: Event<'a>, range: Range<usize>) {
        match event {
            Event::Start(tag) => self.start_tag(tag, range),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(text),
            Event::Code(code) => self.code(code),
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => {
                self.flush_current_line();
                if !self.text.lines.is_empty() {
                    self.push_blank_line();
                }
                self.push_line(Line::from("———"));
                self.needs_newline = true;
            }
            Event::Html(html) => self.html(html, false),
            Event::InlineHtml(html) => self.html(html, true),
            Event::FootnoteReference(_) => {}
            Event::TaskListMarker(_) => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'a>, range: Range<usize>) {
        match tag {
            Tag::Paragraph => self.start_paragraph(),
            Tag::Heading { level, .. } => self.start_heading(level),
            Tag::BlockQuote => self.start_blockquote(),
            Tag::CodeBlock(kind) => {
                let indent = match kind {
                    CodeBlockKind::Fenced(_) => None,
                    CodeBlockKind::Indented => Some(Span::from(" ".repeat(4))),
                };
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => Some(lang.to_string()),
                    CodeBlockKind::Indented => None,
                };
                self.start_codeblock(lang, indent)
            }
            Tag::List(start) => self.start_list(start),
            Tag::Item => self.start_item(),
            Tag::Emphasis => self.push_inline_style(self.styles.emphasis),
            Tag::Strong => self.push_inline_style(self.styles.strong),
            Tag::Strikethrough => self.push_inline_style(self.styles.strikethrough),
            Tag::Link { dest_url, .. } => self.push_link(dest_url.to_string()),
            Tag::Table(alignments) => self.start_table(alignments),
            Tag::TableHead => self.start_table_head(),
            Tag::TableRow => self.start_table_row(range),
            Tag::TableCell => self.start_table_cell(),
            Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::Image { .. }
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.end_paragraph(),
            TagEnd::Heading(_) => self.end_heading(),
            TagEnd::BlockQuote => self.end_blockquote(),
            TagEnd::CodeBlock => self.end_codeblock(),
            TagEnd::List(_) => self.end_list(),
            TagEnd::Item => {
                self.indent_stack.pop();
                self.pending_marker_line = false;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_inline_style(),
            TagEnd::Link => self.pop_link(),
            TagEnd::Table => self.end_table(),
            TagEnd::TableHead => self.end_table_head(),
            TagEnd::TableRow => self.end_table_row(),
            TagEnd::TableCell => self.end_table_cell(),
            TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::Image
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn start_paragraph(&mut self) {
        if self.in_table_cell() {
            return;
        }
        if self.needs_newline {
            self.push_blank_line();
        }
        self.push_line(Line::default());
        self.needs_newline = false;
        self.in_paragraph = true;
    }

    fn end_paragraph(&mut self) {
        if self.in_table_cell() {
            return;
        }
        self.needs_newline = true;
        self.in_paragraph = false;
        self.pending_marker_line = false;
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        if self.in_table_cell() {
            return;
        }
        if self.needs_newline {
            self.push_line(Line::default());
            self.needs_newline = false;
        }
        let heading_style = match level {
            HeadingLevel::H1 => self.styles.h1,
            HeadingLevel::H2 => self.styles.h2,
            HeadingLevel::H3 => self.styles.h3,
            HeadingLevel::H4 => self.styles.h4,
            HeadingLevel::H5 => self.styles.h5,
            HeadingLevel::H6 => self.styles.h6,
        };
        let content = format!("{} ", "#".repeat(level as usize));
        self.push_line(Line::from(vec![Span::styled(content, heading_style)]));
        self.push_inline_style(heading_style);
        self.needs_newline = false;
    }

    fn end_heading(&mut self) {
        if self.in_table_cell() {
            return;
        }
        self.needs_newline = true;
        self.pop_inline_style();
    }

    fn start_blockquote(&mut self) {
        if self.in_table_cell() {
            return;
        }
        if self.needs_newline {
            self.push_blank_line();
            self.needs_newline = false;
        }
        self.indent_stack
            .push(IndentContext::new(vec![Span::from("> ")], None, false));
    }

    fn end_blockquote(&mut self) {
        if self.in_table_cell() {
            return;
        }
        self.indent_stack.pop();
        self.needs_newline = true;
    }

    fn text(&mut self, text: CowStr<'a>) {
        if self.in_table_cell() {
            self.push_text_to_table_cell(&text);
            return;
        }
        if self.pending_marker_line {
            self.push_line(Line::default());
        }
        self.pending_marker_line = false;
        if self.in_code_block && !self.needs_newline {
            let has_content = self
                .current_line_content
                .as_ref()
                .map(|line| !line.spans.is_empty())
                .unwrap_or_else(|| {
                    self.text
                        .lines
                        .last()
                        .map(|line| !line.spans.is_empty())
                        .unwrap_or(false)
                });
            if has_content {
                self.push_line(Line::default());
            }
        }
        for (i, line) in text.lines().enumerate() {
            if self.needs_newline {
                self.push_line(Line::default());
                self.needs_newline = false;
            }
            if i > 0 {
                self.push_line(Line::default());
            }
            let content = line.to_string();
            let span = Span::styled(
                content,
                self.inline_styles.last().copied().unwrap_or_default(),
            );
            self.push_span(span);
        }
        self.needs_newline = false;
    }

    fn code(&mut self, code: CowStr<'a>) {
        let mut code = code.into_string();
        if let Some(sentinel) = self.inline_code_pipe_sentinel
            && code.contains(sentinel)
        {
            code = code.replace(sentinel, "|");
        }
        if self.in_table_cell() {
            self.push_span_to_table_cell(Span::from(code).style(self.styles.code));
            return;
        }
        if self.pending_marker_line {
            self.push_line(Line::default());
            self.pending_marker_line = false;
        }
        let span = Span::from(code).style(self.styles.code);
        self.push_span(span);
    }

    fn html(&mut self, html: CowStr<'a>, inline: bool) {
        let html = strip_leading_html_comments_from_html(&html);
        if html.is_empty() {
            return;
        }
        if self.in_table_cell() {
            let style = self.inline_styles.last().copied().unwrap_or_default();
            for (line_index, line) in html.lines().enumerate() {
                if line_index > 0 {
                    self.push_table_cell_hard_break();
                }
                self.push_span_to_table_cell(Span::styled(line.to_string(), style));
            }
            if !inline {
                self.push_table_cell_hard_break();
            }
            return;
        }
        self.pending_marker_line = false;
        for (i, line) in html.lines().enumerate() {
            if self.needs_newline {
                self.push_line(Line::default());
                self.needs_newline = false;
            }
            if i > 0 {
                self.push_line(Line::default());
            }
            let style = self.inline_styles.last().copied().unwrap_or_default();
            self.push_span(Span::styled(line.to_string(), style));
        }
        self.needs_newline = !inline;
    }

    fn hard_break(&mut self) {
        if self.in_table_cell() {
            self.push_table_cell_hard_break();
            return;
        }
        self.push_line(Line::default());
    }

    fn soft_break(&mut self) {
        if self.in_table_cell() {
            let style = self.inline_styles.last().copied().unwrap_or_default();
            self.push_span_to_table_cell(Span::styled(" ".to_string(), style));
            return;
        }
        self.push_line(Line::default());
    }

    fn start_list(&mut self, index: Option<u64>) {
        if self.list_indices.is_empty() && self.needs_newline {
            self.push_line(Line::default());
        }
        self.list_indices.push(index);
    }

    fn end_list(&mut self) {
        self.list_indices.pop();
        self.needs_newline = true;
    }

    fn start_item(&mut self) {
        self.pending_marker_line = true;
        let depth = self.list_indices.len();
        let is_ordered = self
            .list_indices
            .last()
            .map(Option::is_some)
            .unwrap_or(false);
        let width = depth * 4 - 3;
        let marker = if let Some(last_index) = self.list_indices.last_mut() {
            match last_index {
                None => Some(vec![Span::styled(
                    " ".repeat(width - 1) + "- ",
                    self.styles.unordered_list_marker,
                )]),
                Some(index) => {
                    *index += 1;
                    Some(vec![Span::styled(
                        format!("{:width$}. ", *index - 1),
                        self.styles.ordered_list_marker,
                    )])
                }
            }
        } else {
            None
        };
        let indent_prefix = if depth == 0 {
            Vec::new()
        } else {
            let indent_len = if is_ordered { width + 2 } else { width + 1 };
            vec![Span::from(" ".repeat(indent_len))]
        };
        self.indent_stack
            .push(IndentContext::new(indent_prefix, marker, true));
        self.needs_newline = false;
    }

    fn start_codeblock(&mut self, _lang: Option<String>, indent: Option<Span<'static>>) {
        self.flush_current_line();
        if !self.text.lines.is_empty() {
            self.push_blank_line();
        }
        self.in_code_block = true;
        self.indent_stack.push(IndentContext::new(
            vec![indent.unwrap_or_default()],
            None,
            false,
        ));
        self.needs_newline = true;
    }

    fn end_codeblock(&mut self) {
        self.needs_newline = true;
        self.in_code_block = false;
        self.indent_stack.pop();
    }

    fn start_table(&mut self, alignments: Vec<Alignment>) {
        self.flush_current_line();
        if self.needs_newline {
            self.push_blank_line();
            self.needs_newline = false;
        }
        self.table_state = Some(TableState::new(alignments));
    }

    fn end_table(&mut self) {
        let Some(table_state) = self.table_state.take() else {
            return;
        };
        let RenderedTableLines {
            table_lines,
            table_lines_prewrapped,
            spillover_lines,
        } = self.render_table_lines(table_state);
        let mut pending_marker_line = self.pending_marker_line;
        for line in table_lines {
            if table_lines_prewrapped {
                self.push_prewrapped_line(line, pending_marker_line);
            } else {
                self.push_line(line);
                self.flush_current_line();
            }
            pending_marker_line = false;
        }
        self.pending_marker_line = false;
        for spillover_line in spillover_lines {
            self.push_line(spillover_line);
            self.flush_current_line();
        }
        self.needs_newline = true;
    }

    fn start_table_head(&mut self) {
        if let Some(table_state) = self.table_state.as_mut() {
            table_state.in_header = true;
            table_state.current_row = Some(Vec::new());
        }
    }

    fn end_table_head(&mut self) {
        let Some(table_state) = self.table_state.as_mut() else {
            return;
        };
        if let Some(current_cell) = table_state.current_cell.take() {
            table_state
                .current_row
                .get_or_insert_with(Vec::new)
                .push(current_cell);
        }
        if let Some(row) = table_state.current_row.take() {
            table_state.header = Some(row);
        }
        table_state.in_header = false;
    }

    fn start_table_row(&mut self, source_range: Range<usize>) {
        let has_table_pipe_syntax = self.has_table_row_boundary_pipe(source_range.clone());
        if let Some(table_state) = self.table_state.as_mut() {
            table_state.current_row = Some(Vec::new());
            table_state.current_row_has_table_pipe_syntax = has_table_pipe_syntax;
            table_state.current_row_source_range = Some(source_range);
        }
    }

    fn has_table_row_boundary_pipe(&self, source_range: Range<usize>) -> bool {
        let Some(source) = self.input.get(source_range) else {
            return false;
        };
        let source = source.trim();
        let has_trailing_boundary = source.ends_with('|')
            && !table_source::is_backslash_escaped(source, source.len().saturating_sub(1));
        source.starts_with('|') || has_trailing_boundary
    }

    fn end_table_row(&mut self) {
        let Some(table_state) = self.table_state.as_mut() else {
            return;
        };
        if let Some(current_cell) = table_state.current_cell.take() {
            table_state
                .current_row
                .get_or_insert_with(Vec::new)
                .push(current_cell);
        }
        let Some(row) = table_state.current_row.take() else {
            return;
        };
        let source_range = table_state.current_row_source_range.take();
        if table_state.in_header {
            table_state.header = Some(row);
        } else {
            table_state.rows.push(TableBodyRow {
                cells: row,
                has_table_pipe_syntax: table_state.current_row_has_table_pipe_syntax,
                source_range: source_range.unwrap_or_default(),
            });
        }
        table_state.current_row_has_table_pipe_syntax = false;
    }

    fn start_table_cell(&mut self) {
        if let Some(table_state) = self.table_state.as_mut() {
            table_state.current_cell = Some(TableCell::default());
        }
    }

    fn end_table_cell(&mut self) {
        let Some(table_state) = self.table_state.as_mut() else {
            return;
        };
        if let Some(cell) = table_state.current_cell.take() {
            table_state
                .current_row
                .get_or_insert_with(Vec::new)
                .push(cell);
        }
    }

    fn in_table_cell(&self) -> bool {
        self.table_state
            .as_ref()
            .and_then(|table_state| table_state.current_cell.as_ref())
            .is_some()
    }

    fn push_span_to_table_cell(&mut self, span: Span<'static>) {
        if let Some(table_state) = self.table_state.as_mut()
            && let Some(cell) = table_state.current_cell.as_mut()
        {
            cell.push_span(span);
        }
    }

    fn push_table_cell_hard_break(&mut self) {
        if let Some(table_state) = self.table_state.as_mut()
            && let Some(cell) = table_state.current_cell.as_mut()
        {
            cell.hard_break();
        }
    }

    fn push_text_to_table_cell(&mut self, text: &str) {
        let style = self.inline_styles.last().copied().unwrap_or_default();
        for (line_index, line) in text.lines().enumerate() {
            if line_index > 0 {
                self.push_table_cell_hard_break();
            }
            self.push_span_to_table_cell(Span::styled(line.to_string(), style));
        }
    }

    fn render_table_lines(&self, mut table_state: TableState) -> RenderedTableLines {
        let column_count = table_state.alignments.len();
        if column_count == 0 {
            return RenderedTableLines {
                table_lines: Vec::new(),
                table_lines_prewrapped: true,
                spillover_lines: Vec::new(),
            };
        }

        let mut spillover_lines = Vec::new();
        let mut rows = Vec::with_capacity(table_state.rows.len());
        let mut in_spillover = false;
        for (row_index, row) in table_state.rows.iter().enumerate() {
            let next_row = table_state.rows.get(row_index + 1);
            if in_spillover || column_count > 1 && Self::is_spillover_row(row, next_row) {
                in_spillover = true;
                spillover_lines.extend(self.render_spillover_row_source(row));
            } else {
                rows.push(row.cells.clone());
            }
        }

        let mut header = table_state
            .header
            .take()
            .unwrap_or_else(|| vec![TableCell::default(); column_count]);
        Self::normalize_row(&mut header, column_count);
        for row in &mut rows {
            Self::normalize_row(row, column_count);
        }

        let metrics = Self::collect_table_column_metrics(&header, &rows, column_count);
        let widths =
            Self::compute_column_widths(&metrics, self.available_table_width(column_count));

        let Some(column_widths) = widths else {
            if !rows.is_empty() {
                return RenderedTableLines {
                    table_lines: table_key_value::render_records(
                        &header,
                        &rows,
                        &metrics,
                        self.available_record_width(),
                        self.styles.table_header,
                        self.styles.table_separator,
                    ),
                    table_lines_prewrapped: true,
                    spillover_lines,
                };
            }
            return RenderedTableLines {
                table_lines: self.render_table_pipe_fallback(
                    &header,
                    &rows,
                    &table_state.alignments,
                ),
                table_lines_prewrapped: false,
                spillover_lines,
            };
        };

        if table_key_value::should_render_records(&rows, &column_widths, &metrics) {
            return RenderedTableLines {
                table_lines: table_key_value::render_records(
                    &header,
                    &rows,
                    &metrics,
                    self.available_record_width(),
                    self.styles.table_header,
                    self.styles.table_separator,
                ),
                table_lines_prewrapped: true,
                spillover_lines,
            };
        }

        let mut table_lines = Vec::with_capacity(2 + rows.len() * 2);
        table_lines.extend(self.render_table_row(
            &header,
            &column_widths,
            &table_state.alignments,
            self.styles.table_header,
        ));
        table_lines.push(Self::render_table_separator(
            &column_widths,
            TABLE_HEADER_SEPARATOR_CHAR,
            self.styles.table_separator,
        ));
        for (row_index, row) in rows.iter().enumerate() {
            table_lines.extend(self.render_table_row(
                row,
                &column_widths,
                &table_state.alignments,
                Style::default(),
            ));
            if row_index + 1 < rows.len() {
                table_lines.push(Self::render_table_separator(
                    &column_widths,
                    TABLE_BODY_SEPARATOR_CHAR,
                    self.styles.table_separator,
                ));
            }
        }

        RenderedTableLines {
            table_lines,
            table_lines_prewrapped: true,
            spillover_lines,
        }
    }

    fn normalize_row(row: &mut Vec<TableCell>, column_count: usize) {
        row.truncate(column_count);
        row.resize(column_count, TableCell::default());
    }

    fn available_table_width(&self, column_count: usize) -> Option<usize> {
        self.wrap_width.map(|wrap_width| {
            let prefix_width =
                Self::spans_display_width(&self.prefix_spans(self.pending_marker_line));
            let reserved = prefix_width
                + (column_count.saturating_sub(1) * TABLE_COLUMN_GAP)
                + (column_count * TABLE_CELL_PADDING * 2);
            wrap_width.saturating_sub(reserved)
        })
    }

    fn available_record_width(&self) -> Option<usize> {
        self.wrap_width.map(|wrap_width| {
            let prefix_width =
                Self::spans_display_width(&self.prefix_spans(self.pending_marker_line));
            wrap_width.saturating_sub(prefix_width)
        })
    }

    fn compute_column_widths(
        metrics: &[TableColumnMetrics],
        available_width: Option<usize>,
    ) -> Option<Vec<usize>> {
        let min_column_width = 3;
        let mut widths: Vec<usize> = metrics
            .iter()
            .map(|column| column.max_width.max(min_column_width))
            .collect();

        let Some(max_width) = available_width else {
            return Some(widths);
        };
        let minimum_total = metrics.len() * min_column_width;
        if max_width < minimum_total {
            return None;
        }

        let mut floors: Vec<usize> = metrics
            .iter()
            .map(|column| Self::preferred_column_floor(column, min_column_width))
            .collect();
        let floor_total: usize = floors.iter().sum();
        if floor_total > max_width {
            let minimums = vec![min_column_width; floors.len()];
            Self::shrink_columns(&mut floors, &minimums, metrics, floor_total - max_width);
        }

        let total_width: usize = widths.iter().sum();
        if total_width > max_width {
            let remaining =
                Self::shrink_columns(&mut widths, &floors, metrics, total_width - max_width);
            if remaining > 0 {
                return None;
            }
        }

        Some(widths)
    }

    fn collect_table_column_metrics(
        header: &[TableCell],
        rows: &[Vec<TableCell>],
        column_count: usize,
    ) -> Vec<TableColumnMetrics> {
        let mut metrics = Vec::with_capacity(column_count);
        for column in 0..column_count {
            let header_cell = &header[column];
            let header_plain = header_cell.plain_text();
            let header_token_width = Self::longest_token_width(&header_plain);
            let mut max_width = Self::cell_display_width(header_cell);
            let mut body_token_width = 0usize;
            let mut body_token_count = 0usize;
            let mut long_body_token_count = 0usize;
            let mut total_words = 0usize;
            let mut total_cells = 0usize;
            let mut total_cell_width = 0usize;

            for row in rows {
                let cell = &row[column];
                max_width = max_width.max(Self::cell_display_width(cell));
                let plain = cell.plain_text();
                let mut word_count = 0;
                for token in plain.split_whitespace() {
                    let token_width = token.width();
                    body_token_width = body_token_width.max(token_width);
                    long_body_token_count += usize::from(Self::is_token_heavy_token(token));
                    word_count += 1;
                }
                if word_count > 0 {
                    body_token_count += word_count;
                    total_words += word_count;
                    total_cells += 1;
                    total_cell_width += plain.width();
                }
            }

            let avg_words_per_cell = if total_cells == 0 {
                header_plain.split_whitespace().count() as f64
            } else {
                total_words as f64 / total_cells as f64
            };
            let avg_cell_width = if total_cells == 0 {
                header_plain.width() as f64
            } else {
                total_cell_width as f64 / total_cells as f64
            };
            let kind = if long_body_token_count > 0
                && long_body_token_count >= body_token_count.saturating_sub(long_body_token_count)
            {
                TableColumnKind::TokenHeavy
            } else if avg_words_per_cell >= 4.0 || avg_cell_width >= 28.0 {
                TableColumnKind::Narrative
            } else {
                TableColumnKind::Compact
            };

            metrics.push(TableColumnMetrics {
                max_width,
                header_token_width,
                body_token_width,
                kind,
            });
        }

        metrics
    }

    fn preferred_column_floor(metrics: &TableColumnMetrics, min_column_width: usize) -> usize {
        let token_target = match metrics.kind {
            TableColumnKind::Narrative | TableColumnKind::TokenHeavy => 16,
            TableColumnKind::Compact => metrics
                .header_token_width
                .max(metrics.body_token_width.min(16)),
        };
        token_target.max(min_column_width).min(metrics.max_width)
    }

    fn shrink_columns(
        widths: &mut [usize],
        floors: &[usize],
        metrics: &[TableColumnMetrics],
        mut amount: usize,
    ) -> usize {
        for kind in [
            TableColumnKind::TokenHeavy,
            TableColumnKind::Narrative,
            TableColumnKind::Compact,
        ] {
            let slack_total = widths
                .iter()
                .enumerate()
                .filter(|(index, _)| metrics[*index].kind == kind)
                .map(|(index, width)| width.saturating_sub(floors[index]))
                .sum::<usize>();
            let to_remove = amount.min(slack_total);
            if to_remove == 0 {
                continue;
            }

            let mut low = 0;
            let mut high = widths
                .iter()
                .enumerate()
                .filter(|(index, _)| metrics[*index].kind == kind)
                .map(|(index, width)| width.saturating_sub(floors[index]))
                .max()
                .unwrap_or(0);
            while low < high {
                let cap = low + (high - low) / 2;
                let removed = widths
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| metrics[*index].kind == kind)
                    .map(|(index, width)| width.saturating_sub(floors[index]).saturating_sub(cap))
                    .sum::<usize>();
                if removed > to_remove {
                    low = cap + 1;
                } else {
                    high = cap;
                }
            }

            let cap = low;
            let mut removed = 0;
            for (index, width) in widths.iter_mut().enumerate() {
                if metrics[index].kind != kind {
                    continue;
                }
                let reduction = width.saturating_sub(floors[index]).saturating_sub(cap);
                *width -= reduction;
                removed += reduction;
            }

            let mut remainder = to_remove - removed;
            for (index, width) in widths.iter_mut().enumerate() {
                if remainder == 0 {
                    break;
                }
                if metrics[index].kind == kind && width.saturating_sub(floors[index]) == cap {
                    *width -= 1;
                    remainder -= 1;
                }
            }

            amount -= to_remove;
            if amount == 0 {
                break;
            }
        }

        amount
    }

    fn render_table_separator(
        column_widths: &[usize],
        separator_char: char,
        style: Style,
    ) -> Line<'static> {
        let segment_char = separator_char.to_string();
        let gap = " ".repeat(TABLE_COLUMN_GAP);
        let text = column_widths
            .iter()
            .map(|width| segment_char.repeat(*width + (TABLE_CELL_PADDING * 2)))
            .collect::<Vec<_>>()
            .join(&gap);
        Line::from(Span::styled(text, style))
    }

    fn render_table_row(
        &self,
        row: &[TableCell],
        column_widths: &[usize],
        alignments: &[Alignment],
        row_style: Style,
    ) -> Vec<Line<'static>> {
        let wrapped_cells: Vec<Vec<Line<'static>>> = row
            .iter()
            .zip(column_widths)
            .map(|(cell, width)| self.wrap_cell(cell, *width))
            .collect();
        let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1);

        let mut output = Vec::with_capacity(row_height);
        for row_line in 0..row_height {
            let Some(last_visible_column) = wrapped_cells.iter().rposition(|lines| {
                lines
                    .get(row_line)
                    .is_some_and(|line| Self::line_display_width(line) > 0)
            }) else {
                output.push(Line::default());
                continue;
            };

            let mut spans = Vec::new();
            for (column, width) in column_widths
                .iter()
                .enumerate()
                .take(last_visible_column + 1)
            {
                spans.push(Span::raw(" ".repeat(TABLE_CELL_PADDING)));
                let mut line = wrapped_cells[column]
                    .get(row_line)
                    .cloned()
                    .unwrap_or_default();
                for span in &mut line.spans {
                    span.style = span.style.patch(row_style);
                }
                let line_width = Self::line_display_width(&line);
                let remaining = width.saturating_sub(line_width);
                let (left_padding, right_padding) = match alignments[column] {
                    Alignment::Left | Alignment::None => (0, remaining),
                    Alignment::Center => (remaining / 2, remaining - (remaining / 2)),
                    Alignment::Right => (remaining, 0),
                };
                if left_padding > 0 {
                    spans.push(Span::raw(" ".repeat(left_padding)));
                }
                spans.append(&mut line.spans);
                let is_last_column = column == last_visible_column;
                if right_padding > 0 && !is_last_column {
                    spans.push(Span::raw(" ".repeat(right_padding)));
                }
                if !is_last_column {
                    spans.push(Span::raw(" ".repeat(TABLE_CELL_PADDING)));
                    spans.push(Span::raw(" ".repeat(TABLE_COLUMN_GAP)));
                }
            }
            output.push(Line::from(spans));
        }

        output
    }

    fn render_table_pipe_fallback(
        &self,
        header: &[TableCell],
        rows: &[Vec<TableCell>],
        alignments: &[Alignment],
    ) -> Vec<Line<'static>> {
        let mut output = vec![
            Self::render_pipe_fallback_row(header),
            Line::from(Self::alignments_to_pipe_delimiter(alignments)),
        ];
        output.extend(rows.iter().map(|row| Self::render_pipe_fallback_row(row)));
        output
    }

    fn render_pipe_fallback_row(row: &[TableCell]) -> Line<'static> {
        let mut spans = vec!["|".into()];
        for cell in row {
            spans.push(" ".into());
            for (line_index, line) in cell.lines.iter().enumerate() {
                if line_index > 0 {
                    spans.push(" ".into());
                }
                spans.extend(line.spans.iter().cloned());
            }
            spans.push(" |".into());
        }
        Line::from(spans)
    }

    fn alignments_to_pipe_delimiter(alignments: &[Alignment]) -> String {
        let mut output = String::from("|");
        for alignment in alignments {
            let segment = match alignment {
                Alignment::Left => ":---",
                Alignment::Center => ":---:",
                Alignment::Right => "---:",
                Alignment::None => "---",
            };
            output.push_str(segment);
            output.push('|');
        }
        output
    }

    fn wrap_cell(&self, cell: &TableCell, width: usize) -> Vec<Line<'static>> {
        if cell.lines.is_empty() {
            return vec![Line::default()];
        }
        let mut wrapped = Vec::new();
        for source_line in &cell.lines {
            let rendered = word_wrap_line_grapheme_safe(source_line, width);
            if rendered.is_empty() {
                wrapped.push(Line::default());
            } else {
                wrapped.extend(rendered);
            }
        }
        if wrapped.is_empty() {
            wrapped.push(Line::default());
        }
        wrapped
    }

    fn is_spillover_row(row: &TableBodyRow, _next_row: Option<&TableBodyRow>) -> bool {
        !row.has_table_pipe_syntax && Self::first_non_empty_only_text(&row.cells).is_some()
    }

    fn render_spillover_row_source(&self, row: &TableBodyRow) -> Vec<Line<'static>> {
        let input = self.input;
        if let Some(source) = input.get(row.source_range.clone()) {
            let mut options = Options::empty();
            options.insert(Options::ENABLE_STRIKETHROUGH);
            let resolver = FragmentReferenceResolver::from_fragment(
                source,
                options,
                self.iter.reference_definitions(),
            );
            let parser = Parser::new_with_broken_link_callback(source, options, Some(resolver))
                .into_offset_iter();
            let mut writer = Writer::new(source, parser, None, self.inline_code_pipe_sentinel);
            writer.run();
            return writer.text.lines;
        }
        Self::spillover_row_fallback(row)
    }

    fn spillover_row_fallback(row: &TableBodyRow) -> Vec<Line<'static>> {
        if let Some(cell) = row.cells.first()
            && Self::first_non_empty_only_text(&row.cells).is_some()
        {
            return cell.lines.clone();
        }
        let mut spans = Vec::new();
        for (cell_index, cell) in row.cells.iter().enumerate() {
            if cell_index > 0 {
                spans.push(" | ".into());
            }
            for (line_index, line) in cell.lines.iter().enumerate() {
                if line_index > 0 {
                    spans.push(" ".into());
                }
                spans.extend(line.spans.iter().cloned());
            }
        }
        vec![Line::from(spans)]
    }

    fn first_non_empty_only_text(row: &[TableCell]) -> Option<String> {
        let first = row.first()?.plain_text();
        if first.trim().is_empty() {
            return None;
        }
        row[1..]
            .iter()
            .all(|cell| cell.plain_text().trim().is_empty())
            .then_some(first)
    }

    fn spans_display_width(spans: &[Span<'_>]) -> usize {
        spans.iter().map(|span| span.content.width()).sum()
    }

    fn line_display_width(line: &Line<'_>) -> usize {
        Self::spans_display_width(&line.spans)
    }

    fn cell_display_width(cell: &TableCell) -> usize {
        cell.lines
            .iter()
            .map(Self::line_display_width)
            .max()
            .unwrap_or(0)
    }

    fn longest_token_width(text: &str) -> usize {
        text.split_whitespace().map(str::width).max().unwrap_or(0)
    }

    fn is_token_heavy_token(token: &str) -> bool {
        token.width() >= 20
            && (token.is_ascii()
                || token.contains('/')
                || token.contains('\\')
                || token.contains("::"))
    }

    fn push_inline_style(&mut self, style: Style) {
        let current = self.inline_styles.last().copied().unwrap_or_default();
        let merged = current.patch(style);
        self.inline_styles.push(merged);
    }

    fn pop_inline_style(&mut self) {
        self.inline_styles.pop();
    }

    fn push_link(&mut self, dest_url: String) {
        self.link = Some(dest_url);
    }

    fn pop_link(&mut self) {
        if let Some(link) = self.link.take() {
            if self.in_table_cell() {
                self.push_span_to_table_cell(" (".into());
                self.push_span_to_table_cell(Span::styled(link, self.styles.link));
                self.push_span_to_table_cell(")".into());
            } else {
                self.push_span(" (".into());
                self.push_span(Span::styled(link, self.styles.link));
                self.push_span(")".into());
            }
        }
    }

    fn flush_current_line(&mut self) {
        if let Some(line) = self.current_line_content.take() {
            let style = self.current_line_style;
            // NB we don't wrap code in code blocks, in order to preserve whitespace for copy/paste.
            if !self.current_line_in_code_block
                && let Some(width) = self.wrap_width
            {
                let opts = RtOptions::new(width)
                    .initial_indent(self.current_initial_indent.clone().into())
                    .subsequent_indent(self.current_subsequent_indent.clone().into());
                for wrapped in word_wrap_line(&line, opts) {
                    let owned = line_to_static(&wrapped).style(style);
                    self.text.lines.push(owned);
                }
            } else {
                let mut spans = self.current_initial_indent.clone();
                let mut line = line;
                spans.append(&mut line.spans);
                self.text.lines.push(Line::from_iter(spans).style(style));
            }
            self.current_initial_indent.clear();
            self.current_subsequent_indent.clear();
            self.current_line_in_code_block = false;
        }
    }

    fn is_blockquote_active(&self) -> bool {
        self.indent_stack
            .iter()
            .any(|context| context.prefix.iter().any(|span| span.content.contains('>')))
    }

    fn push_prewrapped_line(&mut self, mut line: Line<'static>, pending_marker_line: bool) {
        self.flush_current_line();
        let style = if self.is_blockquote_active() {
            self.styles.blockquote.patch(line.style)
        } else {
            line.style
        };
        let mut spans = self.prefix_spans(pending_marker_line);
        spans.append(&mut line.spans);
        self.text.lines.push(Line::from(spans).style(style));
    }

    fn push_line(&mut self, line: Line<'static>) {
        self.flush_current_line();
        let style = if self.is_blockquote_active() {
            self.styles.blockquote
        } else {
            line.style
        };
        let was_pending = self.pending_marker_line;

        self.current_initial_indent = self.prefix_spans(was_pending);
        self.current_subsequent_indent = self.prefix_spans(false);
        self.current_line_style = style;
        self.current_line_content = Some(line);
        self.current_line_in_code_block = self.in_code_block;

        self.pending_marker_line = false;
    }

    fn push_span(&mut self, span: Span<'static>) {
        if let Some(line) = self.current_line_content.as_mut() {
            line.push_span(span);
        } else {
            self.push_line(Line::from(vec![span]));
        }
    }

    fn push_blank_line(&mut self) {
        self.flush_current_line();
        if self.indent_stack.iter().all(|ctx| ctx.is_list) {
            self.text.lines.push(Line::default());
        } else {
            self.push_line(Line::default());
            self.flush_current_line();
        }
    }

    fn prefix_spans(&self, pending_marker_line: bool) -> Vec<Span<'static>> {
        let mut prefix: Vec<Span<'static>> = Vec::new();
        let last_marker_index = if pending_marker_line {
            self.indent_stack
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, ctx)| if ctx.marker.is_some() { Some(i) } else { None })
        } else {
            None
        };
        let last_list_index = self.indent_stack.iter().rposition(|ctx| ctx.is_list);

        for (i, ctx) in self.indent_stack.iter().enumerate() {
            if pending_marker_line {
                if Some(i) == last_marker_index
                    && let Some(marker) = &ctx.marker
                {
                    prefix.extend(marker.iter().cloned());
                    continue;
                }
                if ctx.is_list && last_marker_index.is_some_and(|idx| idx > i) {
                    continue;
                }
            } else if ctx.is_list && Some(i) != last_list_index {
                continue;
            }
            prefix.extend(ctx.prefix.iter().cloned());
        }

        prefix
    }
}

fn strip_leading_html_comments_from_html(mut html: &str) -> &str {
    let mut stripped = false;
    loop {
        let trimmed = html.trim_start();
        if !trimmed.starts_with("<!--") {
            return if stripped { trimmed } else { html };
        }
        let Some(end) = trimmed.find("-->").map(|index| index + "-->".len()) else {
            return html;
        };
        stripped = true;
        html = &trimmed[end..];
    }
}

#[cfg(test)]
mod markdown_render_tests {
    include!("markdown_render_tests.rs");
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::style::Stylize;
    use ratatui::text::Text;

    fn lines_to_strings(text: &Text<'_>) -> Vec<String> {
        text.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn wraps_plain_text_when_width_provided() {
        let markdown = "This is a simple sentence that should wrap.";
        let rendered = render_markdown_text_with_width(markdown, Some(16));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "This is a simple".to_string(),
                "sentence that".to_string(),
                "should wrap.".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_list_items_preserving_indent() {
        let markdown = "- first second third fourth";
        let rendered = render_markdown_text_with_width(markdown, Some(14));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec!["- first second".to_string(), "  third fourth".to_string(),]
        );
    }

    #[test]
    fn wraps_nested_lists() {
        let markdown =
            "- outer item with several words to wrap\n  - inner item that also needs wrapping";
        let rendered = render_markdown_text_with_width(markdown, Some(20));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "- outer item with".to_string(),
                "  several words to".to_string(),
                "  wrap".to_string(),
                "    - inner item".to_string(),
                "      that also".to_string(),
                "      needs wrapping".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_ordered_lists() {
        let markdown = "1. ordered item contains many words for wrapping";
        let rendered = render_markdown_text_with_width(markdown, Some(18));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "1. ordered item".to_string(),
                "   contains many".to_string(),
                "   words for".to_string(),
                "   wrapping".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_blockquotes() {
        let markdown = "> block quote with content that should wrap nicely";
        let rendered = render_markdown_text_with_width(markdown, Some(22));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "> block quote with".to_string(),
                "> content that should".to_string(),
                "> wrap nicely".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_blockquotes_inside_lists() {
        let markdown = "- list item\n  > block quote inside list that wraps";
        let rendered = render_markdown_text_with_width(markdown, Some(24));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "- list item".to_string(),
                "  > block quote inside".to_string(),
                "  > list that wraps".to_string(),
            ]
        );
    }

    #[test]
    fn wraps_list_items_containing_blockquotes() {
        let markdown = "1. item with quote\n   > quoted text that should wrap";
        let rendered = render_markdown_text_with_width(markdown, Some(24));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec![
                "1. item with quote".to_string(),
                "   > quoted text that".to_string(),
                "   > should wrap".to_string(),
            ]
        );
    }

    #[test]
    fn does_not_wrap_code_blocks() {
        let markdown = "````\nfn main() { println!(\"hi from a long line\"); }\n````";
        let rendered = render_markdown_text_with_width(markdown, Some(10));
        let lines = lines_to_strings(&rendered);
        assert_eq!(
            lines,
            vec!["fn main() { println!(\"hi from a long line\"); }".to_string(),]
        );
    }

    type TestWriter<'a> = Writer<'a, OffsetIter<'a, pulldown_cmark::DefaultBrokenLinkCallback>>;

    fn make_cell(text: &str) -> TableCell {
        let mut cell = TableCell::default();
        cell.push_span(Span::raw(text.to_string()));
        cell
    }

    fn make_body_row(cells: Vec<TableCell>, has_table_pipe_syntax: bool) -> TableBodyRow {
        TableBodyRow {
            cells,
            has_table_pipe_syntax,
            source_range: 0..0,
        }
    }

    #[test]
    fn normalize_row_pads_and_truncates() {
        let mut short = vec![make_cell("one")];
        TestWriter::normalize_row(&mut short, 3);
        assert_eq!(
            short,
            vec![make_cell("one"), TableCell::default(), TableCell::default(),]
        );

        let mut long = vec![
            make_cell("one"),
            make_cell("two"),
            make_cell("three"),
            make_cell("ignored"),
        ];
        TestWriter::normalize_row(&mut long, 3);
        assert_eq!(
            long,
            vec![make_cell("one"), make_cell("two"), make_cell("three")]
        );
    }

    #[test]
    fn column_widths_fit_budget_or_fall_back_below_minimum() {
        let metrics = [
            TableColumnMetrics {
                max_width: 40,
                header_token_width: 4,
                body_token_width: 40,
                kind: TableColumnKind::TokenHeavy,
            },
            TableColumnMetrics {
                max_width: 30,
                header_token_width: 11,
                body_token_width: 10,
                kind: TableColumnKind::Narrative,
            },
            TableColumnMetrics {
                max_width: 8,
                header_token_width: 6,
                body_token_width: 8,
                kind: TableColumnKind::Compact,
            },
        ];

        let widths =
            TestWriter::compute_column_widths(&metrics, Some(48)).expect("table should fit");
        assert_eq!(widths.iter().sum::<usize>(), 48);
        assert!(widths.iter().all(|width| *width >= 3));
        assert_eq!(TestWriter::compute_column_widths(&metrics, Some(8)), None);
    }

    #[test]
    fn wrap_cell_preserves_hard_breaks_styles_and_display_width() {
        let mut cell = TableCell::default();
        cell.push_span(Span::styled("中中", Style::new().bold()));
        cell.hard_break();
        cell.push_span(Span::styled("👨‍💻", Style::new().italic()));
        cell.push_span(Span::styled(" code", Style::new().cyan()));

        let writer = TestWriter::new("", Parser::new("").into_offset_iter(), Some(80), None);
        let wrapped = writer.wrap_cell(&cell, 4);
        let rendered = wrapped
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["中中", "👨‍💻", "code"]);
        assert!(wrapped.iter().all(|line| line.width() <= 4));
        assert!(
            wrapped[0].spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert!(
            wrapped[1].spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::ITALIC)
        );
        assert_eq!(
            wrapped[2].spans[0].style.fg,
            Some(ratatui::style::Color::Cyan)
        );
    }

    #[test]
    fn trailing_table_boundary_uses_backslash_parity() {
        for backslash_count in 0..=5 {
            let source = format!("value{}|", "\\".repeat(backslash_count));
            let writer =
                TestWriter::new(&source, Parser::new(&source).into_offset_iter(), None, None);

            assert_eq!(
                writer.has_table_row_boundary_pipe(0..source.len()),
                backslash_count % 2 == 0,
                "{backslash_count}: {source:?}"
            );
            assert_eq!(
                table_source::is_backslash_escaped(&source, source.len() - 1),
                backslash_count % 2 == 1,
                "{backslash_count}: {source:?}"
            );
        }
    }

    #[test]
    fn spillover_classification_is_table_driven() {
        struct Case {
            name: &'static str,
            row: TableBodyRow,
            next: Option<TableBodyRow>,
            expected: bool,
        }

        let cases = [
            Case {
                name: "plain paragraph",
                row: make_body_row(vec![make_cell("ordinary paragraph")], false),
                next: None,
                expected: true,
            },
            Case {
                name: "explicit sparse pipe row",
                row: make_body_row(vec![make_cell("sparse value")], true),
                next: None,
                expected: false,
            },
            Case {
                name: "html content",
                row: make_body_row(vec![make_cell("<div>content</div>"), make_cell("")], false),
                next: None,
                expected: true,
            },
            Case {
                name: "explicit sparse html row",
                row: make_body_row(vec![make_cell("<div>content</div>"), make_cell("")], true),
                next: None,
                expected: false,
            },
            Case {
                name: "html label before block",
                row: make_body_row(vec![make_cell("HTML block:"), make_cell("")], false),
                next: Some(make_body_row(
                    vec![make_cell("<div>content</div>"), make_cell("")],
                    false,
                )),
                expected: true,
            },
            Case {
                name: "ordinary sparse label",
                row: make_body_row(vec![make_cell("Status:"), make_cell("")], true),
                next: Some(make_body_row(vec![make_cell("ready"), make_cell("")], true)),
                expected: false,
            },
        ];

        for case in cases {
            assert_eq!(
                TestWriter::is_spillover_row(&case.row, case.next.as_ref()),
                case.expected,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn column_compression_prefers_token_heavy_then_narrative_then_compact() {
        let metrics = [
            TableColumnMetrics {
                max_width: 40,
                header_token_width: 4,
                body_token_width: 40,
                kind: TableColumnKind::TokenHeavy,
            },
            TableColumnMetrics {
                max_width: 30,
                header_token_width: 11,
                body_token_width: 10,
                kind: TableColumnKind::Narrative,
            },
            TableColumnMetrics {
                max_width: 8,
                header_token_width: 6,
                body_token_width: 8,
                kind: TableColumnKind::Compact,
            },
        ];

        assert_eq!(
            TestWriter::compute_column_widths(&metrics, Some(48)),
            Some(vec![16, 24, 8])
        );
    }

    #[test]
    fn long_cjk_prose_is_classified_as_narrative() {
        let header = vec![make_cell("说明")];
        let rows = vec![vec![make_cell(
            "这是一段没有空格但仍应按照自然语言说明列处理的中文内容",
        )]];

        let metrics = TestWriter::collect_table_column_metrics(&header, &rows, 1);
        assert_eq!(metrics[0].kind, TableColumnKind::Narrative);
    }

    #[test]
    fn key_value_switch_requires_systemic_fragmentation() {
        let metrics = [TableColumnMetrics {
            max_width: 16,
            header_token_width: 3,
            body_token_width: 16,
            kind: TableColumnKind::Compact,
        }];
        let occasional = vec![
            vec![make_cell("ok")],
            vec![make_cell("verylongvalue")],
            vec![make_cell("done")],
        ];
        let systemic = vec![
            vec![make_cell("firstlongvalue")],
            vec![make_cell("secondlongvalue")],
            vec![make_cell("done")],
        ];

        assert!(!table_key_value::should_render_records(
            &occasional,
            &[4],
            &metrics
        ));
        assert!(!table_key_value::should_render_records(
            &[vec![make_cell("verylongvalue")]],
            &[4],
            &metrics
        ));
        assert!(table_key_value::should_render_records(
            &systemic,
            &[4],
            &metrics
        ));
    }
}
