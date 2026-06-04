//! The bottom-pane footer renders transient hints and context indicators.
//!
//! The footer is pure rendering: it formats `FooterProps` into `Line`s without mutating any state.
//! It intentionally does not decide *which* footer content should be shown; that is owned by the
//! `ChatComposer` (which selects a `FooterMode`) and by higher-level state machines like
//! `ChatWidget` (which decides when quit/interrupt is allowed).
//!
//! Some footer content is time-based rather than event-based, such as the "press again to quit"
//! hint. The owning widgets schedule redraws so time-based hints can expire even if the UI is
//! otherwise idle.
//!
//! Single-line collapse overview:
//! 1. The composer decides the current `FooterMode` and hint flags, then calls
//!    `single_line_footer_layout` for the base single-line modes.
//! 2. `single_line_footer_layout` applies the width-based fallback rules:
//!    (If this description is hard to follow, just try it out by resizing
//!    your terminal width; these rules were built out of trial and error.)
//!    - Start with the fullest left-side hint plus the right-side context.
//!    - When the queue hint is active, prefer keeping that queue hint visible,
//!      even if it means dropping the right-side context earlier; the queue
//!      hint may also be shortened before it is removed.
//!    - When the queue hint is not active but the identity change hint is applicable,
//!      drop "? for shortcuts" before dropping "(shift+tab to change)".
//!    - If "(shift+tab to change)" cannot fit, keep the right-side identity label
//!      without the hint if that fits.
//!    - Finally, try a mode-only line (with and without context), and fall
//!      back to no left-side footer if nothing can fit.
//! 3. When collapse chooses a specific line, callers render it via
//!    `render_footer_line`. Otherwise, callers render the straightforward
//!    mode-to-text mapping via `render_footer_from_props`.
//!
//! In short: `single_line_footer_layout` chooses *what* best fits, and the two
//! render helpers choose whether to draw the chosen line or the default
//! `FooterProps` mapping.
use crate::product::tui_app::key_hint;
use crate::product::tui_app::key_hint::KeyBinding;
use crate::product::tui_app::render::line_utils::prefix_lines;
use crate::product::tui_app::status::format_tokens_compact;
use crate::product::tui_app::ui_consts::FOOTER_INDENT_COLS;
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

/// The rendering inputs for the footer area under the composer.
///
/// Callers are expected to construct `FooterProps` from higher-level state (`ChatComposer`,
/// `BottomPane`, and `ChatWidget`) and pass it to the footer render helpers
/// (`render_footer_from_props` or the single-line collapse logic). The footer
/// treats these values as authoritative and does not attempt to infer missing
/// state (for example, it does not query whether a task is running).
#[derive(Clone, Debug)]
pub(crate) struct FooterProps {
    pub(crate) mode: FooterMode,
    pub(crate) esc_backtrack_hint: bool,
    pub(crate) use_shift_enter_hint: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) is_task_running: bool,
    pub(crate) is_wsl: bool,
    /// Which key the user must press again to quit.
    ///
    /// This is rendered when `mode` is `FooterMode::QuitShortcutReminder`.
    pub(crate) quit_shortcut_key: KeyBinding,
    pub(crate) context_window_percent: Option<i64>,
    pub(crate) context_window_used_tokens: Option<i64>,
    pub(crate) model_name: String,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) cwd: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdentityIndicator {
    Nobody,
    Planner,
    Programmer,
    Explorer,
    Reviewer,
}

const FOOTER_CONTEXT_GAP_COLS: u16 = 1;
const IDENTITY_CHANGE_HINT: &str = "shift+tab to change";

impl IdentityIndicator {
    fn name(self) -> &'static str {
        match self {
            IdentityIndicator::Nobody => "nobody",
            IdentityIndicator::Planner => "planner",
            IdentityIndicator::Programmer => "programmer",
            IdentityIndicator::Explorer => "explorer",
            IdentityIndicator::Reviewer => "reviewer",
        }
    }

    fn label(self, show_cycle_hint: bool) -> String {
        let suffix = if show_cycle_hint {
            format!(" ({IDENTITY_CHANGE_HINT})")
        } else {
            String::new()
        };
        let name = self.name();
        format!("Identity {name}{suffix}")
    }

    fn styled_span(self, show_cycle_hint: bool) -> Span<'static> {
        let label = self.label(show_cycle_hint);
        Span::from(label).magenta()
    }
}

/// Selects which footer content is rendered.
///
/// The current mode is owned by `ChatComposer`, which may override it based on transient state
/// (for example, showing `QuitShortcutReminder` only while its timer is active).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FooterMode {
    /// Transient "press again to quit" reminder (Ctrl+C/Ctrl+D).
    QuitShortcutReminder,
    /// Multi-line shortcut overlay shown after pressing `?`.
    ShortcutOverlay,
    /// Transient "press Esc again" hint shown after the first Esc while idle.
    EscHint,
    /// Base single-line footer when the composer is empty.
    ComposerEmpty,
    /// Base single-line footer when the composer contains a draft.
    ///
    /// The shortcuts hint is suppressed here; when a task is running with
    /// steer enabled, this mode can show the queue hint instead.
    ComposerHasDraft,
}

pub(crate) fn toggle_shortcut_mode(
    current: FooterMode,
    ctrl_c_hint: bool,
    is_empty: bool,
) -> FooterMode {
    if ctrl_c_hint && matches!(current, FooterMode::QuitShortcutReminder) {
        return current;
    }

    let base_mode = if is_empty {
        FooterMode::ComposerEmpty
    } else {
        FooterMode::ComposerHasDraft
    };

    match current {
        FooterMode::ShortcutOverlay | FooterMode::QuitShortcutReminder => base_mode,
        _ => FooterMode::ShortcutOverlay,
    }
}

pub(crate) fn esc_hint_mode(current: FooterMode, is_task_running: bool) -> FooterMode {
    if is_task_running {
        current
    } else {
        FooterMode::EscHint
    }
}

pub(crate) fn reset_mode_after_activity(current: FooterMode) -> FooterMode {
    match current {
        FooterMode::EscHint
        | FooterMode::ShortcutOverlay
        | FooterMode::QuitShortcutReminder
        | FooterMode::ComposerHasDraft => FooterMode::ComposerEmpty,
        other => other,
    }
}

pub(crate) fn footer_height(props: &FooterProps) -> u16 {
    footer_from_props_lines(props.clone(), None, false, false, false).len() as u16
}

/// Render a single precomputed footer line.
pub(crate) fn render_footer_line(area: Rect, buf: &mut Buffer, line: Line<'static>) {
    Paragraph::new(prefix_lines(
        vec![line],
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

/// Render footer content directly from `FooterProps`.
///
/// This is intentionally not part of the width-based collapse/fallback logic.
/// Transient instructional states (shortcut overlay, Esc hint, quit reminder)
/// prioritize "what to do next" instructions and currently suppress the
/// identity label entirely. When collapse logic has already chosen a
/// specific single line, prefer `render_footer_line`.
pub(crate) fn render_footer_from_props(
    area: Rect,
    buf: &mut Buffer,
    props: FooterProps,
    identity_indicator: Option<IdentityIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) {
    Paragraph::new(prefix_lines(
        footer_from_props_lines(
            props,
            identity_indicator,
            show_cycle_hint,
            show_shortcuts_hint,
            show_queue_hint,
        ),
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

pub(crate) fn left_fits(area: Rect, left_width: u16) -> bool {
    let max_width = area.width.saturating_sub(FOOTER_INDENT_COLS as u16);
    left_width <= max_width
}

pub(crate) enum SummaryLeft {
    Default,
    Custom(Line<'static>),
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FooterInfoState {
    show_reasoning: bool,
    show_context: bool,
    show_cwd: bool,
}

fn mode_indicator_line(
    identity_indicator: Option<IdentityIndicator>,
    show_cycle_hint: bool,
) -> Option<Line<'static>> {
    identity_indicator.map(|indicator| Line::from(vec![indicator.styled_span(show_cycle_hint)]))
}

fn footer_info_line(props: &FooterProps, state: FooterInfoState) -> Line<'static> {
    let mut spans = vec![Span::from(props.model_name.clone()).dim()];

    if state.show_reasoning
        && let Some(reasoning_effort) = &props.reasoning_effort
    {
        spans.push(" ".into());
        spans.push(Span::from(reasoning_effort.clone()).dim());
    }

    if state.show_context {
        spans.push(" · ".dim());
        spans.extend(
            context_window_line(
                props.context_window_percent,
                props.context_window_used_tokens,
            )
            .spans,
        );
    }

    if state.show_cwd && !props.cwd.is_empty() {
        spans.push(" · ".dim());
        spans.push(Span::from(props.cwd.clone()).dim());
    }

    Line::from(spans)
}

fn footer_info_variants(props: &FooterProps) -> Vec<(FooterInfoState, Line<'static>)> {
    let mut variants = Vec::new();
    let candidate_states = [
        FooterInfoState {
            show_reasoning: true,
            show_context: true,
            show_cwd: true,
        },
        FooterInfoState {
            show_reasoning: true,
            show_context: true,
            show_cwd: false,
        },
        FooterInfoState {
            show_reasoning: true,
            show_context: false,
            show_cwd: false,
        },
        FooterInfoState {
            show_reasoning: false,
            show_context: false,
            show_cwd: false,
        },
    ];

    for state in candidate_states {
        let line = footer_info_line(props, state);
        if variants
            .last()
            .is_some_and(|(_, previous)| previous == &line)
        {
            continue;
        }
        variants.push((state, line));
    }

    variants
}

/// Compute the single-line footer layout and the right-side mode label.
pub(crate) fn single_line_footer_layout(
    area: Rect,
    props: &FooterProps,
    identity_indicator: Option<IdentityIndicator>,
    show_cycle_hint: bool,
) -> (SummaryLeft, Option<Line<'static>>) {
    let left_variants = footer_info_variants(props);
    let right_variants = if show_cycle_hint {
        [
            mode_indicator_line(identity_indicator, true),
            mode_indicator_line(identity_indicator, false),
            None,
        ]
    } else {
        [mode_indicator_line(identity_indicator, false), None, None]
    };

    for right_line in right_variants
        .into_iter()
        .flatten()
        .map(Some)
        .chain(std::iter::once(None))
    {
        let right_width = right_line
            .as_ref()
            .map(|line| line.width() as u16)
            .unwrap_or(0);

        for (idx, (_, left_line)) in left_variants.iter().enumerate() {
            let left_width = left_line.width() as u16;
            let fits = if right_line.is_some() {
                can_show_left_with_context(area, left_width, right_width)
            } else {
                left_fits(area, left_width)
            };

            if !fits {
                continue;
            }

            let summary = if idx == 0 {
                SummaryLeft::Default
            } else {
                SummaryLeft::Custom(left_line.clone())
            };
            return (summary, right_line);
        }

        if right_line.is_some() && left_fits(area, right_width) {
            return (SummaryLeft::None, right_line);
        }
    }

    (SummaryLeft::None, None)
}

fn right_aligned_x(area: Rect, content_width: u16) -> Option<u16> {
    if area.is_empty() {
        return None;
    }

    let right_padding = FOOTER_INDENT_COLS as u16;
    let max_width = area.width.saturating_sub(right_padding);
    if content_width == 0 || max_width == 0 {
        return None;
    }

    if content_width >= max_width {
        return Some(area.x.saturating_add(right_padding));
    }

    Some(
        area.x
            .saturating_add(area.width)
            .saturating_sub(content_width)
            .saturating_sub(right_padding),
    )
}

pub(crate) fn can_show_left_with_context(area: Rect, left_width: u16, context_width: u16) -> bool {
    let Some(context_x) = right_aligned_x(area, context_width) else {
        return true;
    };
    if left_width == 0 {
        return true;
    }
    let left_extent = FOOTER_INDENT_COLS as u16 + left_width + FOOTER_CONTEXT_GAP_COLS;
    left_extent <= context_x.saturating_sub(area.x)
}

pub(crate) fn render_context_right(area: Rect, buf: &mut Buffer, line: &Line<'static>) {
    if area.is_empty() {
        return;
    }

    let context_width = line.width() as u16;
    let Some(mut x) = right_aligned_x(area, context_width) else {
        return;
    };
    let y = area.y + area.height.saturating_sub(1);
    let max_x = area.x.saturating_add(area.width);

    for span in &line.spans {
        if x >= max_x {
            break;
        }
        let span_width = span.width() as u16;
        if span_width == 0 {
            continue;
        }
        let remaining = max_x.saturating_sub(x);
        let draw_width = span_width.min(remaining);
        buf.set_span(x, y, span, draw_width);
        x = x.saturating_add(span_width);
    }
}

pub(crate) fn inset_footer_hint_area(mut area: Rect) -> Rect {
    if area.width > 2 {
        area.x += 2;
        area.width = area.width.saturating_sub(2);
    }
    area
}

pub(crate) fn render_footer_hint_items(area: Rect, buf: &mut Buffer, items: &[(String, String)]) {
    if items.is_empty() {
        return;
    }

    footer_hint_items_line(items).render(inset_footer_hint_area(area), buf);
}

/// Map `FooterProps` to footer lines without width-based collapse.
///
/// This is the canonical FooterMode-to-text mapping. It powers transient,
/// instructional states (shortcut overlay, Esc hint, quit reminder) and also
/// the default rendering for base states when collapse is not applied (or when
/// `single_line_footer_layout` returns `SummaryLeft::Default`). Collapse and
/// fallback decisions live in `single_line_footer_layout`; this function only
/// formats the chosen/default content.
fn footer_from_props_lines(
    props: FooterProps,
    _identity_indicator: Option<IdentityIndicator>,
    _show_cycle_hint: bool,
    _show_shortcuts_hint: bool,
    _show_queue_hint: bool,
) -> Vec<Line<'static>> {
    match props.mode {
        FooterMode::QuitShortcutReminder => {
            vec![quit_shortcut_reminder_line(props.quit_shortcut_key)]
        }
        FooterMode::ComposerEmpty => vec![footer_info_line(
            &props,
            FooterInfoState {
                show_reasoning: true,
                show_context: true,
                show_cwd: true,
            },
        )],
        FooterMode::ShortcutOverlay => {
            let state = ShortcutsState {
                use_shift_enter_hint: props.use_shift_enter_hint,
                esc_backtrack_hint: props.esc_backtrack_hint,
                is_wsl: props.is_wsl,
            };
            shortcut_overlay_lines(state)
        }
        FooterMode::EscHint => vec![esc_hint_line(props.esc_backtrack_hint)],
        FooterMode::ComposerHasDraft => vec![footer_info_line(
            &props,
            FooterInfoState {
                show_reasoning: true,
                show_context: true,
                show_cwd: true,
            },
        )],
    }
}

fn footer_hint_items_line(items: &[(String, String)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(items.len() * 4);
    for (idx, (key, label)) in items.iter().enumerate() {
        spans.push(" ".into());
        spans.push(key.clone().bold());
        spans.push(format!(" {label}").into());
        if idx + 1 != items.len() {
            spans.push("   ".into());
        }
    }
    Line::from(spans)
}

#[derive(Clone, Copy, Debug)]
struct ShortcutsState {
    use_shift_enter_hint: bool,
    esc_backtrack_hint: bool,
    is_wsl: bool,
}

fn quit_shortcut_reminder_line(key: KeyBinding) -> Line<'static> {
    Line::from(vec![key.into(), " again to quit".into()]).dim()
}

fn esc_hint_line(esc_backtrack_hint: bool) -> Line<'static> {
    let esc = key_hint::plain(KeyCode::Esc);
    if esc_backtrack_hint {
        Line::from(vec![esc.into(), " again to edit previous message".into()]).dim()
    } else {
        Line::from(vec![
            esc.into(),
            " ".into(),
            esc.into(),
            " to edit previous message".into(),
        ])
        .dim()
    }
}

fn shortcut_overlay_lines(state: ShortcutsState) -> Vec<Line<'static>> {
    let mut commands = Line::from("");
    let mut shell_commands = Line::from("");
    let mut newline = Line::from("");
    let mut queue_message_tab = Line::from("");
    let mut file_paths = Line::from("");
    let mut paste_image = Line::from("");
    let mut external_editor = Line::from("");
    let mut edit_previous = Line::from("");
    let mut quit = Line::from("");
    let mut show_transcript = Line::from("");

    for descriptor in SHORTCUTS {
        if let Some(text) = descriptor.overlay_entry(state) {
            match descriptor.id {
                ShortcutId::Commands => commands = text,
                ShortcutId::ShellCommands => shell_commands = text,
                ShortcutId::InsertNewline => newline = text,
                ShortcutId::QueueMessageTab => queue_message_tab = text,
                ShortcutId::FilePaths => file_paths = text,
                ShortcutId::PasteImage => paste_image = text,
                ShortcutId::ExternalEditor => external_editor = text,
                ShortcutId::EditPrevious => edit_previous = text,
                ShortcutId::Quit => quit = text,
                ShortcutId::ShowTranscript => show_transcript = text,
            }
        }
    }

    let mut ordered = vec![
        commands,
        shell_commands,
        newline,
        queue_message_tab,
        file_paths,
        paste_image,
        external_editor,
        edit_previous,
        quit,
    ];
    ordered.push(Line::from(""));
    ordered.push(show_transcript);

    build_columns(ordered)
}

fn build_columns(entries: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if entries.is_empty() {
        return Vec::new();
    }

    const COLUMNS: usize = 2;
    const COLUMN_PADDING: [usize; COLUMNS] = [4, 4];
    const COLUMN_GAP: usize = 4;

    let rows = entries.len().div_ceil(COLUMNS);
    let target_len = rows * COLUMNS;
    let mut entries = entries;
    if entries.len() < target_len {
        entries.extend(std::iter::repeat_n(
            Line::from(""),
            target_len - entries.len(),
        ));
    }

    let mut column_widths = [0usize; COLUMNS];

    for (idx, entry) in entries.iter().enumerate() {
        let column = idx % COLUMNS;
        column_widths[column] = column_widths[column].max(entry.width());
    }

    for (idx, width) in column_widths.iter_mut().enumerate() {
        *width += COLUMN_PADDING[idx];
    }

    entries
        .chunks(COLUMNS)
        .map(|chunk| {
            let mut line = Line::from("");
            for (col, entry) in chunk.iter().enumerate() {
                line.extend(entry.spans.clone());
                if col < COLUMNS - 1 {
                    let target_width = column_widths[col];
                    let padding = target_width.saturating_sub(entry.width()) + COLUMN_GAP;
                    line.push_span(Span::from(" ".repeat(padding)));
                }
            }
            line.dim()
        })
        .collect()
}

pub(crate) fn context_window_line(percent: Option<i64>, used_tokens: Option<i64>) -> Line<'static> {
    if let Some(percent) = percent {
        let percent = percent.clamp(0, 100);
        return Line::from(vec![Span::from(format!("{percent}% left")).dim()]);
    }

    if let Some(used_tokens) = used_tokens {
        let used_tokens = format_tokens_compact(used_tokens);
        return Line::from(vec![Span::from(format!("{used_tokens} used")).dim()]);
    }

    Line::from(vec![Span::from("100% left").dim()])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShortcutId {
    Commands,
    ShellCommands,
    InsertNewline,
    QueueMessageTab,
    FilePaths,
    PasteImage,
    ExternalEditor,
    EditPrevious,
    Quit,
    ShowTranscript,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShortcutBinding {
    key: KeyBinding,
    condition: DisplayCondition,
}

impl ShortcutBinding {
    fn matches(&self, state: ShortcutsState) -> bool {
        self.condition.matches(state)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayCondition {
    Always,
    WhenShiftEnterHint,
    WhenNotShiftEnterHint,
    WhenUnderWSL,
}

impl DisplayCondition {
    fn matches(self, state: ShortcutsState) -> bool {
        match self {
            DisplayCondition::Always => true,
            DisplayCondition::WhenShiftEnterHint => state.use_shift_enter_hint,
            DisplayCondition::WhenNotShiftEnterHint => !state.use_shift_enter_hint,
            DisplayCondition::WhenUnderWSL => state.is_wsl,
        }
    }
}

struct ShortcutDescriptor {
    id: ShortcutId,
    bindings: &'static [ShortcutBinding],
    prefix: &'static str,
    label: &'static str,
}

impl ShortcutDescriptor {
    fn binding_for(&self, state: ShortcutsState) -> Option<&'static ShortcutBinding> {
        self.bindings.iter().find(|binding| binding.matches(state))
    }

    fn overlay_entry(&self, state: ShortcutsState) -> Option<Line<'static>> {
        let binding = self.binding_for(state)?;
        let mut line = Line::from(vec![self.prefix.into(), binding.key.into()]);
        match self.id {
            ShortcutId::EditPrevious => {
                if state.esc_backtrack_hint {
                    line.push_span(" again to edit previous message");
                } else {
                    line.extend(vec![
                        " ".into(),
                        key_hint::plain(KeyCode::Esc).into(),
                        " to edit previous message".into(),
                    ]);
                }
            }
            _ => line.push_span(self.label),
        };
        Some(line)
    }
}

const SHORTCUTS: &[ShortcutDescriptor] = &[
    ShortcutDescriptor {
        id: ShortcutId::Commands,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('/')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for commands",
    },
    ShortcutDescriptor {
        id: ShortcutId::ShellCommands,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('!')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for shell commands",
    },
    ShortcutDescriptor {
        id: ShortcutId::InsertNewline,
        bindings: &[
            ShortcutBinding {
                key: key_hint::shift(KeyCode::Enter),
                condition: DisplayCondition::WhenShiftEnterHint,
            },
            ShortcutBinding {
                key: key_hint::ctrl(KeyCode::Char('j')),
                condition: DisplayCondition::WhenNotShiftEnterHint,
            },
        ],
        prefix: "",
        label: " for newline",
    },
    ShortcutDescriptor {
        id: ShortcutId::QueueMessageTab,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Tab),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to queue message",
    },
    ShortcutDescriptor {
        id: ShortcutId::FilePaths,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Char('@')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " for file paths",
    },
    ShortcutDescriptor {
        id: ShortcutId::PasteImage,
        // Show Ctrl+Alt+V when running under WSL (terminals often intercept plain
        // Ctrl+V); otherwise fall back to Ctrl+V.
        bindings: &[
            ShortcutBinding {
                key: key_hint::ctrl_alt(KeyCode::Char('v')),
                condition: DisplayCondition::WhenUnderWSL,
            },
            ShortcutBinding {
                key: key_hint::ctrl(KeyCode::Char('v')),
                condition: DisplayCondition::Always,
            },
        ],
        prefix: "",
        label: " to paste images",
    },
    ShortcutDescriptor {
        id: ShortcutId::ExternalEditor,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('g')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to edit in external editor",
    },
    ShortcutDescriptor {
        id: ShortcutId::EditPrevious,
        bindings: &[ShortcutBinding {
            key: key_hint::plain(KeyCode::Esc),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: "",
    },
    ShortcutDescriptor {
        id: ShortcutId::Quit,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('c')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to exit",
    },
    ShortcutDescriptor {
        id: ShortcutId::ShowTranscript,
        bindings: &[ShortcutBinding {
            key: key_hint::ctrl(KeyCode::Char('t')),
            condition: DisplayCondition::Always,
        }],
        prefix: "",
        label: " to view transcript",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::style::Modifier;
    use ratatui::style::Style;

    fn test_footer_props(mode: FooterMode) -> FooterProps {
        FooterProps {
            mode,
            esc_backtrack_hint: false,
            use_shift_enter_hint: false,
            is_task_running: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            context_window_percent: None,
            context_window_used_tokens: None,
            model_name: "gpt-5.4".to_string(),
            reasoning_effort: Some("high".to_string()),
            cwd: "~/Workspace/lha".to_string(),
        }
    }

    fn snapshot_footer(name: &str, props: FooterProps) {
        snapshot_footer_with_mode_indicator(name, 80, props, None);
    }

    fn snapshot_footer_with_mode_indicator(
        name: &str,
        width: u16,
        props: FooterProps,
        identity_indicator: Option<IdentityIndicator>,
    ) {
        let height = footer_height(&props).max(1);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, f.area().width, height);
                let show_cycle_hint = !props.is_task_running;
                if matches!(
                    props.mode,
                    FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                ) {
                    let (summary_left, right_line) = single_line_footer_layout(
                        area,
                        &props,
                        identity_indicator,
                        show_cycle_hint,
                    );
                    match summary_left {
                        SummaryLeft::Default => {
                            render_footer_from_props(
                                area,
                                f.buffer_mut(),
                                props.clone(),
                                identity_indicator,
                                show_cycle_hint,
                                false,
                                false,
                            );
                        }
                        SummaryLeft::Custom(line) => {
                            render_footer_line(area, f.buffer_mut(), line);
                        }
                        SummaryLeft::None => {}
                    }
                    if let Some(line) = right_line.as_ref() {
                        render_context_right(area, f.buffer_mut(), line);
                    }
                } else {
                    render_footer_from_props(
                        area,
                        f.buffer_mut(),
                        props,
                        identity_indicator,
                        show_cycle_hint,
                        false,
                        false,
                    );
                }
            })
            .unwrap();
        assert_snapshot!(name, terminal.backend());
    }

    fn span_style(line: &Line<'_>, text: &str) -> Style {
        line.spans
            .iter()
            .find(|span| span.content.as_ref() == text)
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("missing span: {text}"))
    }

    #[test]
    fn footer_snapshots() {
        snapshot_footer(
            "footer_shortcuts_default",
            test_footer_props(FooterMode::ComposerEmpty),
        );

        snapshot_footer(
            "footer_shortcuts_shift_and_esc",
            FooterProps {
                mode: FooterMode::ShortcutOverlay,
                esc_backtrack_hint: true,
                use_shift_enter_hint: true,
                ..test_footer_props(FooterMode::ShortcutOverlay)
            },
        );

        snapshot_footer(
            "footer_ctrl_c_quit_idle",
            test_footer_props(FooterMode::QuitShortcutReminder),
        );

        snapshot_footer(
            "footer_ctrl_c_quit_running",
            FooterProps {
                mode: FooterMode::QuitShortcutReminder,
                is_task_running: true,
                ..test_footer_props(FooterMode::QuitShortcutReminder)
            },
        );

        snapshot_footer(
            "footer_esc_hint_idle",
            test_footer_props(FooterMode::EscHint),
        );

        snapshot_footer(
            "footer_esc_hint_primed",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: true,
                ..test_footer_props(FooterMode::EscHint)
            },
        );

        snapshot_footer(
            "footer_shortcuts_context_running",
            FooterProps {
                mode: FooterMode::ComposerEmpty,
                is_task_running: true,
                context_window_percent: Some(72),
                ..test_footer_props(FooterMode::ComposerEmpty)
            },
        );

        snapshot_footer(
            "footer_context_tokens_used",
            FooterProps {
                context_window_used_tokens: Some(123_456),
                ..test_footer_props(FooterMode::ComposerEmpty)
            },
        );

        snapshot_footer(
            "footer_composer_has_draft_queue_hint_disabled",
            FooterProps {
                mode: FooterMode::ComposerHasDraft,
                esc_backtrack_hint: false,
                use_shift_enter_hint: false,
                is_task_running: true,
                ..test_footer_props(FooterMode::ComposerHasDraft)
            },
        );

        snapshot_footer(
            "footer_composer_has_draft_queue_hint_enabled",
            FooterProps {
                mode: FooterMode::ComposerHasDraft,
                is_task_running: true,
                ..test_footer_props(FooterMode::ComposerHasDraft)
            },
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            ..test_footer_props(FooterMode::ComposerEmpty)
        };

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_wide",
            120,
            props.clone(),
            Some(IdentityIndicator::Planner),
        );

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_code_wide",
            120,
            props.clone(),
            Some(IdentityIndicator::Programmer),
        );

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_narrow_overlap_hides",
            50,
            props,
            Some(IdentityIndicator::Planner),
        );

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_mode_only_when_left_too_wide",
            40,
            FooterProps {
                model_name: "gpt-5.4-super-long-model-name".to_string(),
                ..test_footer_props(FooterMode::ComposerEmpty)
            },
            Some(IdentityIndicator::Planner),
        );

        let props = FooterProps {
            mode: FooterMode::ComposerEmpty,
            is_task_running: true,
            ..test_footer_props(FooterMode::ComposerEmpty)
        };

        snapshot_footer_with_mode_indicator(
            "footer_mode_indicator_running_hides_hint",
            120,
            props,
            Some(IdentityIndicator::Planner),
        );
    }

    #[test]
    fn footer_info_line_dims_model_effort_context_and_cwd() {
        let props = FooterProps {
            context_window_percent: Some(84),
            ..test_footer_props(FooterMode::ComposerEmpty)
        };

        let line = footer_info_line(
            &props,
            FooterInfoState {
                show_reasoning: true,
                show_context: true,
                show_cwd: true,
            },
        );

        let model_style = span_style(&line, "gpt-5.4");
        let effort_style = span_style(&line, "high");
        let context_style = span_style(&line, "84% left");
        let cwd_style = span_style(&line, "~/Workspace/lha");

        assert!(context_style.add_modifier.contains(Modifier::DIM));
        assert_eq!(model_style, context_style);
        assert_eq!(effort_style, context_style);
        assert_eq!(cwd_style, context_style);
    }

    #[test]
    fn nobody_identity_indicator_is_magenta() {
        let line = mode_indicator_line(Some(IdentityIndicator::Nobody), true)
            .expect("identity indicator should render");
        let style = span_style(&line, "Identity nobody (shift+tab to change)");

        assert_eq!(style.fg, Some(Color::Magenta));
        assert!(!style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn identity_indicators_include_change_hint_when_requested() {
        let cases = [
            (
                IdentityIndicator::Nobody,
                "Identity nobody (shift+tab to change)",
            ),
            (
                IdentityIndicator::Planner,
                "Identity planner (shift+tab to change)",
            ),
            (
                IdentityIndicator::Programmer,
                "Identity programmer (shift+tab to change)",
            ),
            (
                IdentityIndicator::Explorer,
                "Identity explorer (shift+tab to change)",
            ),
            (
                IdentityIndicator::Reviewer,
                "Identity reviewer (shift+tab to change)",
            ),
        ];

        for (indicator, expected) in cases {
            let line = mode_indicator_line(Some(indicator), true)
                .expect("identity indicator should render");
            assert_eq!(line.to_string(), expected);
        }
    }

    #[test]
    fn paste_image_shortcut_prefers_ctrl_alt_v_under_wsl() {
        let descriptor = SHORTCUTS
            .iter()
            .find(|descriptor| descriptor.id == ShortcutId::PasteImage)
            .expect("paste image shortcut");

        let is_wsl = {
            #[cfg(target_os = "linux")]
            {
                crate::product::tui_app::clipboard_paste::is_probably_wsl()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        };

        let expected_key = if is_wsl {
            key_hint::ctrl_alt(KeyCode::Char('v'))
        } else {
            key_hint::ctrl(KeyCode::Char('v'))
        };

        let actual_key = descriptor
            .binding_for(ShortcutsState {
                use_shift_enter_hint: false,
                esc_backtrack_hint: false,
                is_wsl,
            })
            .expect("shortcut binding")
            .key;

        assert_eq!(actual_key, expected_key);
    }
}
