use adam_agent::protocol::AgentStatus;
use adam_protocol::ThreadId;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use textwrap::Options;
use textwrap::wrap;

use crate::render::Insets;
use crate::render::RectExt as _;
use crate::text_formatting::truncate_text;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

const AGENT_ERROR_PREVIEW_GRAPHEMES: usize = 160;
const AGENT_RESPONSE_PREVIEW_GRAPHEMES: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSelectionModalItem {
    pub(crate) thread_id: ThreadId,
    pub(crate) name: String,
    pub(crate) status: AgentStatus,
    pub(crate) is_current: bool,
    pub(crate) is_closed: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentSelectionModal {
    items: Vec<AgentSelectionModalItem>,
    selected_idx: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentSelectionModalAction {
    None,
    Exit,
    SelectThread(ThreadId),
}

struct ModalRenderLines {
    header: Vec<Line<'static>>,
    item_groups: Vec<Vec<Line<'static>>>,
    footer: Vec<Line<'static>>,
}

impl AgentSelectionModal {
    pub(crate) fn new(
        items: Vec<AgentSelectionModalItem>,
        initial_selected_idx: Option<usize>,
    ) -> Option<Self> {
        if items.is_empty() {
            return None;
        }

        let selected_idx = initial_selected_idx
            .filter(|idx| *idx < items.len())
            .or_else(|| items.iter().position(|item| item.is_current))
            .unwrap_or_default();
        Some(Self {
            items,
            selected_idx,
        })
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> AgentSelectionModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return AgentSelectionModalAction::None;
        }

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{0010}'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_up();
                AgentSelectionModalAction::None
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{000e}'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_down();
                AgentSelectionModalAction::None
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                ..
            } if c.is_ascii_digit() => {
                if let Some(idx) = c
                    .to_digit(10)
                    .map(|digit| digit as usize)
                    .and_then(|digit| digit.checked_sub(1))
                    && idx < self.items.len()
                {
                    self.selected_idx = idx;
                    return self.selected_action();
                }
                AgentSelectionModalAction::None
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.selected_action(),
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => AgentSelectionModalAction::Exit,
            _ => AgentSelectionModalAction::None,
        }
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let modal_area = self.modal_area(area);
        Clear.render(modal_area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().dim());
        let inner_area = block.inner(modal_area);
        block.render(modal_area, buf);

        let content_area = inner_area.inset(Insets::vh(1, 2));
        if content_area.is_empty() {
            return;
        }

        let width = content_area.width.max(1) as usize;
        let lines = self.render_lines(width);
        let footer_height = (lines.footer.len() as u16).min(content_area.height);
        let header_height =
            (lines.header.len() as u16).min(content_area.height.saturating_sub(footer_height));
        let list_height = content_area
            .height
            .saturating_sub(header_height)
            .saturating_sub(footer_height);

        if header_height > 0 {
            Paragraph::new(
                lines
                    .header
                    .iter()
                    .take(header_height as usize)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .render(
                Rect {
                    height: header_height,
                    ..content_area
                },
                buf,
            );
        }

        if list_height > 0 {
            let scroll_top = Self::list_scroll_top_for_selected_item(
                &lines.item_groups,
                self.selected_idx,
                list_height as usize,
            );
            let visible_lines = lines
                .item_groups
                .iter()
                .flat_map(|group| group.iter().cloned())
                .skip(scroll_top)
                .take(list_height as usize)
                .collect::<Vec<_>>();
            Paragraph::new(visible_lines).render(
                Rect {
                    y: content_area.y.saturating_add(header_height),
                    height: list_height,
                    ..content_area
                },
                buf,
            );
        }

        if footer_height > 0 {
            let footer_start = lines.footer.len().saturating_sub(footer_height as usize);
            Paragraph::new(
                lines
                    .footer
                    .iter()
                    .skip(footer_start)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .render(
                Rect {
                    y: content_area
                        .y
                        .saturating_add(content_area.height.saturating_sub(footer_height)),
                    height: footer_height,
                    ..content_area
                },
                buf,
            );
        }
    }

    fn render_lines(&self, width: usize) -> ModalRenderLines {
        ModalRenderLines {
            header: vec![
                "Multi-agents".bold().into(),
                "Select an agent to watch.".dim().into(),
                "".into(),
            ],
            item_groups: self.item_groups(width),
            footer: vec![
                "".into(),
                vec![
                    "Enter".cyan(),
                    " select   ".dim(),
                    "↑↓/jk".cyan(),
                    " move   ".dim(),
                    "Esc".cyan(),
                    " exit".dim(),
                ]
                .into(),
            ],
        }
    }

    fn item_groups(&self, width: usize) -> Vec<Vec<Line<'static>>> {
        self.items
            .iter()
            .enumerate()
            .map(|(idx, item)| self.item_lines(width, idx, item))
            .collect()
    }

    fn item_lines(
        &self,
        width: usize,
        idx: usize,
        item: &AgentSelectionModalItem,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let selected = self.selected_idx == idx;
        let marker = if selected { "›".cyan() } else { " ".into() };
        let number = format!("{}. ", idx + 1);
        let state_marker = if item.is_closed { "○ " } else { "● " };
        let label = if item.is_current {
            format!("{state_marker}{} (current)", item.name)
        } else {
            format!("{state_marker}{}", item.name)
        };
        let label = if selected { label.bold() } else { label.into() };
        lines.push(vec![marker, " ".into(), number.into(), label].into());

        let thread_id = item.thread_id.to_string();
        for line in wrap(
            thread_id.as_str(),
            Options::new(width)
                .initial_indent("   ")
                .subsequent_indent("   "),
        ) {
            lines.push(line.into_owned().dim().into());
        }

        let status = Self::status_summary_line(&item.status);
        lines.extend(Self::wrap_status_line(status, width));
        lines
    }

    fn wrap_status_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
        word_wrap_lines(
            [line],
            RtOptions::new(width)
                .initial_indent(vec!["   ".dim()].into())
                .subsequent_indent(vec!["   ".dim()].into()),
        )
    }

    fn status_summary_line(status: &AgentStatus) -> Line<'static> {
        Self::status_summary_spans(status).into()
    }

    fn status_summary_spans(status: &AgentStatus) -> Vec<Span<'static>> {
        match status {
            AgentStatus::PendingInit => vec![Span::from("Pending init").cyan()],
            AgentStatus::Running => vec![Span::from("Running").cyan().bold()],
            #[allow(clippy::disallowed_methods)]
            AgentStatus::Interrupted => vec![Span::from("Interrupted").yellow()],
            AgentStatus::Completed(message) => {
                let mut spans = vec![Span::from("Completed").green()];
                if let Some(message) = message.as_ref() {
                    let message_preview = truncate_text(
                        &message.split_whitespace().collect::<Vec<_>>().join(" "),
                        AGENT_RESPONSE_PREVIEW_GRAPHEMES,
                    );
                    if !message_preview.is_empty() {
                        spans.push(" - ".dim());
                        spans.push(Span::from(message_preview));
                    }
                }
                spans
            }
            AgentStatus::Errored(error) => {
                let mut spans = vec![Span::from("Error").red()];
                let error_preview = truncate_text(
                    &error.split_whitespace().collect::<Vec<_>>().join(" "),
                    AGENT_ERROR_PREVIEW_GRAPHEMES,
                );
                if !error_preview.is_empty() {
                    spans.push(" - ".dim());
                    spans.push(Span::from(error_preview));
                }
                spans
            }
            AgentStatus::Shutdown => vec![Span::from("Shutdown")],
            AgentStatus::NotFound => vec![Span::from("Not found").red()],
        }
    }

    fn content_height(&self, width: usize) -> usize {
        let lines = self.render_lines(width);
        lines
            .header
            .len()
            .saturating_add(lines.item_groups.iter().map(Vec::len).sum::<usize>())
            .saturating_add(lines.footer.len())
    }

    fn list_scroll_top_for_selected_item(
        item_groups: &[Vec<Line<'static>>],
        selected_idx: usize,
        visible_height: usize,
    ) -> usize {
        if item_groups.is_empty() || visible_height == 0 {
            return 0;
        }

        let selected_idx = selected_idx.min(item_groups.len().saturating_sub(1));
        let selected_top = item_groups
            .iter()
            .take(selected_idx)
            .map(Vec::len)
            .sum::<usize>();
        let selected_height = item_groups[selected_idx].len().max(1);
        let selected_bottom = selected_top.saturating_add(selected_height);
        let total_lines = item_groups.iter().map(Vec::len).sum::<usize>();
        let max_scroll = total_lines.saturating_sub(visible_height);

        if selected_height >= visible_height {
            return selected_top.min(max_scroll);
        }
        if selected_bottom <= visible_height {
            0
        } else {
            selected_bottom
                .saturating_sub(visible_height)
                .min(max_scroll)
        }
    }

    fn selected_action(&self) -> AgentSelectionModalAction {
        self.items
            .get(self.selected_idx)
            .map(|item| AgentSelectionModalAction::SelectThread(item.thread_id))
            .unwrap_or(AgentSelectionModalAction::None)
    }

    fn move_up(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected_idx == 0 {
            self.selected_idx = self.items.len().saturating_sub(1);
        } else {
            self.selected_idx -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected_idx = (self.selected_idx + 1) % self.items.len();
    }

    fn modal_area(&self, area: Rect) -> Rect {
        let width = area.width.saturating_sub(4).min(88).max(area.width.min(44));
        let content_width = width.saturating_sub(6).max(1) as usize;
        let desired_content_height = self.content_height(content_width);
        let desired_height = desired_content_height
            .saturating_add(4)
            .try_into()
            .unwrap_or(u16::MAX);
        let height = area
            .height
            .saturating_sub(2)
            .min(desired_height)
            .max(area.height.min(10));
        Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn thread_id(suffix: u128) -> ThreadId {
        ThreadId::from_string(&format!("00000000-0000-0000-0000-{suffix:012}"))
            .expect("valid thread id")
    }

    fn item(
        suffix: u128,
        name: &str,
        status: AgentStatus,
        is_current: bool,
        is_closed: bool,
    ) -> AgentSelectionModalItem {
        AgentSelectionModalItem {
            thread_id: thread_id(suffix),
            name: name.to_string(),
            status,
            is_current,
            is_closed,
        }
    }

    fn modal() -> AgentSelectionModal {
        AgentSelectionModal::new(
            vec![
                item(1, "Main [default]", AgentStatus::Running, true, false),
                item(
                    2,
                    "Robie [explorer]",
                    AgentStatus::Completed(Some("mapped the branch".to_string())),
                    false,
                    false,
                ),
                item(3, "Closed [worker]", AgentStatus::Shutdown, false, true),
            ],
            Some(1),
        )
        .expect("modal")
    }

    #[test]
    fn renders_centered_agent_selection_modal() {
        let modal = modal();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        assert_snapshot!("agent_selection_modal", terminal.backend());
    }

    #[test]
    fn wraps_status_line_once_and_preserves_status_style() {
        let line = AgentSelectionModal::status_summary_line(&AgentStatus::Completed(Some(
            "mapped the full repository structure and summarized every important subsystem"
                .to_string(),
        )));
        let lines = AgentSelectionModal::wrap_status_line(line, 24);

        assert!(lines.len() > 1);
        for line in &lines {
            let text = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(text.starts_with("   "));
            assert!(!text.starts_with("      "));
            assert!(line.width() <= 24);
        }

        assert_eq!(lines[0].spans[1].content.as_ref(), "Completed");
        assert_eq!(lines[0].spans[1].style.fg, Some(Color::Green));
    }

    #[test]
    fn enter_selects_current_item() {
        let mut modal = modal();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            AgentSelectionModalAction::SelectThread(thread_id(2))
        );
    }

    #[test]
    fn arrow_and_jk_navigation_changes_selection() {
        let mut modal = modal();

        modal.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            AgentSelectionModalAction::SelectThread(thread_id(3))
        );

        modal.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            AgentSelectionModalAction::SelectThread(thread_id(2))
        );
    }

    #[test]
    fn digit_selects_matching_item() {
        let mut modal = modal();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)),
            AgentSelectionModalAction::SelectThread(thread_id(3))
        );
    }

    #[test]
    fn escape_exits_modal() {
        let mut modal = modal();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            AgentSelectionModalAction::Exit
        );
    }
}
