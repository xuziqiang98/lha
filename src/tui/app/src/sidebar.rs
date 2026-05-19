use crate::buddy;
use crate::buddy::state::BuddyState;
use crate::status::format_tokens_compact;
use adam_agent::protocol::AgentStatus;
use adam_protocol::ThreadId;
use adam_protocol::plan_tool::StepStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Styled;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use std::path::PathBuf;
use unicode_width::UnicodeWidthChar;

pub(crate) const SIDEBAR_MIN_TERMINAL_WIDTH: u16 = 120;
pub(crate) const SIDEBAR_MIN_WIDTH: u16 = 28;
pub(crate) const SIDEBAR_MAX_WIDTH: u16 = 42;
pub(crate) const SIDEBAR_PERCENT: u16 = 28;
pub(crate) const MAIN_MIN_WIDTH: u16 = 72;
pub(crate) const SIDEBAR_VISIBLE_FILES_LIMIT: usize = 6;
const SIDEBAR_BUDDY_CONTENT_RESERVE: u16 = 2;

#[derive(Clone, Debug, Default)]
pub(crate) struct SidebarSnapshot {
    pub(crate) task: Option<TaskPanelSnapshot>,
    pub(crate) todo: Option<TodoPanelSnapshot>,
    pub(crate) files: Vec<String>,
    pub(crate) files_more_count: usize,
    pub(crate) agents: Vec<AgentPanelEntry>,
    pub(crate) skills: Vec<SkillPanelEntry>,
    pub(crate) mcp: Option<McpPanelSnapshot>,
    pub(crate) status: Option<StatusPanelSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskPanelSnapshot {
    pub(crate) title: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TodoPanelSnapshot {
    pub(crate) items: Vec<TodoPanelItem>,
}

#[derive(Clone, Debug)]
pub(crate) struct TodoPanelItem {
    pub(crate) step: String,
    pub(crate) status: StepStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentPanelEntry {
    pub(crate) thread_id: ThreadId,
    pub(crate) label: String,
    pub(crate) status: AgentStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SkillPanelEntry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpPanelSnapshot {
    pub(crate) starting: usize,
    pub(crate) ready: usize,
    pub(crate) failed: Vec<String>,
    pub(crate) cancelled: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StatusPanelSnapshot {
    pub(crate) model: String,
    pub(crate) identity: String,
    pub(crate) left_context_tokens: Option<i64>,
    pub(crate) total_usage_tokens: i64,
    pub(crate) cache_hit_percent: Option<i64>,
    pub(crate) context_compact_count: usize,
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
        push_todo(&mut lines, self.snapshot.todo.as_ref(), area.width);
        push_files(
            &mut lines,
            &self.snapshot.files,
            self.snapshot.files_more_count,
            area.width,
        );
        push_agents(&mut lines, &self.snapshot.agents, area.width);
        push_skills(&mut lines, &self.snapshot.skills, area.width);
        push_mcp(&mut lines, self.snapshot.mcp.as_ref(), area.width);
        push_status(&mut lines, self.snapshot.status.as_ref(), area.width);

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
    lines.extend(wrap_task_title(&task.title, width));
}

fn push_todo(lines: &mut Vec<Line<'static>>, todo: Option<&TodoPanelSnapshot>, width: u16) {
    let Some(todo) = todo else {
        return;
    };
    if todo.items.is_empty() {
        return;
    }

    push_section(lines, "Todo");
    for item in todo.items.iter().take(8) {
        let style = match item.status {
            StepStatus::Completed => Style::default().dim().crossed_out(),
            StepStatus::InProgress => Style::default().cyan().bold(),
            StepStatus::Pending => Style::default().dim(),
        };
        lines.push(Line::from(vec![
            "  ".into(),
            truncate(&item.step, width).set_style(style),
        ]));
    }
    if todo.items.len() > 8 {
        lines.push(format!("  +{} more", todo.items.len() - 8).dim().into());
    }
}

fn push_files(lines: &mut Vec<Line<'static>>, files: &[String], more_count: usize, width: u16) {
    if files.is_empty() {
        return;
    }
    push_section(lines, "Files");
    for file in files.iter().take(SIDEBAR_VISIBLE_FILES_LIMIT) {
        lines.push(Line::from(vec![
            "  ".into(),
            truncate(file, width).magenta(),
        ]));
    }
    if more_count > 0 {
        lines.push(format!("  +{more_count} more").dim().into());
    }
}

fn push_agents(lines: &mut Vec<Line<'static>>, agents: &[AgentPanelEntry], width: u16) {
    if agents.is_empty() {
        return;
    }

    push_section(lines, "Agents");
    for agent in agents.iter().take(6) {
        let _thread_id = agent.thread_id;
        let status = agent_status_label(&agent.status);
        lines.push(Line::from(vec![
            "  ".into(),
            truncate(&agent.label, width).cyan(),
            " ".dim(),
            status,
        ]));
    }
    if agents.len() > 6 {
        lines.push(format!("  +{} more", agents.len() - 6).dim().into());
    }
}

fn push_skills(lines: &mut Vec<Line<'static>>, skills: &[SkillPanelEntry], width: u16) {
    if skills.is_empty() {
        return;
    }

    push_section(lines, "Skills");
    for skill in skills.iter().take(6) {
        let _path = &skill.path;
        lines.push(Line::from(vec![
            "  ".into(),
            truncate(&skill.name, width).magenta(),
        ]));
    }
    if skills.len() > 6 {
        lines.push(format!("  +{} more", skills.len() - 6).dim().into());
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

fn push_status(lines: &mut Vec<Line<'static>>, status: Option<&StatusPanelSnapshot>, _width: u16) {
    let Some(context) = status else {
        return;
    };
    push_section(lines, "Status");
    lines.push(Line::from(vec![
        "  model ".dim(),
        context.model.clone().into(),
    ]));
    lines.push(Line::from(vec![
        "  identity ".dim(),
        context.identity.clone().into(),
    ]));
    if let Some(left_context_tokens) = context.left_context_tokens {
        lines.push(Line::from(vec![
            "  left ".dim(),
            format_tokens_compact(left_context_tokens).into(),
        ]));
    }
    lines.push(Line::from(vec![
        "  total ".dim(),
        format_tokens_compact(context.total_usage_tokens).into(),
    ]));
    if let Some(cache_hit_percent) = context.cache_hit_percent {
        lines.push(Line::from(vec![
            "  cached ".dim(),
            format!("{cache_hit_percent}%").into(),
        ]));
    }
    if context.context_compact_count > 0 {
        lines.push(Line::from(vec![
            "  compact ".dim(),
            context.context_compact_count.to_string().into(),
        ]));
    }
}

fn agent_status_label(status: &AgentStatus) -> Span<'static> {
    match status {
        AgentStatus::PendingInit => "pending".dim(),
        AgentStatus::Running => "running".dim(),
        AgentStatus::Interrupted => "interrupted".dim(),
        AgentStatus::Completed(_) => "completed".dim(),
        AgentStatus::Errored(_) => "error".red(),
        AgentStatus::Shutdown => "shutdown".dim(),
        AgentStatus::NotFound => "missing".red(),
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

fn wrap_task_title(title: &str, width: u16) -> Vec<Line<'static>> {
    let max = width.saturating_sub(4) as usize;
    if max == 0 {
        return vec![Line::from("  ")];
    }

    let wrapped = textwrap::wrap(title, max);
    let mut title_lines = if wrapped.is_empty() {
        vec![String::new()]
    } else {
        wrapped
            .iter()
            .take(2)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };

    if wrapped.len() > 2
        && let Some(second) = title_lines.get_mut(1)
    {
        *second = append_ellipsis(second, max);
    }

    title_lines
        .into_iter()
        .map(|line| Line::from(vec!["  ".into(), line.cyan().bold()]))
        .collect()
}

fn append_ellipsis(value: &str, max: usize) -> String {
    if max <= 3 {
        return ".".repeat(max);
    }

    let mut out = String::new();
    let mut width = 0usize;
    for ch in value.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max.saturating_sub(3) {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn render_sidebar(snapshot: &SidebarSnapshot) -> String {
        let area = Rect::new(0, 0, 42, 30);
        let mut buf = Buffer::empty(area);
        SidebarWidget {
            snapshot,
            buddy_state: None,
            animations_enabled: false,
        }
        .render(area, &mut buf);

        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_sections_in_sidebar_order() {
        let snapshot = SidebarSnapshot {
            task: Some(TaskPanelSnapshot {
                title: "Build Sidebar".to_string(),
            }),
            todo: Some(TodoPanelSnapshot {
                items: vec![
                    TodoPanelItem {
                        step: "Explore codebase".to_string(),
                        status: StepStatus::Completed,
                    },
                    TodoPanelItem {
                        step: "Implement feature".to_string(),
                        status: StepStatus::InProgress,
                    },
                    TodoPanelItem {
                        step: "Write tests".to_string(),
                        status: StepStatus::Pending,
                    },
                ],
            }),
            files: vec!["src/tui/app/src/sidebar.rs".to_string()],
            files_more_count: 0,
            agents: vec![AgentPanelEntry {
                thread_id: ThreadId::new(),
                label: "Worker (worker)".to_string(),
                status: AgentStatus::Running,
            }],
            skills: vec![SkillPanelEntry {
                name: "skill-creator".to_string(),
                path: PathBuf::from("/tmp/skill-creator/SKILL.md"),
            }],
            mcp: Some(McpPanelSnapshot {
                starting: 1,
                ready: 2,
                failed: vec!["broken-server".to_string()],
                cancelled: 1,
            }),
            status: Some(StatusPanelSnapshot {
                model: "gpt-5".to_string(),
                identity: "Planner".to_string(),
                left_context_tokens: Some(55),
                total_usage_tokens: 45,
                cache_hit_percent: Some(25),
                context_compact_count: 1,
            }),
        };

        let rendered = render_sidebar(&snapshot);
        let sections = ["Task", "Todo", "Files", "Agents", "Skills", "MCP", "Status"];
        let positions = sections
            .iter()
            .map(|section| {
                rendered
                    .find(section)
                    .unwrap_or_else(|| panic!("missing section {section}: {rendered:?}"))
            })
            .collect::<Vec<_>>();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted);
        assert!(rendered.contains("Build Sidebar"));
        assert!(rendered.contains("Explore codebase"));
        assert!(rendered.contains("Implement feature"));
        assert!(rendered.contains("Write tests"));
        assert!(!rendered.contains("[x]"));
        assert!(!rendered.contains("[~]"));
        assert!(!rendered.contains("[ ]"));
        assert!(rendered.contains("Worker (worker) running"));
        let agent_line = rendered
            .lines()
            .find(|line| line.contains("Worker (worker) running"))
            .unwrap_or_else(|| panic!("missing agent line: {rendered:?}"));
        assert!(!agent_line.contains("●"));
        assert!(rendered.contains("skill-creator"));
        assert!(rendered.contains("model gpt-5"));
        assert!(rendered.contains("left 55"));
        assert!(rendered.contains("total 45"));
        assert!(rendered.contains("cached 25%"));
        assert!(rendered.contains("compact 1"));
        assert!(!rendered.contains("used"));
        assert!(!rendered.contains("tokens"));
        assert!(!rendered.contains("Context"));
    }

    #[test]
    fn status_renders_left_and_total_when_context_window_known() {
        let snapshot = SidebarSnapshot {
            status: Some(StatusPanelSnapshot {
                model: "gpt-5".to_string(),
                identity: "Planner".to_string(),
                left_context_tokens: Some(19_800),
                total_usage_tokens: 12_345,
                cache_hit_percent: Some(25),
                context_compact_count: 2,
            }),
            ..Default::default()
        };

        let rendered = render_sidebar(&snapshot);

        assert!(rendered.contains("left 19.8K"));
        assert!(rendered.contains("total 12.3K"));
        assert!(rendered.contains("cached 25%"));
        assert!(rendered.contains("compact 2"));
        assert!(!rendered.contains("5K"));
    }

    #[test]
    fn status_omits_left_and_cached_when_context_window_and_cache_unknown() {
        let snapshot = SidebarSnapshot {
            status: Some(StatusPanelSnapshot {
                model: "gpt-5".to_string(),
                identity: "Planner".to_string(),
                left_context_tokens: None,
                total_usage_tokens: 12_345,
                cache_hit_percent: None,
                context_compact_count: 0,
            }),
            ..Default::default()
        };

        let rendered = render_sidebar(&snapshot);

        assert!(!rendered.contains("left"));
        assert!(!rendered.contains("cached"));
        assert!(!rendered.contains("compact"));
        assert!(rendered.contains("total 12.3K"));
    }

    #[test]
    fn hides_empty_sections() {
        let rendered = render_sidebar(&SidebarSnapshot::default());
        for section in ["Task", "Todo", "Files", "Agents", "Skills", "MCP"] {
            assert!(!rendered.contains(section));
        }
        assert!(!rendered.contains("Status"));
    }

    #[test]
    fn task_title_wraps_to_two_lines_with_ellipsis() {
        let snapshot = SidebarSnapshot {
            task: Some(TaskPanelSnapshot {
                title:
                    "Repair sidebar task rendering after streamed proposed plan headings overflow"
                        .to_string(),
            }),
            ..Default::default()
        };

        let rendered = render_sidebar(&snapshot);
        assert!(rendered.contains("Repair sidebar task rendering after"));
        assert!(rendered.contains("streamed proposed plan headings..."));
        assert!(!rendered.contains("overflow"));
    }

    #[test]
    fn task_title_wraps_to_two_lines_without_ellipsis_when_it_fits() {
        let snapshot = SidebarSnapshot {
            task: Some(TaskPanelSnapshot {
                title: "Repair sidebar task rendering after streamed plan".to_string(),
            }),
            ..Default::default()
        };

        let rendered = render_sidebar(&snapshot);
        assert!(rendered.contains("Repair sidebar task rendering after"));
        assert!(rendered.contains("streamed plan"));
        assert!(!rendered.contains("..."));
    }

    #[test]
    fn files_and_skills_show_more_counts() {
        let snapshot = SidebarSnapshot {
            files: (0..SIDEBAR_VISIBLE_FILES_LIMIT)
                .map(|i| format!("file-{i}.rs"))
                .collect(),
            files_more_count: 2,
            skills: (0..8)
                .map(|i| SkillPanelEntry {
                    name: format!("skill-{i}"),
                    path: PathBuf::from(format!("/tmp/skill-{i}/SKILL.md")),
                })
                .collect(),
            ..Default::default()
        };

        let rendered = render_sidebar(&snapshot);
        assert!(rendered.contains("file-5.rs"));
        assert!(!rendered.contains("file-6.rs"));
        assert!(rendered.contains("skill-5"));
        assert!(!rendered.contains("skill-6"));
        assert_eq!(rendered.matches("+2 more").count(), 2);
    }
}
