use diffy::Hunk;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line as RtLine;
use ratatui::text::Span as RtSpan;
use ratatui::widgets::Paragraph;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use crate::color::is_light;
use crate::exec_command::relativize_to_home;
use crate::render::Insets;
use crate::render::line_utils::pad_line_to_width;
use crate::render::line_utils::prefix_lines;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::InsetRenderable;
use crate::render::renderable::Renderable;
use crate::terminal_palette::default_bg;
use adam_agent::git_info::get_git_repo_root;
use adam_agent::protocol::FileChange;
use adam_agent::terminal::TerminalName;
use adam_agent::terminal::terminal_info;

// Diff background palette. Dark-theme tints are intentionally muted so they
// don't overpower syntax colors or the terminal's default foreground.
const DARK_TC_ADD_LINE_BG_RGB: (u8, u8, u8) = (33, 58, 43); // #213A2B
const DARK_TC_DEL_LINE_BG_RGB: (u8, u8, u8) = (74, 34, 29); // #4A221D
const LIGHT_TC_ADD_LINE_BG_RGB: (u8, u8, u8) = (218, 251, 225); // #dafbe1
const LIGHT_TC_DEL_LINE_BG_RGB: (u8, u8, u8) = (255, 235, 233); // #ffebe9
const LIGHT_TC_GUTTER_FG_RGB: (u8, u8, u8) = (31, 35, 40); // #1f2328

const DARK_256_ADD_LINE_BG_IDX: u8 = 22;
const DARK_256_DEL_LINE_BG_IDX: u8 = 52;
const LIGHT_256_ADD_LINE_BG_IDX: u8 = 194;
const LIGHT_256_DEL_LINE_BG_IDX: u8 = 224;
const LIGHT_256_GUTTER_FG_IDX: u8 = 236;

// Internal representation for diff line rendering
#[derive(Clone, Copy)]
enum DiffLineType {
    Insert,
    Delete,
    Context,
}

#[derive(Clone, Copy, Debug)]
enum DiffTheme {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiffColorLevel {
    TrueColor,
    Ansi256,
    Ansi16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RichDiffColorLevel {
    TrueColor,
    Ansi256,
}

impl RichDiffColorLevel {
    fn from_diff_color_level(level: DiffColorLevel) -> Option<Self> {
        match level {
            DiffColorLevel::TrueColor => Some(Self::TrueColor),
            DiffColorLevel::Ansi256 => Some(Self::Ansi256),
            DiffColorLevel::Ansi16 => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ResolvedDiffBackgrounds {
    add: Option<Color>,
    del: Option<Color>,
}

#[derive(Clone, Copy, Debug)]
struct DiffRenderStyleContext {
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
}

fn current_diff_render_style_context() -> DiffRenderStyleContext {
    let theme = diff_theme();
    let color_level = diff_color_level();
    let diff_backgrounds = fallback_diff_backgrounds(theme, color_level);
    DiffRenderStyleContext {
        theme,
        color_level,
        diff_backgrounds,
    }
}

pub struct DiffSummary {
    changes: HashMap<PathBuf, FileChange>,
    cwd: PathBuf,
}

impl DiffSummary {
    pub fn new(changes: HashMap<PathBuf, FileChange>, cwd: PathBuf) -> Self {
        Self { changes, cwd }
    }
}

impl Renderable for FileChange {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut lines = vec![];
        render_change(self, &mut lines, area.width as usize);
        Paragraph::new(lines).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let mut lines = vec![];
        render_change(self, &mut lines, width as usize);
        lines.len() as u16
    }
}

impl From<DiffSummary> for Box<dyn Renderable> {
    fn from(val: DiffSummary) -> Self {
        let mut rows: Vec<Box<dyn Renderable>> = vec![];

        for (i, row) in collect_rows(&val.changes).into_iter().enumerate() {
            if i > 0 {
                rows.push(Box::new(RtLine::from("")));
            }
            let mut path = RtLine::from(display_path_for(&row.path, &val.cwd));
            path.push_span(" ");
            path.extend(render_line_count_summary(row.added, row.removed));
            rows.push(Box::new(path));
            rows.push(Box::new(RtLine::from("")));
            rows.push(Box::new(InsetRenderable::new(
                Box::new(row.change) as Box<dyn Renderable>,
                Insets::tlbr(0, 2, 0, 0),
            )));
        }

        Box::new(ColumnRenderable::with(rows))
    }
}

pub(crate) fn create_diff_summary(
    changes: &HashMap<PathBuf, FileChange>,
    cwd: &Path,
    wrap_cols: usize,
) -> Vec<RtLine<'static>> {
    let rows = collect_rows(changes);
    render_changes_block(rows, wrap_cols, cwd)
}

// Shared row for per-file presentation
#[derive(Clone)]
struct Row {
    #[allow(dead_code)]
    path: PathBuf,
    move_path: Option<PathBuf>,
    added: usize,
    removed: usize,
    change: FileChange,
}

fn collect_rows(changes: &HashMap<PathBuf, FileChange>) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (path, change) in changes.iter() {
        let (added, removed) = match change {
            FileChange::Add { content } => (content.lines().count(), 0),
            FileChange::Delete { content } => (0, content.lines().count()),
            FileChange::Update { unified_diff, .. } => calculate_add_remove_from_diff(unified_diff),
        };
        let move_path = match change {
            FileChange::Update {
                move_path: Some(new),
                ..
            } => Some(new.clone()),
            _ => None,
        };
        rows.push(Row {
            path: path.clone(),
            move_path,
            added,
            removed,
            change: change.clone(),
        });
    }
    rows.sort_by_key(|r| r.path.clone());
    rows
}

pub(crate) fn render_line_count_summary(added: usize, removed: usize) -> Vec<RtSpan<'static>> {
    let mut spans = Vec::new();
    spans.push("(".into());
    spans.push(format!("+{added}").green());
    spans.push(" ".into());
    spans.push(format!("-{removed}").red());
    spans.push(")".into());
    spans
}

fn render_changes_block(rows: Vec<Row>, wrap_cols: usize, cwd: &Path) -> Vec<RtLine<'static>> {
    let mut out: Vec<RtLine<'static>> = Vec::new();

    let render_path = |row: &Row| -> Vec<RtSpan<'static>> {
        let mut spans = Vec::new();
        spans.push(display_path_for(&row.path, cwd).into());
        if let Some(move_path) = &row.move_path {
            spans.push(format!(" → {}", display_path_for(move_path, cwd)).into());
        }
        spans
    };

    // Header
    let total_added: usize = rows.iter().map(|r| r.added).sum();
    let total_removed: usize = rows.iter().map(|r| r.removed).sum();
    let file_count = rows.len();
    let noun = if file_count == 1 { "file" } else { "files" };
    let mut header_spans: Vec<RtSpan<'static>> = vec!["• ".dim()];
    if let [row] = &rows[..] {
        let verb = match &row.change {
            FileChange::Add { .. } => "Added",
            FileChange::Delete { .. } => "Deleted",
            _ => "Edited",
        };
        header_spans.push(verb.bold());
        header_spans.push(" ".into());
        header_spans.extend(render_path(row));
        header_spans.push(" ".into());
        header_spans.extend(render_line_count_summary(row.added, row.removed));
    } else {
        header_spans.push("Edited".bold());
        header_spans.push(format!(" {file_count} {noun} ").into());
        header_spans.extend(render_line_count_summary(total_added, total_removed));
    }
    out.push(RtLine::from(header_spans));

    for (idx, r) in rows.into_iter().enumerate() {
        // Insert a blank separator between file chunks (except before the first)
        if idx > 0 {
            out.push("".into());
        }
        // File header line (skip when single-file header already shows the name)
        let skip_file_header = file_count == 1;
        if !skip_file_header {
            let mut header: Vec<RtSpan<'static>> = Vec::new();
            header.push("  └ ".dim());
            header.extend(render_path(&r));
            header.push(" ".into());
            header.extend(render_line_count_summary(r.added, r.removed));
            out.push(RtLine::from(header));
        }

        let mut lines = vec![];
        render_change(&r.change, &mut lines, wrap_cols - 4);
        out.extend(prefix_lines(lines, "    ".into(), "    ".into()));
    }

    out
}

fn render_change(change: &FileChange, out: &mut Vec<RtLine<'static>>, width: usize) {
    let style_context = current_diff_render_style_context();
    match change {
        FileChange::Add { content } => {
            let line_number_width = line_number_width(content.lines().count());
            for (i, raw) in content.lines().enumerate() {
                out.extend(push_wrapped_diff_line_with_style_context(
                    i + 1,
                    DiffLineType::Insert,
                    raw,
                    width,
                    line_number_width,
                    style_context,
                ));
            }
        }
        FileChange::Delete { content } => {
            let line_number_width = line_number_width(content.lines().count());
            for (i, raw) in content.lines().enumerate() {
                out.extend(push_wrapped_diff_line_with_style_context(
                    i + 1,
                    DiffLineType::Delete,
                    raw,
                    width,
                    line_number_width,
                    style_context,
                ));
            }
        }
        FileChange::Update { unified_diff, .. } => {
            if let Ok(patch) = diffy::Patch::from_str(unified_diff) {
                let mut max_line_number = 0;
                for h in patch.hunks() {
                    let mut old_ln = h.old_range().start();
                    let mut new_ln = h.new_range().start();
                    for l in h.lines() {
                        match l {
                            diffy::Line::Insert(_) => {
                                max_line_number = max_line_number.max(new_ln);
                                new_ln += 1;
                            }
                            diffy::Line::Delete(_) => {
                                max_line_number = max_line_number.max(old_ln);
                                old_ln += 1;
                            }
                            diffy::Line::Context(_) => {
                                max_line_number = max_line_number.max(new_ln);
                                old_ln += 1;
                                new_ln += 1;
                            }
                        }
                    }
                }
                let line_number_width = line_number_width(max_line_number);
                let mut is_first_hunk = true;
                for h in patch.hunks() {
                    if !is_first_hunk {
                        let spacer = format!("{:width$} ", "", width = line_number_width.max(1));
                        let spacer_span = RtSpan::styled(
                            spacer,
                            style_gutter_for(
                                DiffLineType::Context,
                                style_context.theme,
                                style_context.color_level,
                            ),
                        );
                        out.push(RtLine::from(vec![spacer_span, "⋮".dim()]));
                    }
                    is_first_hunk = false;

                    let mut old_ln = h.old_range().start();
                    let mut new_ln = h.new_range().start();
                    for l in h.lines() {
                        match l {
                            diffy::Line::Insert(text) => {
                                let s = text.trim_end_matches('\n');
                                out.extend(push_wrapped_diff_line_with_style_context(
                                    new_ln,
                                    DiffLineType::Insert,
                                    s,
                                    width,
                                    line_number_width,
                                    style_context,
                                ));
                                new_ln += 1;
                            }
                            diffy::Line::Delete(text) => {
                                let s = text.trim_end_matches('\n');
                                out.extend(push_wrapped_diff_line_with_style_context(
                                    old_ln,
                                    DiffLineType::Delete,
                                    s,
                                    width,
                                    line_number_width,
                                    style_context,
                                ));
                                old_ln += 1;
                            }
                            diffy::Line::Context(text) => {
                                let s = text.trim_end_matches('\n');
                                out.extend(push_wrapped_diff_line_with_style_context(
                                    new_ln,
                                    DiffLineType::Context,
                                    s,
                                    width,
                                    line_number_width,
                                    style_context,
                                ));
                                old_ln += 1;
                                new_ln += 1;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Format a path for display relative to the current working directory when
/// possible, keeping output stable in jj/no-`.git` workspaces (e.g. image
/// tool calls should show `example.png` instead of an absolute path).
pub(crate) fn display_path_for(path: &Path, cwd: &Path) -> String {
    if path.is_relative() {
        return path.display().to_string();
    }

    if let Ok(stripped) = path.strip_prefix(cwd) {
        return stripped.display().to_string();
    }

    let path_in_same_repo = match (get_git_repo_root(cwd), get_git_repo_root(path)) {
        (Some(cwd_repo), Some(path_repo)) => cwd_repo == path_repo,
        _ => false,
    };
    let chosen = if path_in_same_repo {
        pathdiff::diff_paths(path, cwd).unwrap_or_else(|| path.to_path_buf())
    } else {
        relativize_to_home(path)
            .map(|p| PathBuf::from_iter([Path::new("~"), p.as_path()]))
            .unwrap_or_else(|| path.to_path_buf())
    };
    chosen.display().to_string()
}

fn calculate_add_remove_from_diff(diff: &str) -> (usize, usize) {
    if let Ok(patch) = diffy::Patch::from_str(diff) {
        patch
            .hunks()
            .iter()
            .flat_map(Hunk::lines)
            .fold((0, 0), |(a, d), l| match l {
                diffy::Line::Insert(_) => (a + 1, d),
                diffy::Line::Delete(_) => (a, d + 1),
                diffy::Line::Context(_) => (a, d),
            })
    } else {
        // For unparsable diffs, return 0 for both counts.
        (0, 0)
    }
}

fn push_wrapped_diff_line_with_style_context(
    line_number: usize,
    kind: DiffLineType,
    text: &str,
    width: usize,
    line_number_width: usize,
    style_context: DiffRenderStyleContext,
) -> Vec<RtLine<'static>> {
    let ln_str = line_number.to_string();
    let mut remaining_text: &str = text;

    // Reserve a fixed number of spaces (equal to the widest line number plus a
    // trailing spacer) so the sign column stays aligned across the diff block.
    let gutter_width = line_number_width.max(1);
    let prefix_cols = gutter_width + 1;

    let mut first = true;
    let gutter_style = style_gutter_for(kind, style_context.theme, style_context.color_level);
    let line_bg = style_line_bg_for(kind, style_context.diff_backgrounds);
    let (sign_char, sign_style, content_style) = match kind {
        DiffLineType::Insert => (
            '+',
            style_sign_add(
                style_context.theme,
                style_context.color_level,
                style_context.diff_backgrounds,
            ),
            style_add(
                style_context.theme,
                style_context.color_level,
                style_context.diff_backgrounds,
            ),
        ),
        DiffLineType::Delete => (
            '-',
            style_sign_del(
                style_context.theme,
                style_context.color_level,
                style_context.diff_backgrounds,
            ),
            style_del(
                style_context.theme,
                style_context.color_level,
                style_context.diff_backgrounds,
            ),
        ),
        DiffLineType::Context => (' ', style_context_line(), style_context_line()),
    };
    let mut lines: Vec<RtLine<'static>> = Vec::new();

    loop {
        // Fit the content for the current terminal row:
        // compute how many columns are available after the prefix, then split
        // at a UTF-8 character boundary so this row's chunk fits exactly.
        let available_content_cols = width.saturating_sub(prefix_cols + 1).max(1);
        let split_at_byte_index = remaining_text
            .char_indices()
            .nth(available_content_cols)
            .map(|(i, _)| i)
            .unwrap_or_else(|| remaining_text.len());
        let (chunk, rest) = remaining_text.split_at(split_at_byte_index);
        remaining_text = rest;

        if first {
            // Build gutter (right-aligned line number plus spacer) as a dimmed span
            let gutter = format!("{ln_str:>gutter_width$} ");
            let line = RtLine::from(vec![
                RtSpan::styled(gutter, gutter_style),
                RtSpan::styled(sign_char.to_string(), sign_style),
                RtSpan::styled(chunk.to_string(), content_style),
            ])
            .style(line_bg);
            lines.push(pad_line_to_width(line, width, line_bg));
            first = false;
        } else {
            // Continuation lines keep a space for the sign column so content aligns
            let gutter = format!("{:gutter_width$}  ", "");
            let line = RtLine::from(vec![
                RtSpan::styled(gutter, gutter_style),
                RtSpan::styled(chunk.to_string(), content_style),
            ])
            .style(line_bg);
            lines.push(pad_line_to_width(line, width, line_bg));
        }
        if remaining_text.is_empty() {
            break;
        }
    }
    lines
}

fn line_number_width(max_line_number: usize) -> usize {
    if max_line_number == 0 {
        1
    } else {
        max_line_number.to_string().len()
    }
}

fn diff_theme_for_bg(bg: Option<(u8, u8, u8)>) -> DiffTheme {
    if let Some(rgb) = bg
        && is_light(rgb)
    {
        return DiffTheme::Light;
    }
    DiffTheme::Dark
}

fn diff_theme() -> DiffTheme {
    diff_theme_for_bg(default_bg())
}

fn diff_color_level() -> DiffColorLevel {
    let color_level = supports_color::on_cached(supports_color::Stream::Stdout);
    diff_color_level_for_terminal(
        color_level.is_some_and(|level| level.has_16m),
        color_level.is_some_and(|level| level.has_256),
        terminal_info().name,
        std::env::var_os("WT_SESSION").is_some(),
        std::env::var_os("FORCE_COLOR").is_some(),
    )
}

fn diff_color_level_for_terminal(
    has_16m: bool,
    has_256: bool,
    terminal_name: TerminalName,
    has_wt_session: bool,
    has_force_color_override: bool,
) -> DiffColorLevel {
    if has_wt_session && !has_force_color_override {
        return DiffColorLevel::TrueColor;
    }

    let base = if has_16m {
        DiffColorLevel::TrueColor
    } else if has_256 {
        DiffColorLevel::Ansi256
    } else {
        DiffColorLevel::Ansi16
    };

    if base == DiffColorLevel::Ansi16
        && terminal_name == TerminalName::WindowsTerminal
        && !has_force_color_override
    {
        DiffColorLevel::TrueColor
    } else {
        base
    }
}

fn fallback_diff_backgrounds(
    theme: DiffTheme,
    color_level: DiffColorLevel,
) -> ResolvedDiffBackgrounds {
    match RichDiffColorLevel::from_diff_color_level(color_level) {
        Some(level) => ResolvedDiffBackgrounds {
            add: Some(add_line_bg(theme, level)),
            del: Some(del_line_bg(theme, level)),
        },
        None => ResolvedDiffBackgrounds::default(),
    }
}

#[allow(clippy::disallowed_methods)]
fn rgb_color(rgb: (u8, u8, u8)) -> Color {
    let (r, g, b) = rgb;
    Color::Rgb(r, g, b)
}

#[allow(clippy::disallowed_methods)]
fn indexed_color(index: u8) -> Color {
    Color::Indexed(index)
}

fn style_line_bg_for(kind: DiffLineType, diff_backgrounds: ResolvedDiffBackgrounds) -> Style {
    match kind {
        DiffLineType::Insert => diff_backgrounds
            .add
            .map_or_else(Style::default, |bg| Style::default().bg(bg)),
        DiffLineType::Delete => diff_backgrounds
            .del
            .map_or_else(Style::default, |bg| Style::default().bg(bg)),
        DiffLineType::Context => Style::default(),
    }
}

fn style_context_line() -> Style {
    Style::default()
}

fn add_line_bg(theme: DiffTheme, color_level: RichDiffColorLevel) -> Color {
    match (theme, color_level) {
        (DiffTheme::Dark, RichDiffColorLevel::TrueColor) => rgb_color(DARK_TC_ADD_LINE_BG_RGB),
        (DiffTheme::Dark, RichDiffColorLevel::Ansi256) => indexed_color(DARK_256_ADD_LINE_BG_IDX),
        (DiffTheme::Light, RichDiffColorLevel::TrueColor) => rgb_color(LIGHT_TC_ADD_LINE_BG_RGB),
        (DiffTheme::Light, RichDiffColorLevel::Ansi256) => indexed_color(LIGHT_256_ADD_LINE_BG_IDX),
    }
}

fn del_line_bg(theme: DiffTheme, color_level: RichDiffColorLevel) -> Color {
    match (theme, color_level) {
        (DiffTheme::Dark, RichDiffColorLevel::TrueColor) => rgb_color(DARK_TC_DEL_LINE_BG_RGB),
        (DiffTheme::Dark, RichDiffColorLevel::Ansi256) => indexed_color(DARK_256_DEL_LINE_BG_IDX),
        (DiffTheme::Light, RichDiffColorLevel::TrueColor) => rgb_color(LIGHT_TC_DEL_LINE_BG_RGB),
        (DiffTheme::Light, RichDiffColorLevel::Ansi256) => indexed_color(LIGHT_256_DEL_LINE_BG_IDX),
    }
}

fn light_gutter_fg(color_level: DiffColorLevel) -> Color {
    match color_level {
        DiffColorLevel::TrueColor => rgb_color(LIGHT_TC_GUTTER_FG_RGB),
        DiffColorLevel::Ansi256 => indexed_color(LIGHT_256_GUTTER_FG_IDX),
        DiffColorLevel::Ansi16 => Color::Black,
    }
}

fn style_gutter_for(kind: DiffLineType, theme: DiffTheme, color_level: DiffColorLevel) -> Style {
    match (
        theme,
        kind,
        RichDiffColorLevel::from_diff_color_level(color_level),
    ) {
        (DiffTheme::Light, DiffLineType::Insert, None) => {
            Style::default().fg(light_gutter_fg(color_level))
        }
        (DiffTheme::Light, DiffLineType::Delete, None) => {
            Style::default().fg(light_gutter_fg(color_level))
        }
        (DiffTheme::Light, DiffLineType::Insert, Some(level)) => Style::default()
            .fg(light_gutter_fg(color_level))
            .bg(add_line_bg(DiffTheme::Light, level)),
        (DiffTheme::Light, DiffLineType::Delete, Some(level)) => Style::default()
            .fg(light_gutter_fg(color_level))
            .bg(del_line_bg(DiffTheme::Light, level)),
        _ => style_gutter_dim(),
    }
}

fn style_sign_add(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match theme {
        DiffTheme::Light => Style::default().fg(Color::Green),
        DiffTheme::Dark => style_add(theme, color_level, diff_backgrounds),
    }
}

fn style_sign_del(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match theme {
        DiffTheme::Light => Style::default().fg(Color::Red),
        DiffTheme::Dark => style_del(theme, color_level, diff_backgrounds),
    }
}

fn style_add(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match (theme, color_level, diff_backgrounds.add) {
        (_, DiffColorLevel::Ansi16, _) => Style::default().fg(Color::Green),
        (DiffTheme::Light, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Light, DiffColorLevel::Ansi256, Some(bg)) => Style::default().bg(bg),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, Some(bg)) => {
            Style::default().fg(Color::Green).bg(bg)
        }
        (DiffTheme::Light, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Light, DiffColorLevel::Ansi256, None) => Style::default(),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, None) => Style::default().fg(Color::Green),
    }
}

fn style_del(
    theme: DiffTheme,
    color_level: DiffColorLevel,
    diff_backgrounds: ResolvedDiffBackgrounds,
) -> Style {
    match (theme, color_level, diff_backgrounds.del) {
        (_, DiffColorLevel::Ansi16, _) => Style::default().fg(Color::Red),
        (DiffTheme::Light, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Light, DiffColorLevel::Ansi256, Some(bg)) => Style::default().bg(bg),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, Some(bg))
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, Some(bg)) => {
            Style::default().fg(Color::Red).bg(bg)
        }
        (DiffTheme::Light, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Light, DiffColorLevel::Ansi256, None) => Style::default(),
        (DiffTheme::Dark, DiffColorLevel::TrueColor, None)
        | (DiffTheme::Dark, DiffColorLevel::Ansi256, None) => Style::default().fg(Color::Red),
    }
}

fn style_gutter_dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::text::Text;
    use ratatui::widgets::Paragraph;
    use ratatui::widgets::WidgetRef;
    use ratatui::widgets::Wrap;
    fn diff_summary_for_tests(changes: &HashMap<PathBuf, FileChange>) -> Vec<RtLine<'static>> {
        create_diff_summary(changes, &PathBuf::from("/"), 80)
    }

    fn snapshot_lines(name: &str, lines: Vec<RtLine<'static>>, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|f| {
                Paragraph::new(Text::from(lines))
                    .wrap(Wrap { trim: false })
                    .render_ref(f.area(), f.buffer_mut())
            })
            .expect("draw");
        assert_snapshot!(name, terminal.backend());
    }

    fn snapshot_lines_text(name: &str, lines: &[RtLine<'static>]) {
        // Convert Lines to plain text rows and trim trailing spaces so it's
        // easier to validate indentation visually in snapshots.
        let text = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .map(|s| s.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_snapshot!(name, text);
    }

    #[test]
    fn display_path_prefers_cwd_without_git_repo() {
        let cwd = if cfg!(windows) {
            PathBuf::from(r"C:\workspace\codex")
        } else {
            PathBuf::from("/workspace/codex")
        };
        let path = cwd.join("tui").join("example.png");

        let rendered = display_path_for(&path, &cwd);

        assert_eq!(
            rendered,
            PathBuf::from("tui")
                .join("example.png")
                .display()
                .to_string()
        );
    }

    #[test]
    fn truecolor_dark_theme_uses_configured_backgrounds() {
        let backgrounds = fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor);

        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            Style::default().bg(rgb_color(DARK_TC_ADD_LINE_BG_RGB))
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, backgrounds),
            Style::default().bg(rgb_color(DARK_TC_DEL_LINE_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Dark,
                DiffColorLevel::TrueColor
            ),
            style_gutter_dim()
        );
    }

    #[test]
    fn ansi256_dark_theme_uses_distinct_add_and_delete_backgrounds() {
        let backgrounds = fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256);

        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            Style::default().bg(indexed_color(DARK_256_ADD_LINE_BG_IDX))
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, backgrounds),
            Style::default().bg(indexed_color(DARK_256_DEL_LINE_BG_IDX))
        );
        assert_ne!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            style_line_bg_for(DiffLineType::Delete, backgrounds)
        );
    }

    #[test]
    fn ansi16_disables_line_and_gutter_backgrounds() {
        let backgrounds = fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::Ansi16);

        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            Style::default()
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, backgrounds),
            Style::default()
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Light,
                DiffColorLevel::Ansi16
            ),
            Style::default().fg(Color::Black)
        );
        assert_eq!(
            style_add(DiffTheme::Dark, DiffColorLevel::Ansi16, backgrounds),
            Style::default().fg(Color::Green)
        );
        assert_eq!(
            style_del(DiffTheme::Dark, DiffColorLevel::Ansi16, backgrounds),
            Style::default().fg(Color::Red)
        );
    }

    #[test]
    fn light_truecolor_theme_uses_readable_gutter_and_line_backgrounds() {
        let backgrounds = fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor);

        assert_eq!(
            style_line_bg_for(DiffLineType::Insert, backgrounds),
            Style::default().bg(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB))
        );
        assert_eq!(
            style_line_bg_for(DiffLineType::Delete, backgrounds),
            Style::default().bg(rgb_color(LIGHT_TC_DEL_LINE_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Insert,
                DiffTheme::Light,
                DiffColorLevel::TrueColor
            ),
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB))
        );
        assert_eq!(
            style_gutter_for(
                DiffLineType::Delete,
                DiffTheme::Light,
                DiffColorLevel::TrueColor
            ),
            Style::default()
                .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
                .bg(rgb_color(LIGHT_TC_DEL_LINE_BG_RGB))
        );
    }

    #[test]
    fn truecolor_dark_render_applies_background_to_wrapped_lines() {
        let style_context = DiffRenderStyleContext {
            theme: DiffTheme::Dark,
            color_level: DiffColorLevel::TrueColor,
            diff_backgrounds: fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor),
        };

        let lines = push_wrapped_diff_line_with_style_context(
            12,
            DiffLineType::Insert,
            "abcdefghij",
            8,
            line_number_width(12),
            style_context,
        );

        assert!(lines.len() > 1);
        for line in &lines {
            assert_eq!(line.style.bg, Some(rgb_color(DARK_TC_ADD_LINE_BG_RGB)));
        }
        assert_eq!(lines[0].spans[0].style, style_gutter_dim());
        assert_eq!(
            lines[0].spans[1].style,
            Style::default()
                .fg(Color::Green)
                .bg(rgb_color(DARK_TC_ADD_LINE_BG_RGB))
        );
    }

    #[test]
    fn truecolor_dark_insert_background_fills_rendered_row() {
        let bg = rgb_color(DARK_TC_ADD_LINE_BG_RGB);
        let style_context = DiffRenderStyleContext {
            theme: DiffTheme::Dark,
            color_level: DiffColorLevel::TrueColor,
            diff_backgrounds: fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor),
        };
        let lines = push_wrapped_diff_line_with_style_context(
            1,
            DiffLineType::Insert,
            "abc",
            12,
            line_number_width(1),
            style_context,
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));

        Paragraph::new(Text::from(lines)).render(buf.area, &mut buf);

        for x in 0..12 {
            assert_eq!(buf[(x, 0)].style().bg, Some(bg));
        }
    }

    #[test]
    fn truecolor_dark_delete_background_fills_rendered_row() {
        let bg = rgb_color(DARK_TC_DEL_LINE_BG_RGB);
        let style_context = DiffRenderStyleContext {
            theme: DiffTheme::Dark,
            color_level: DiffColorLevel::TrueColor,
            diff_backgrounds: fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor),
        };
        let lines = push_wrapped_diff_line_with_style_context(
            1,
            DiffLineType::Delete,
            "abc",
            12,
            line_number_width(1),
            style_context,
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));

        Paragraph::new(Text::from(lines)).render(buf.area, &mut buf);

        for x in 0..12 {
            assert_eq!(buf[(x, 0)].style().bg, Some(bg));
        }
    }

    #[test]
    fn context_background_does_not_fill_rendered_row() {
        let style_context = DiffRenderStyleContext {
            theme: DiffTheme::Dark,
            color_level: DiffColorLevel::TrueColor,
            diff_backgrounds: fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor),
        };
        let lines = push_wrapped_diff_line_with_style_context(
            1,
            DiffLineType::Context,
            "abc",
            12,
            line_number_width(1),
            style_context,
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));

        Paragraph::new(Text::from(lines)).render(buf.area, &mut buf);

        for x in 0..12 {
            assert_ne!(
                buf[(x, 0)].style().bg,
                Some(rgb_color(DARK_TC_ADD_LINE_BG_RGB))
            );
            assert_ne!(
                buf[(x, 0)].style().bg,
                Some(rgb_color(DARK_TC_DEL_LINE_BG_RGB))
            );
        }
    }

    #[test]
    fn wrapped_insert_background_fills_every_rendered_row() {
        let bg = rgb_color(DARK_TC_ADD_LINE_BG_RGB);
        let style_context = DiffRenderStyleContext {
            theme: DiffTheme::Dark,
            color_level: DiffColorLevel::TrueColor,
            diff_backgrounds: fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor),
        };
        let lines = push_wrapped_diff_line_with_style_context(
            12,
            DiffLineType::Insert,
            "abcdefghij",
            8,
            line_number_width(12),
            style_context,
        );
        assert!(lines.len() > 1);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, lines.len() as u16));

        Paragraph::new(Text::from(lines)).render(buf.area, &mut buf);

        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                assert_eq!(buf[(x, y)].style().bg, Some(bg));
            }
        }
    }

    #[test]
    fn windows_terminal_promotes_ansi16_to_truecolor_for_diffs() {
        assert_eq!(
            diff_color_level_for_terminal(
                false,
                false,
                TerminalName::WindowsTerminal,
                false,
                false,
            ),
            DiffColorLevel::TrueColor
        );
    }

    #[test]
    fn wt_session_promotes_ansi16_to_truecolor_for_diffs() {
        assert_eq!(
            diff_color_level_for_terminal(false, false, TerminalName::Unknown, true, false),
            DiffColorLevel::TrueColor
        );
    }

    #[test]
    fn force_color_override_preserves_explicit_truecolor_on_windows_terminal() {
        assert_eq!(
            diff_color_level_for_terminal(true, false, TerminalName::WindowsTerminal, true, true),
            DiffColorLevel::TrueColor
        );
    }

    #[test]
    fn force_color_override_keeps_ansi16_on_windows_terminal() {
        assert_eq!(
            diff_color_level_for_terminal(false, false, TerminalName::WindowsTerminal, false, true),
            DiffColorLevel::Ansi16
        );
    }

    #[test]
    fn ui_snapshot_wrap_behavior_insert() {
        // Narrow width to force wrapping within our diff line rendering
        let long_line = "this is a very long line that should wrap across multiple terminal columns and continue";

        // Call the wrapping function directly so we can precisely control the width
        let style_context = DiffRenderStyleContext {
            theme: DiffTheme::Dark,
            color_level: DiffColorLevel::Ansi16,
            diff_backgrounds: fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        };
        let lines = push_wrapped_diff_line_with_style_context(
            1,
            DiffLineType::Insert,
            long_line,
            80,
            line_number_width(1),
            style_context,
        );

        // Render into a small terminal to capture the visual layout
        snapshot_lines("wrap_behavior_insert", lines, 90, 8);
    }

    #[test]
    fn ui_snapshot_apply_update_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        let original = "line one\nline two\nline three\n";
        let modified = "line one\nline two changed\nline three\n";
        let patch = diffy::create_patch(original, modified).to_string();

        changes.insert(
            PathBuf::from("example.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_update_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_with_rename_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        let original = "A\nB\nC\n";
        let modified = "A\nB changed\nC\n";
        let patch = diffy::create_patch(original, modified).to_string();

        changes.insert(
            PathBuf::from("old_name.rs"),
            FileChange::Update {
                unified_diff: patch,
                move_path: Some(PathBuf::from("new_name.rs")),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_update_with_rename_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_multiple_files_block() {
        // Two files: one update and one add, to exercise combined header and per-file rows
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

        // File a.txt: single-line replacement (one delete, one insert)
        let patch_a = diffy::create_patch("one\n", "one changed\n").to_string();
        changes.insert(
            PathBuf::from("a.txt"),
            FileChange::Update {
                unified_diff: patch_a,
                move_path: None,
            },
        );

        // File b.txt: newly added with one line
        changes.insert(
            PathBuf::from("b.txt"),
            FileChange::Add {
                content: "new\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_multiple_files_block", lines, 80, 14);
    }

    #[test]
    fn ui_snapshot_apply_add_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("new_file.txt"),
            FileChange::Add {
                content: "alpha\nbeta\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);

        snapshot_lines("apply_add_block", lines, 80, 10);
    }

    #[test]
    fn ui_snapshot_apply_delete_block() {
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("tmp_delete_example.txt"),
            FileChange::Delete {
                content: "first\nsecond\nthird\n".to_string(),
            },
        );

        let lines = diff_summary_for_tests(&changes);
        snapshot_lines("apply_delete_block", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_block_wraps_long_lines() {
        // Create a patch with a long modified line to force wrapping
        let original = "line 1\nshort\nline 3\n";
        let modified = "line 1\nshort this_is_a_very_long_modified_line_that_should_wrap_across_multiple_terminal_columns_and_continue_even_further_beyond_eighty_columns_to_force_multiple_wraps\nline 3\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("long_example.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 72);

        // Render with backend width wider than wrap width to avoid Paragraph auto-wrap.
        snapshot_lines("apply_update_block_wraps_long_lines", lines, 80, 12);
    }

    #[test]
    fn ui_snapshot_apply_update_block_wraps_long_lines_text() {
        // This mirrors the desired layout example: sign only on first inserted line,
        // subsequent wrapped pieces start aligned under the line number gutter.
        let original = "1\n2\n3\n4\n";
        let modified = "1\nadded long line which wraps and_if_there_is_a_long_token_it_will_be_broken\n3\n4 context line which also wraps across\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("wrap_demo.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 28);
        snapshot_lines_text("apply_update_block_wraps_long_lines_text", &lines);
    }

    #[test]
    fn ui_snapshot_apply_update_block_line_numbers_three_digits_text() {
        let original = (1..=110).map(|i| format!("line {i}\n")).collect::<String>();
        let modified = (1..=110)
            .map(|i| {
                if i == 100 {
                    format!("line {i} changed\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect::<String>();
        let patch = diffy::create_patch(&original, &modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("hundreds.txt"),
            FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        let lines = create_diff_summary(&changes, &PathBuf::from("/"), 80);
        snapshot_lines_text("apply_update_block_line_numbers_three_digits_text", &lines);
    }

    #[test]
    fn ui_snapshot_apply_update_block_relativizes_path() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let abs_old = cwd.join("abs_old.rs");
        let abs_new = cwd.join("abs_new.rs");

        let original = "X\nY\n";
        let modified = "X changed\nY\n";
        let patch = diffy::create_patch(original, modified).to_string();

        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
        changes.insert(
            abs_old,
            FileChange::Update {
                unified_diff: patch,
                move_path: Some(abs_new),
            },
        );

        let lines = create_diff_summary(&changes, &cwd, 80);

        snapshot_lines("apply_update_block_relativizes_path", lines, 80, 10);
    }
}
