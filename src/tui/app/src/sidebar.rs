use crate::buddy;
use crate::buddy::state::BuddyState;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

pub(crate) const SIDEBAR_MIN_TERMINAL_WIDTH: u16 = 120;
pub(crate) const SIDEBAR_MIN_WIDTH: u16 = 28;
pub(crate) const SIDEBAR_MAX_WIDTH: u16 = 42;
pub(crate) const SIDEBAR_PERCENT: u16 = 28;
pub(crate) const MAIN_MIN_WIDTH: u16 = 72;
const SIDEBAR_BUDDY_CONTENT_RESERVE: u16 = 2;

#[derive(Clone, Debug, Default)]
pub(crate) struct SidebarSnapshot {
    pub(crate) task: Option<TaskPanelSnapshot>,
    pub(crate) files: Vec<String>,
    pub(crate) mcp: Option<McpPanelSnapshot>,
    pub(crate) context: Option<ContextPanelSnapshot>,
}

#[derive(Clone, Debug)]
pub(crate) struct TaskPanelSnapshot {
    pub(crate) status: String,
    pub(crate) detail: Option<String>,
    pub(crate) queued_messages: usize,
    pub(crate) active_commands: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct McpPanelSnapshot {
    pub(crate) starting: usize,
    pub(crate) ready: usize,
    pub(crate) failed: Vec<String>,
    pub(crate) cancelled: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextPanelSnapshot {
    pub(crate) model: String,
    pub(crate) identity: String,
    pub(crate) used_tokens: i64,
    pub(crate) context_window: Option<i64>,
}

pub(crate) fn sidebar_width(total_width: u16) -> Option<u16> {
    if total_width < SIDEBAR_MIN_TERMINAL_WIDTH {
        return None;
    }
    let preferred = ((u32::from(total_width) * u32::from(SIDEBAR_PERCENT)) / 100) as u16;
    let width = preferred.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
    (total_width.saturating_sub(width) >= MAIN_MIN_WIDTH).then_some(width)
}

pub(crate) fn external_buddy_desired_height(buddy_state: Option<&BuddyState>) -> u16 {
    buddy_state
        .filter(|state| state.is_visible())
        .map(|state| {
            buddy::layout::full_required_height(state).saturating_add(SIDEBAR_BUDDY_CONTENT_RESERVE)
        })
        .unwrap_or(0)
}

pub(crate) struct SidebarWidget<'a> {
    pub(crate) snapshot: &'a SidebarSnapshot,
    pub(crate) buddy_state: Option<&'a BuddyState>,
    pub(crate) animations_enabled: bool,
}

impl Widget for SidebarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < SIDEBAR_MIN_WIDTH || area.height < 4 {
            return;
        }

        let buddy_height = self
            .buddy_state
            .filter(|state| state.is_visible())
            .map(buddy::layout::full_required_height)
            .unwrap_or(0)
            .min(area.height.saturating_sub(SIDEBAR_BUDDY_CONTENT_RESERVE));
        let [content_area, buddy_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(buddy_height)]).areas(area);

        let mut lines = Vec::new();
        push_task(&mut lines, self.snapshot.task.as_ref(), area.width);
        push_files(&mut lines, &self.snapshot.files, area.width);
        push_mcp(&mut lines, self.snapshot.mcp.as_ref(), area.width);
        push_context(&mut lines, self.snapshot.context.as_ref(), area.width);

        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_style(Style::default().dim()),
            )
            .render(content_area, buf);

        if let Some(state) = self.buddy_state
            && buddy_height > 0
        {
            buddy::render::render_buddy(buddy_area, buf, state, self.animations_enabled);
        }
    }
}

fn push_section(lines: &mut Vec<Line<'static>>, title: &'static str) {
    if !lines.is_empty() {
        lines.push("".into());
    }
    lines.push(title.bold().into());
}

fn push_task(lines: &mut Vec<Line<'static>>, task: Option<&TaskPanelSnapshot>, width: u16) {
    let Some(task) = task else {
        return;
    };
    push_section(lines, "Task");
    lines.push(Line::from(vec!["  ".into(), task.status.clone().cyan()]));
    if let Some(detail) = &task.detail {
        lines.push(Line::from(vec!["  ".into(), truncate(detail, width).dim()]));
    }
    if task.queued_messages > 0 {
        lines.push(Line::from(vec![
            "  queue ".dim(),
            task.queued_messages.to_string().magenta(),
        ]));
    }
    for command in task.active_commands.iter().take(3) {
        lines.push(Line::from(vec![
            "  $ ".dim(),
            truncate(command, width).into(),
        ]));
    }
}

fn push_files(lines: &mut Vec<Line<'static>>, files: &[String], width: u16) {
    if files.is_empty() {
        return;
    }
    push_section(lines, "Files");
    for file in files.iter().take(6) {
        lines.push(Line::from(vec![
            "  ".into(),
            truncate(file, width).magenta(),
        ]));
    }
    if files.len() > 6 {
        lines.push(format!("  +{} more", files.len() - 6).dim().into());
    }
}

fn push_mcp(lines: &mut Vec<Line<'static>>, mcp: Option<&McpPanelSnapshot>, _width: u16) {
    let Some(mcp) = mcp else {
        return;
    };
    push_section(lines, "MCP");
    if mcp.starting > 0 {
        lines.push(Line::from(vec![
            "  ● ".magenta(),
            format!("{} starting", mcp.starting).into(),
        ]));
    }
    if mcp.ready > 0 {
        lines.push(Line::from(vec![
            "  ● ".green(),
            format!("{} ready", mcp.ready).into(),
        ]));
    }
    if mcp.cancelled > 0 {
        lines.push(Line::from(vec![
            "  ● ".dim(),
            format!("{} cancelled", mcp.cancelled).into(),
        ]));
    }
    for failed in mcp.failed.iter().take(3) {
        lines.push(Line::from(vec!["  ● ".red(), failed.clone().red()]));
    }
}

fn push_context(
    lines: &mut Vec<Line<'static>>,
    context: Option<&ContextPanelSnapshot>,
    _width: u16,
) {
    let Some(context) = context else {
        return;
    };
    push_section(lines, "Context");
    lines.push(Line::from(vec![
        "  model ".dim(),
        context.model.clone().into(),
    ]));
    lines.push(Line::from(vec![
        "  identity ".dim(),
        context.identity.clone().into(),
    ]));
    if let Some(window) = context.context_window {
        let percent = if window > 0 {
            (context.used_tokens.saturating_mul(100) / window).clamp(0, 100)
        } else {
            0
        };
        let percent_span: Span<'static> = if percent >= 90 {
            format!("{percent}%").red()
        } else if percent >= 75 {
            format!("{percent}%").magenta()
        } else {
            format!("{percent}%").green()
        };
        lines.push(Line::from(vec!["  used ".dim(), percent_span]));
    } else {
        lines.push(Line::from(vec![
            "  tokens ".dim(),
            context.used_tokens.to_string().into(),
        ]));
    }
}

fn truncate(value: &str, width: u16) -> String {
    let max = width.saturating_sub(4) as usize;
    if value.chars().count() <= max {
        return value.to_string();
    }
    let take = max.saturating_sub(1);
    let mut out = value.chars().take(take).collect::<String>();
    out.push_str("...");
    out
}
