use ratatui::layout::Alignment;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use std::borrow::Cow;
use std::ops::Range;
use textwrap::Options;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::product::tui_app::render::line_utils::line_to_static;
use crate::product::tui_app::render::line_utils::push_owned_lines;

pub(crate) fn wrap_ranges<'a, O>(text: &str, width_or_options: O) -> Vec<Range<usize>>
where
    O: Into<Options<'a>>,
{
    let opts = width_or_options.into();
    let mut lines: Vec<Range<usize>> = Vec::new();
    for line in textwrap::wrap(text, opts).iter() {
        match line {
            std::borrow::Cow::Borrowed(slice) => {
                let start = unsafe { slice.as_ptr().offset_from(text.as_ptr()) as usize };
                let end = start + slice.len();
                let trailing_spaces = text[end..].chars().take_while(|c| *c == ' ').count();
                lines.push(start..end + trailing_spaces + 1);
            }
            std::borrow::Cow::Owned(_) => panic!("wrap_ranges: unexpected owned string"),
        }
    }
    lines
}

/// Like `wrap_ranges` but returns ranges without trailing whitespace and
/// without the sentinel extra byte. Suitable for general wrapping where
/// trailing spaces should not be preserved.
pub(crate) fn wrap_ranges_trim<'a, O>(text: &str, width_or_options: O) -> Vec<Range<usize>>
where
    O: Into<Options<'a>>,
{
    let opts = width_or_options.into();
    let mut lines: Vec<Range<usize>> = Vec::new();
    for line in textwrap::wrap(text, opts).iter() {
        match line {
            std::borrow::Cow::Borrowed(slice) => {
                let start = unsafe { slice.as_ptr().offset_from(text.as_ptr()) as usize };
                let end = start + slice.len();
                lines.push(start..end);
            }
            std::borrow::Cow::Owned(_) => panic!("wrap_ranges_trim: unexpected owned string"),
        }
    }
    lines
}

#[derive(Debug, Clone)]
pub struct RtOptions<'a> {
    /// The width in columns at which the text will be wrapped.
    pub width: usize,
    /// Line ending used for breaking lines.
    pub line_ending: textwrap::LineEnding,
    /// Indentation used for the first line of output. See the
    /// [`Options::initial_indent`] method.
    pub initial_indent: Line<'a>,
    /// Indentation used for subsequent lines of output. See the
    /// [`Options::subsequent_indent`] method.
    pub subsequent_indent: Line<'a>,
    /// Allow long words to be broken if they cannot fit on a line.
    /// When set to `false`, some lines may be longer than
    /// `self.width`. See the [`Options::break_words`] method.
    pub break_words: bool,
    /// Wrapping algorithm to use, see the implementations of the
    /// [`WrapAlgorithm`] trait for details.
    pub wrap_algorithm: textwrap::WrapAlgorithm,
    /// The line breaking algorithm to use, see the [`WordSeparator`]
    /// trait for an overview and possible implementations.
    pub word_separator: textwrap::WordSeparator,
    /// The method for splitting words. This can be used to prohibit
    /// splitting words on hyphens, or it can be used to implement
    /// language-aware machine hyphenation.
    pub word_splitter: textwrap::WordSplitter,
}
impl From<usize> for RtOptions<'_> {
    fn from(width: usize) -> Self {
        RtOptions::new(width)
    }
}

#[allow(dead_code)]
impl<'a> RtOptions<'a> {
    pub fn new(width: usize) -> Self {
        RtOptions {
            width,
            line_ending: textwrap::LineEnding::LF,
            initial_indent: Line::default(),
            subsequent_indent: Line::default(),
            break_words: true,
            word_separator: textwrap::WordSeparator::new(),
            wrap_algorithm: textwrap::WrapAlgorithm::FirstFit,
            word_splitter: textwrap::WordSplitter::HyphenSplitter,
        }
    }

    pub fn line_ending(self, line_ending: textwrap::LineEnding) -> Self {
        RtOptions {
            line_ending,
            ..self
        }
    }

    pub fn width(self, width: usize) -> Self {
        RtOptions { width, ..self }
    }

    pub fn initial_indent(self, initial_indent: Line<'a>) -> Self {
        RtOptions {
            initial_indent,
            ..self
        }
    }

    pub fn subsequent_indent(self, subsequent_indent: Line<'a>) -> Self {
        RtOptions {
            subsequent_indent,
            ..self
        }
    }

    pub fn break_words(self, break_words: bool) -> Self {
        RtOptions {
            break_words,
            ..self
        }
    }

    pub fn word_separator(self, word_separator: textwrap::WordSeparator) -> RtOptions<'a> {
        RtOptions {
            word_separator,
            ..self
        }
    }

    pub fn wrap_algorithm(self, wrap_algorithm: textwrap::WrapAlgorithm) -> RtOptions<'a> {
        RtOptions {
            wrap_algorithm,
            ..self
        }
    }

    pub fn word_splitter(self, word_splitter: textwrap::WordSplitter) -> RtOptions<'a> {
        RtOptions {
            word_splitter,
            ..self
        }
    }
}

#[must_use]
pub(crate) fn word_wrap_line<'a, O>(line: &'a Line<'a>, width_or_options: O) -> Vec<Line<'a>>
where
    O: Into<RtOptions<'a>>,
{
    // Flatten the line and record span byte ranges.
    let mut flat = String::new();
    let mut span_bounds = Vec::new();
    let mut acc = 0usize;
    for s in &line.spans {
        let text = s.content.as_ref();
        let start = acc;
        flat.push_str(text);
        acc += text.len();
        span_bounds.push((start..acc, s.style));
    }

    let rt_opts: RtOptions<'a> = width_or_options.into();
    let opts = Options::new(rt_opts.width)
        .line_ending(rt_opts.line_ending)
        .break_words(rt_opts.break_words)
        .wrap_algorithm(rt_opts.wrap_algorithm)
        .word_separator(rt_opts.word_separator)
        .word_splitter(rt_opts.word_splitter);

    let mut out: Vec<Line<'a>> = Vec::new();

    // Compute first line range with reduced width due to initial indent.
    let initial_width_available = opts
        .width
        .saturating_sub(rt_opts.initial_indent.width())
        .max(1);
    let initial_wrapped = wrap_ranges_trim(&flat, opts.clone().width(initial_width_available));
    let Some(first_line_range) = initial_wrapped.first() else {
        return vec![rt_opts.initial_indent.clone()];
    };

    // Build first wrapped line with initial indent.
    let mut first_line = rt_opts.initial_indent.clone().style(line.style);
    {
        let sliced = slice_line_spans(line, &span_bounds, first_line_range);
        let mut spans = first_line.spans;
        spans.append(
            &mut sliced
                .spans
                .into_iter()
                .map(|s| s.patch_style(line.style))
                .collect(),
        );
        first_line.spans = spans;
        out.push(first_line);
    }

    // Wrap the remainder using subsequent indent width and map back to original indices.
    let base = first_line_range.end;
    let skip_leading_spaces = flat[base..].chars().take_while(|c| *c == ' ').count();
    let base = base + skip_leading_spaces;
    let subsequent_width_available = opts
        .width
        .saturating_sub(rt_opts.subsequent_indent.width())
        .max(1);
    let remaining_wrapped = wrap_ranges_trim(&flat[base..], opts.width(subsequent_width_available));
    for r in &remaining_wrapped {
        if r.is_empty() {
            continue;
        }
        let mut subsequent_line = rt_opts.subsequent_indent.clone().style(line.style);
        let offset_range = (r.start + base)..(r.end + base);
        let sliced = slice_line_spans(line, &span_bounds, &offset_range);
        let mut spans = subsequent_line.spans;
        spans.append(
            &mut sliced
                .spans
                .into_iter()
                .map(|s| s.patch_style(line.style))
                .collect(),
        );
        subsequent_line.spans = spans;
        out.push(subsequent_line);
    }

    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WrapGrapheme {
    symbol: String,
    style: Style,
    width: usize,
    breakable_whitespace: bool,
    non_breaking_whitespace: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WrapUnit {
    range: Range<usize>,
    contextual_width: usize,
    rendered_width: usize,
}

fn is_non_breaking_space(character: char) -> bool {
    matches!(character, '\u{00A0}' | '\u{2007}' | '\u{202F}')
}

fn is_breakable_whitespace(symbol: &str) -> bool {
    symbol.chars().all(char::is_whitespace) && !symbol.chars().any(is_non_breaking_space)
}

fn collect_wrap_graphemes(line: &Line<'_>) -> Vec<WrapGrapheme> {
    fn push_style_run(output: &mut Vec<WrapGrapheme>, text: &str, style: Style) {
        output.extend(
            text.graphemes(true)
                .filter(|symbol| *symbol != "\n")
                .map(|symbol| WrapGrapheme {
                    symbol: symbol.to_string(),
                    style,
                    width: UnicodeWidthStr::width(symbol),
                    breakable_whitespace: is_breakable_whitespace(symbol),
                    non_breaking_whitespace: symbol.chars().any(is_non_breaking_space),
                }),
        );
    }

    let mut graphemes = Vec::new();
    let mut run = String::new();
    let mut run_style = None;
    for span in &line.spans {
        if span.content.is_empty() {
            continue;
        }
        let style = line.style.patch(span.style);
        if run_style != Some(style) {
            if let Some(style) = run_style {
                push_style_run(&mut graphemes, &run, style);
                run.clear();
            }
            run_style = Some(style);
        }
        run.push_str(span.content.as_ref());
    }
    if let Some(style) = run_style {
        push_style_run(&mut graphemes, &run, style);
    }
    graphemes
}

fn push_grapheme(spans: &mut Vec<Span<'static>>, grapheme: &WrapGrapheme) {
    if let Some(last) = spans.last_mut()
        && last.style == grapheme.style
    {
        last.content.to_mut().push_str(&grapheme.symbol);
    } else {
        spans.push(Span::styled(grapheme.symbol.clone(), grapheme.style));
    }
}

fn push_grapheme_range(spans: &mut Vec<Span<'static>>, graphemes: &[WrapGrapheme]) {
    for grapheme in graphemes {
        push_grapheme(spans, grapheme);
    }
}

fn coalesced_wrap_line(line: &Line<'_>, graphemes: &[WrapGrapheme]) -> Line<'static> {
    let mut spans = Vec::new();
    push_grapheme_range(&mut spans, graphemes);
    let mut output = Line::from(spans);
    output.alignment = line.alignment;
    output
}

pub(crate) fn coalesce_line_graphemes(line: &Line<'_>) -> Line<'static> {
    let graphemes = collect_wrap_graphemes(line);
    if graphemes.is_empty() {
        line_to_static(line)
    } else {
        coalesced_wrap_line(line, &graphemes)
    }
}

fn grapheme_range_width(graphemes: &[WrapGrapheme]) -> usize {
    let mut width = 0usize;
    let mut run = String::new();
    let mut run_style = None;
    for grapheme in graphemes {
        if run_style == Some(grapheme.style) {
            run.push_str(&grapheme.symbol);
            continue;
        }
        width = width.saturating_add(UnicodeWidthStr::width(run.as_str()));
        run.clear();
        run.push_str(&grapheme.symbol);
        run_style = Some(grapheme.style);
    }
    width.saturating_add(UnicodeWidthStr::width(run.as_str()))
}

pub(crate) fn line_width_grapheme_safe(line: &Line<'_>) -> usize {
    collect_wrap_graphemes(line)
        .iter()
        .fold(0usize, |width, grapheme| {
            width.saturating_add(grapheme.width)
        })
}

fn wrap_unit(graphemes: &[WrapGrapheme], range: Range<usize>) -> WrapUnit {
    let contextual_width = if range.end - range.start == 1 {
        graphemes[range.start].width
    } else {
        grapheme_range_width(&graphemes[range.clone()])
    };
    let rendered_width = graphemes[range.clone()]
        .iter()
        .fold(0usize, |width, grapheme| {
            width.saturating_add(grapheme.width)
        });
    WrapUnit {
        range,
        contextual_width,
        rendered_width,
    }
}

fn boundary_requires_same_unit(graphemes: &[WrapGrapheme], right: usize) -> bool {
    let left = &graphemes[right - 1];
    let right_grapheme = &graphemes[right];
    left.non_breaking_whitespace
        || right_grapheme.non_breaking_whitespace
        || grapheme_range_width(&graphemes[right - 1..=right])
            != left.width.saturating_add(right_grapheme.width)
}

fn build_wrap_units_with_word_width(
    graphemes: &[WrapGrapheme],
    word_width: usize,
) -> Vec<WrapUnit> {
    if graphemes.is_empty() {
        return Vec::new();
    }

    let mut units = Vec::new();
    let mut start = 0;
    for right in 1..graphemes.len() {
        if !boundary_requires_same_unit(graphemes, right) {
            units.push(wrap_unit(graphemes, start..right));
            start = right;
        }
    }
    units.push(wrap_unit(graphemes, start..graphemes.len()));

    let unit_width = units.iter().fold(0usize, |width, unit| {
        width.saturating_add(unit.contextual_width)
    });
    if unit_width == word_width {
        units
    } else {
        // Preserve shaping if a future contextual-width rule spans more than one boundary.
        vec![WrapUnit {
            range: 0..graphemes.len(),
            contextual_width: word_width,
            rendered_width: graphemes.iter().fold(0usize, |width, grapheme| {
                width.saturating_add(grapheme.width)
            }),
        }]
    }
}

fn build_wrap_units(graphemes: &[WrapGrapheme]) -> Vec<WrapUnit> {
    build_wrap_units_with_word_width(graphemes, grapheme_range_width(graphemes))
}

fn flush_grapheme_line(
    output: &mut Vec<Line<'static>>,
    spans: &mut Vec<Span<'static>>,
    line_width: &mut usize,
    alignment: Option<Alignment>,
) {
    if !spans.is_empty() {
        let mut line = Line::from(std::mem::take(spans));
        line.alignment = alignment;
        output.push(line);
        *line_width = 0;
    }
}

pub(crate) fn word_wrap_line_grapheme_safe(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let graphemes = collect_wrap_graphemes(line);
    if graphemes.is_empty() {
        return vec![line_to_static(line)];
    }
    let rendered_width = graphemes.iter().fold(0usize, |width, grapheme| {
        width.saturating_add(grapheme.width)
    });
    if rendered_width <= width {
        return vec![coalesced_wrap_line(line, &graphemes)];
    }
    let contextual_width = grapheme_range_width(&graphemes);
    let scalar_graphemes = graphemes
        .iter()
        .all(|grapheme| grapheme.symbol.chars().count() <= 1);
    let has_non_breaking_whitespace = graphemes
        .iter()
        .any(|grapheme| grapheme.non_breaking_whitespace);
    let additive_styled_widths = graphemes.iter().fold(0usize, |width, grapheme| {
        width.saturating_add(grapheme.width)
    }) == contextual_width;
    if scalar_graphemes && !has_non_breaking_whitespace && additive_styled_widths {
        let coalesced = coalesced_wrap_line(line, &graphemes);
        let wrapped = word_wrap_line(&coalesced, width);
        let mut owned = Vec::new();
        push_owned_lines(&wrapped, &mut owned);
        for line in &mut owned {
            line.alignment = coalesced.alignment;
        }
        return owned;
    }

    let mut output = Vec::new();
    let mut spans = Vec::new();
    let mut line_width = 0usize;
    let mut index = 0;
    while index < graphemes.len() {
        let whitespace_start = index;
        while index < graphemes.len() && graphemes[index].breakable_whitespace {
            index += 1;
        }
        let whitespace_end = index;
        let word_start = index;
        while index < graphemes.len() && !graphemes[index].breakable_whitespace {
            index += 1;
        }
        if word_start == index {
            break;
        }

        let word = &graphemes[word_start..index];
        let units = build_wrap_units(word);
        let word_width = units.iter().fold(0usize, |width, unit| {
            width.saturating_add(unit.rendered_width)
        });
        if whitespace_start < whitespace_end && !spans.is_empty() {
            let whitespace_width = graphemes[whitespace_start..whitespace_end]
                .iter()
                .fold(0usize, |width, grapheme| {
                    width.saturating_add(grapheme.width)
                });
            if line_width
                .saturating_add(whitespace_width)
                .saturating_add(word_width)
                <= width
            {
                push_grapheme_range(&mut spans, &graphemes[whitespace_start..whitespace_end]);
                line_width = line_width.saturating_add(whitespace_width);
            } else {
                flush_grapheme_line(&mut output, &mut spans, &mut line_width, line.alignment);
            }
        }

        if !spans.is_empty() && line_width.saturating_add(word_width) > width {
            flush_grapheme_line(&mut output, &mut spans, &mut line_width, line.alignment);
        }

        if word_width <= width {
            push_grapheme_range(&mut spans, word);
            line_width = line_width.saturating_add(word_width);
            continue;
        }

        for unit in units {
            if !spans.is_empty() && line_width.saturating_add(unit.rendered_width) > width {
                flush_grapheme_line(&mut output, &mut spans, &mut line_width, line.alignment);
            }
            push_grapheme_range(&mut spans, &word[unit.range]);
            line_width = line_width.saturating_add(unit.rendered_width);
        }
    }
    flush_grapheme_line(&mut output, &mut spans, &mut line_width, line.alignment);
    if output.is_empty() {
        let mut empty = Line::default().style(line.style);
        empty.alignment = line.alignment;
        output.push(empty);
    }
    output
}

/// Utilities to allow wrapping either borrowed or owned lines.
#[derive(Debug)]
enum LineInput<'a> {
    Borrowed(&'a Line<'a>),
    Owned(Line<'a>),
}

impl<'a> LineInput<'a> {
    fn as_ref(&self) -> &Line<'a> {
        match self {
            LineInput::Borrowed(line) => line,
            LineInput::Owned(line) => line,
        }
    }
}

/// This trait makes it easier to pass whatever we need into word_wrap_lines.
trait IntoLineInput<'a> {
    fn into_line_input(self) -> LineInput<'a>;
}

impl<'a> IntoLineInput<'a> for &'a Line<'a> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Borrowed(self)
    }
}

impl<'a> IntoLineInput<'a> for &'a mut Line<'a> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Borrowed(self)
    }
}

impl<'a> IntoLineInput<'a> for Line<'a> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(self)
    }
}

impl<'a> IntoLineInput<'a> for String {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

impl<'a> IntoLineInput<'a> for &'a str {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

impl<'a> IntoLineInput<'a> for Cow<'a, str> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

impl<'a> IntoLineInput<'a> for Span<'a> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

impl<'a> IntoLineInput<'a> for Vec<Span<'a>> {
    fn into_line_input(self) -> LineInput<'a> {
        LineInput::Owned(Line::from(self))
    }
}

/// Wrap a sequence of lines, applying the initial indent only to the very first
/// output line, and using the subsequent indent for all later wrapped pieces.
#[allow(private_bounds)] // IntoLineInput isn't public, but it doesn't really need to be.
pub(crate) fn word_wrap_lines<'a, I, O, L>(lines: I, width_or_options: O) -> Vec<Line<'static>>
where
    I: IntoIterator<Item = L>,
    L: IntoLineInput<'a>,
    O: Into<RtOptions<'a>>,
{
    let base_opts: RtOptions<'a> = width_or_options.into();
    let mut out: Vec<Line<'static>> = Vec::new();

    for (idx, line) in lines.into_iter().enumerate() {
        let line_input = line.into_line_input();
        if line_is_blank_spaces_only(line_input.as_ref()) {
            out.push(Line::default().style(line_input.as_ref().style));
            continue;
        }
        let opts = if idx == 0 {
            base_opts.clone()
        } else {
            let mut o = base_opts.clone();
            let sub = o.subsequent_indent.clone();
            o = o.initial_indent(sub);
            o
        };
        let wrapped = word_wrap_line(line_input.as_ref(), opts);
        push_owned_lines(&wrapped, &mut out);
    }

    out
}

#[allow(dead_code)]
pub(crate) fn word_wrap_lines_borrowed<'a, I, O>(lines: I, width_or_options: O) -> Vec<Line<'a>>
where
    I: IntoIterator<Item = &'a Line<'a>>,
    O: Into<RtOptions<'a>>,
{
    let base_opts: RtOptions<'a> = width_or_options.into();
    let mut out: Vec<Line<'a>> = Vec::new();
    let mut first = true;
    for line in lines.into_iter() {
        if line_is_blank_spaces_only(line) {
            out.push(Line::default().style(line.style));
            first = false;
            continue;
        }
        let opts = if first {
            base_opts.clone()
        } else {
            base_opts
                .clone()
                .initial_indent(base_opts.subsequent_indent.clone())
        };
        out.extend(word_wrap_line(line, opts));
        first = false;
    }
    out
}

fn line_is_blank_spaces_only(line: &Line<'_>) -> bool {
    line.spans.is_empty()
        || line
            .spans
            .iter()
            .all(|span| span.content.chars().all(|c| c == ' '))
}

fn slice_line_spans<'a>(
    original: &'a Line<'a>,
    span_bounds: &[(Range<usize>, ratatui::style::Style)],
    range: &Range<usize>,
) -> Line<'a> {
    let start_byte = range.start;
    let end_byte = range.end;
    let mut acc: Vec<Span<'a>> = Vec::new();
    for (i, (range, style)) in span_bounds.iter().enumerate() {
        let s = range.start;
        let e = range.end;
        if e <= start_byte {
            continue;
        }
        if s >= end_byte {
            break;
        }
        let seg_start = start_byte.max(s);
        let seg_end = end_byte.min(e);
        if seg_end > seg_start {
            let local_start = seg_start - s;
            let local_end = seg_end - s;
            let content = original.spans[i].content.as_ref();
            let slice = &content[local_start..local_end];
            acc.push(Span {
                style: *style,
                content: std::borrow::Cow::Borrowed(slice),
            });
        }
        if e >= end_byte {
            break;
        }
    }
    Line {
        style: original.style,
        alignment: original.alignment,
        spans: acc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use itertools::Itertools as _;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;
    use ratatui::style::Stylize;
    use std::string::ToString;

    fn concat_line(line: &Line) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn trivial_unstyled_no_indents_wide_width() {
        let line = Line::from("hello");
        let out = word_wrap_line(&line, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(concat_line(&out[0]), "hello");
    }

    #[test]
    fn simple_unstyled_wrap_narrow_width() {
        let line = Line::from("hello world");
        let out = word_wrap_line(&line, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "hello");
        assert_eq!(concat_line(&out[1]), "world");
    }

    #[test]
    fn simple_styled_wrap_preserves_styles() {
        let line = Line::from(vec!["hello ".red(), "world".into()]);
        let out = word_wrap_line(&line, 6);
        assert_eq!(out.len(), 2);
        // First line should carry the red style
        assert_eq!(concat_line(&out[0]), "hello");
        assert_eq!(out[0].spans.len(), 1);
        assert_eq!(out[0].spans[0].style.fg, Some(Color::Red));
        // Second line is unstyled
        assert_eq!(concat_line(&out[1]), "world");
        assert_eq!(out[1].spans.len(), 1);
        assert_eq!(out[1].spans[0].style.fg, None);
    }

    #[test]
    fn with_initial_and_subsequent_indents() {
        let opts = RtOptions::new(8)
            .initial_indent(Line::from("- "))
            .subsequent_indent(Line::from("  "));
        let line = Line::from("hello world foo");
        let out = word_wrap_line(&line, opts);
        // Expect three lines with proper prefixes
        assert!(concat_line(&out[0]).starts_with("- "));
        assert!(concat_line(&out[1]).starts_with("  "));
        assert!(concat_line(&out[2]).starts_with("  "));
        // And content roughly segmented
        assert_eq!(concat_line(&out[0]), "- hello");
        assert_eq!(concat_line(&out[1]), "  world");
        assert_eq!(concat_line(&out[2]), "  foo");
    }

    #[test]
    fn empty_initial_indent_subsequent_spaces() {
        let opts = RtOptions::new(8)
            .initial_indent(Line::from(""))
            .subsequent_indent(Line::from("    "));
        let line = Line::from("hello world foobar");
        let out = word_wrap_line(&line, opts);
        assert!(concat_line(&out[0]).starts_with("hello"));
        for l in &out[1..] {
            assert!(concat_line(l).starts_with("    "));
        }
    }

    #[test]
    fn empty_input_yields_single_empty_line() {
        let line = Line::from("");
        let out = word_wrap_line(&line, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(concat_line(&out[0]), "");
    }

    #[test]
    fn leading_spaces_preserved_on_first_line() {
        let line = Line::from("   hello");
        let out = word_wrap_line(&line, 8);
        assert_eq!(out.len(), 1);
        assert_eq!(concat_line(&out[0]), "   hello");
    }

    #[test]
    fn multiple_spaces_between_words_dont_start_next_line_with_spaces() {
        let line = Line::from("hello   world");
        let out = word_wrap_line(&line, 8);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "hello");
        assert_eq!(concat_line(&out[1]), "world");
    }

    #[test]
    fn break_words_false_allows_overflow_for_long_word() {
        let opts = RtOptions::new(5).break_words(false);
        let line = Line::from("supercalifragilistic");
        let out = word_wrap_line(&line, opts);
        assert_eq!(out.len(), 1);
        assert_eq!(concat_line(&out[0]), "supercalifragilistic");
    }

    #[test]
    fn hyphen_splitter_breaks_at_hyphen() {
        let line = Line::from("hello-world");
        let out = word_wrap_line(&line, 7);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "hello-");
        assert_eq!(concat_line(&out[1]), "world");
    }

    #[test]
    fn indent_consumes_width_leaving_one_char_space() {
        let opts = RtOptions::new(4)
            .initial_indent(Line::from(">>>>"))
            .subsequent_indent(Line::from("--"));
        let line = Line::from("hello");
        let out = word_wrap_line(&line, opts);
        assert_eq!(out.len(), 3);
        assert_eq!(concat_line(&out[0]), ">>>>h");
        assert_eq!(concat_line(&out[1]), "--el");
        assert_eq!(concat_line(&out[2]), "--lo");
    }

    #[test]
    fn wide_unicode_wraps_by_display_width() {
        let line = Line::from("😀😀😀");
        let out = word_wrap_line(&line, 4);
        assert_eq!(out.len(), 2);
        assert_eq!(concat_line(&out[0]), "😀😀");
        assert_eq!(concat_line(&out[1]), "😀");
    }

    #[test]
    fn word_wrap_line_preserves_cjk_ascii_span_order() {
        let line = Line::from(vec![
            "后要被认为是 ".into(),
            "crates.io".cyan(),
            " 发布就绪".into(),
            "Git 依赖；我说的是".green(),
        ]);
        let expected = "后要被认为是crates.io发布就绪Git依赖；我说的是";

        for width in 8..=40 {
            let out = word_wrap_line(&line, width);
            let rendered = out
                .iter()
                .map(concat_line)
                .collect::<String>()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>();

            assert_eq!(
                rendered, expected,
                "width {width} should preserve original CJK/ASCII order"
            );
            assert!(
                !rendered.contains("依；赖"),
                "width {width} rendered the known CJK punctuation corruption: {out:?}"
            );
            assert!(
                !rendered.contains("crates是.io"),
                "width {width} rendered the known crates.io corruption: {out:?}"
            );
        }
    }

    #[test]
    fn styled_split_within_span_preserves_style() {
        use ratatui::style::Stylize;
        let line = Line::from(vec!["abcd".red()]);
        let out = word_wrap_line(&line, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].spans.len(), 1);
        assert_eq!(out[1].spans.len(), 1);
        assert_eq!(out[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(out[1].spans[0].style.fg, Some(Color::Red));
        assert_eq!(concat_line(&out[0]), "ab");
        assert_eq!(concat_line(&out[1]), "cd");
    }

    #[test]
    fn wrap_lines_applies_initial_indent_only_once() {
        let opts = RtOptions::new(8)
            .initial_indent(Line::from("- "))
            .subsequent_indent(Line::from("  "));

        let lines = vec![Line::from("hello world"), Line::from("foo bar baz")];
        let out = word_wrap_lines(lines, opts);

        // Expect: first line prefixed with "- ", subsequent wrapped pieces with "  "
        // and for the second input line, there should be no "- " prefix on its first piece
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert!(rendered[0].starts_with("- "));
        for r in rendered.iter().skip(1) {
            assert!(r.starts_with("  "));
        }
    }

    #[test]
    fn wrap_lines_keeps_blank_lines_unindented() {
        let opts = RtOptions::new(80)
            .initial_indent(Line::from("• "))
            .subsequent_indent(Line::from("  "));

        let lines = vec![
            Line::from("# Title"),
            Line::default(),
            Line::from("## Summary"),
        ];
        let out = word_wrap_lines(lines, opts);

        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["• # Title", "", "  ## Summary"]);
    }

    #[test]
    fn wrap_lines_without_indents_is_concat_of_single_wraps() {
        let lines = vec![Line::from("hello"), Line::from("world!")];
        let out = word_wrap_lines(lines, 10);
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["hello", "world!"]);
    }

    #[test]
    fn wrap_lines_borrowed_applies_initial_indent_only_once() {
        let opts = RtOptions::new(8)
            .initial_indent(Line::from("- "))
            .subsequent_indent(Line::from("  "));

        let lines = [Line::from("hello world"), Line::from("foo bar baz")];
        let out = word_wrap_lines_borrowed(lines.iter(), opts);

        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert!(rendered.first().unwrap().starts_with("- "));
        for r in rendered.iter().skip(1) {
            assert!(r.starts_with("  "));
        }
    }

    #[test]
    fn wrap_lines_borrowed_without_indents_is_concat_of_single_wraps() {
        let lines = [Line::from("hello"), Line::from("world!")];
        let out = word_wrap_lines_borrowed(lines.iter(), 10);
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["hello", "world!"]);
    }

    #[test]
    fn wrap_lines_accepts_borrowed_iterators() {
        let lines = [Line::from("hello world"), Line::from("foo bar baz")];
        let out = word_wrap_lines(lines, 10);
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["hello", "world", "foo bar", "baz"]);
    }

    #[test]
    fn wrap_lines_accepts_str_slices() {
        let lines = ["hello world", "goodnight moon"];
        let out = word_wrap_lines(lines, 12);
        let rendered: Vec<String> = out.iter().map(concat_line).collect();
        assert_eq!(rendered, vec!["hello world", "goodnight", "moon"]);
    }

    #[test]
    fn line_height_counts_double_width_emoji() {
        let line = "😀😀😀".into(); // each emoji ~ width 2
        assert_eq!(word_wrap_line(&line, 4).len(), 2);
        assert_eq!(word_wrap_line(&line, 2).len(), 3);
        assert_eq!(word_wrap_line(&line, 6).len(), 1);
    }

    #[test]
    fn grapheme_safe_wrap_keeps_zwj_emoji_and_styles_intact() {
        let line = Line::from(vec!["👨‍👩‍👧‍👦".red(), " team".cyan()]);
        let wrapped = word_wrap_line_grapheme_safe(&line, 4);

        assert_eq!(
            wrapped.iter().map(concat_line).collect::<Vec<_>>(),
            vec!["👨‍👩‍👧‍👦", "team"]
        );
        assert_eq!(wrapped[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(wrapped[1].spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn grapheme_safe_wrap_handles_long_zwj_runs_without_splitting_or_losing_style() {
        let family = "👨‍👩‍👧‍👦";
        let input = family.repeat(512);
        let line = Line::from(input.clone().red());
        let wrapped = word_wrap_line_grapheme_safe(&line, 2);

        assert_eq!(wrapped, vec![Line::from(family.to_string().red()); 512]);
        assert_eq!(wrapped.iter().map(concat_line).collect::<String>(), input);
    }

    #[test]
    fn grapheme_safe_wrap_preserves_non_breaking_spaces() {
        for space in ['\u{00A0}', '\u{2007}', '\u{202F}'] {
            let suffix = format!("{space}team");
            let line = Line::from(vec!["👨‍👩‍👧‍👦".red(), suffix.cyan()]);
            let first_suffix = format!("{space}t");
            let expected = vec![
                Line::from(vec!["👨‍👩‍👧‍👦".red(), first_suffix.cyan()]),
                Line::from("eam".cyan()),
            ];

            assert_eq!(word_wrap_line_grapheme_safe(&line, 4), expected);
        }
    }

    #[test]
    fn grapheme_safe_wrap_does_not_break_around_scalar_non_breaking_spaces() {
        for space in ['\u{00A0}', '\u{2007}', '\u{202F}'] {
            let line = Line::from(format!("a{space}b").cyan());

            assert_eq!(
                word_wrap_line_grapheme_safe(&line, 1),
                vec![line_to_static(&line)]
            );
        }
    }

    #[test]
    fn grapheme_safe_wrap_keeps_overwide_zwj_emoji_intact() {
        let line = Line::from("👨‍👩‍👧‍👦".red());

        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 1),
            vec![line_to_static(&line)]
        );
    }

    #[test]
    fn grapheme_safe_wrap_preserves_fitting_line_with_leading_space() {
        let line = Line::from(" 👨‍👩‍👧‍👦".cyan()).style(Style::new().bold());

        assert_eq!(
            word_wrap_line_grapheme_safe(&line, line.width()),
            vec![Line::from(" 👨‍👩‍👧‍👦".cyan().bold())]
        );
    }

    #[test]
    fn grapheme_safe_wrap_keeps_lam_alef_on_one_line() {
        let line = Line::from("لا");
        assert_eq!(line.width(), 1);
        assert_eq!(line_width_grapheme_safe(&line), 2);
        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 1),
            vec![line_to_static(&line)]
        );
    }

    #[test]
    fn grapheme_safe_wrap_keeps_repeated_lam_alef_pairs_together() {
        let line = Line::from("لالاx".red());

        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 1),
            vec![
                Line::from("لا".red()),
                Line::from("لا".red()),
                Line::from("x".red())
            ]
        );
    }

    #[test]
    fn grapheme_safe_wrap_budgets_lam_alef_pairs_by_rendered_width() {
        let line = Line::from("لالالا".red());

        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 3),
            vec![
                Line::from("لا".red()),
                Line::from("لا".red()),
                Line::from("لا".red())
            ]
        );
    }

    #[test]
    fn grapheme_safe_wrap_uses_contextual_span_widths() {
        let line = Line::from(vec!["لا".red(), "x".cyan()]);

        assert_eq!(line.width(), 2);
        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 1),
            vec![Line::from("لا".red()), Line::from("x".cyan())]
        );
    }

    #[test]
    fn grapheme_safe_wrap_merges_same_style_spans_for_contextual_width() {
        let line = Line::from(vec!["ل".red(), "ا".red(), "x".red()]);

        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 1),
            vec![Line::from("لا".red()), Line::from("x".red())]
        );
    }

    #[test]
    fn grapheme_safe_wrap_merges_same_style_spans_before_grapheme_segmentation() {
        let line = Line::from(vec!["क".red(), "\u{094D}".red(), "ष".red(), "x".red()]);

        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 1),
            vec![Line::from("क्ष".red()), Line::from("x".red())]
        );
    }

    #[test]
    fn grapheme_safe_wrap_uses_coalesced_width_for_fitting_lines() {
        let line = Line::from(vec!["#".red(), "\u{FE0F}".red()]);
        let wrapped = word_wrap_line_grapheme_safe(&line, 1);

        assert_eq!(line.width(), 1);
        assert_eq!(line_width_grapheme_safe(&line), 2);
        assert_eq!(wrapped, vec![Line::from("#\u{FE0F}".red())]);
        assert_eq!(wrapped[0].width(), 2);
    }

    #[test]
    fn grapheme_safe_wrap_fast_path_preserves_span_style_over_line_style() {
        let line = Line::from("abc".blue()).style(Style::new().red());

        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 1),
            vec![
                Line::from("a".blue()),
                Line::from("b".blue()),
                Line::from("c".blue())
            ]
        );
    }

    #[test]
    fn grapheme_safe_wrap_preserves_alignment_on_all_paths() {
        let scalar = Line::from("abc").centered();
        assert_eq!(
            word_wrap_line_grapheme_safe(&scalar, 1),
            vec![
                Line::from("a").centered(),
                Line::from("b").centered(),
                Line::from("c").centered()
            ]
        );

        let family = "👨‍👩‍👧‍👦";
        let custom = Line::from(family.repeat(2)).right_aligned();
        assert_eq!(
            word_wrap_line_grapheme_safe(&custom, 2),
            vec![
                Line::from(family.to_string()).right_aligned(),
                Line::from(family.to_string()).right_aligned()
            ]
        );
    }

    #[test]
    fn grapheme_safe_wrap_keeps_separately_styled_arabic_scalars_separate() {
        let line = Line::from(vec!["ل".red(), "ا".cyan()]);

        assert_eq!(line.width(), 2);
        assert_eq!(
            word_wrap_line_grapheme_safe(&line, 1),
            vec![Line::from("ل".red()), Line::from("ا".cyan())]
        );
    }

    #[test]
    fn grapheme_safe_wrap_units_split_repeated_zwj_emoji_linearly() {
        let family = "👨‍👩‍👧‍👦";
        let line = Line::from(family.repeat(3).red());
        let graphemes = collect_wrap_graphemes(&line);

        assert_eq!(
            build_wrap_units(&graphemes),
            vec![
                WrapUnit {
                    range: 0..1,
                    contextual_width: 2,
                    rendered_width: 2,
                },
                WrapUnit {
                    range: 1..2,
                    contextual_width: 2,
                    rendered_width: 2,
                },
                WrapUnit {
                    range: 2..3,
                    contextual_width: 2,
                    rendered_width: 2,
                },
            ]
        );
    }

    #[test]
    fn grapheme_safe_wrap_units_keep_lam_alef_pairs_together() {
        let line = Line::from("لالا");
        let graphemes = collect_wrap_graphemes(&line);

        assert_eq!(
            build_wrap_units(&graphemes),
            vec![
                WrapUnit {
                    range: 0..2,
                    contextual_width: 1,
                    rendered_width: 2,
                },
                WrapUnit {
                    range: 2..4,
                    contextual_width: 1,
                    rendered_width: 2,
                },
            ]
        );
    }

    #[test]
    fn grapheme_safe_wrap_units_keep_non_breaking_spaces_with_neighbors() {
        for space in ['\u{00A0}', '\u{2007}', '\u{202F}'] {
            let line = Line::from(format!("👨‍👩‍👧‍👦{space}t"));
            let graphemes = collect_wrap_graphemes(&line);

            assert_eq!(
                build_wrap_units(&graphemes),
                vec![WrapUnit {
                    range: 0..3,
                    contextual_width: 4,
                    rendered_width: 4,
                }]
            );
        }
    }

    #[test]
    fn grapheme_safe_wrap_units_respect_style_boundaries() {
        let line = Line::from(vec!["ل".red(), "ا".cyan()]);
        let graphemes = collect_wrap_graphemes(&line);

        assert_eq!(
            build_wrap_units(&graphemes),
            vec![
                WrapUnit {
                    range: 0..1,
                    contextual_width: 1,
                    rendered_width: 1,
                },
                WrapUnit {
                    range: 1..2,
                    contextual_width: 1,
                    rendered_width: 1,
                },
            ]
        );
    }

    #[test]
    fn grapheme_safe_wrap_units_fall_back_for_longer_contextual_widths() {
        let line = Line::from("abc");
        let graphemes = collect_wrap_graphemes(&line);

        assert_eq!(
            build_wrap_units_with_word_width(&graphemes, 2),
            vec![WrapUnit {
                range: 0..3,
                contextual_width: 2,
                rendered_width: 3,
            }]
        );
    }

    #[test]
    fn word_wrap_does_not_split_words_simple_english() {
        let sample = "Years passed, and Willowmere thrived in peace and friendship. Mira’s herb garden flourished with both ordinary and enchanted plants, and travelers spoke of the kindness of the woman who tended them.";
        let line = Line::from(sample);
        let lines = [line];
        // Force small width to exercise wrapping at spaces.
        let wrapped = word_wrap_lines_borrowed(&lines, 40);
        let joined: String = wrapped.iter().map(ToString::to_string).join("\n");
        assert_eq!(
            joined,
            r#"Years passed, and Willowmere thrived in
peace and friendship. Mira’s herb garden
flourished with both ordinary and
enchanted plants, and travelers spoke of
the kindness of the woman who tended
them."#
        );
    }
}
