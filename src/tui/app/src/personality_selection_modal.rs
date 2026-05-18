use adam_protocol::config_types::Personality;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
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

#[derive(Debug, Clone)]
pub(crate) struct PersonalitySelectionModal {
    selected_idx: usize,
    current_personality: Personality,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PersonalitySelectionModalAction {
    None,
    Exit,
    Select { personality: Personality },
}

struct ModalRenderLines {
    header: Vec<Line<'static>>,
    item_groups: Vec<Vec<Line<'static>>>,
    footer: Vec<Line<'static>>,
}

impl PersonalitySelectionModal {
    pub(crate) fn new(current_personality: Personality) -> Self {
        let selected_idx = Self::personalities()
            .iter()
            .position(|personality| *personality == current_personality)
            .unwrap_or_default();
        Self {
            selected_idx,
            current_personality,
        }
    }

    pub(crate) fn handle_key_event(
        &mut self,
        key_event: KeyEvent,
    ) -> PersonalitySelectionModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return PersonalitySelectionModalAction::None;
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
                PersonalitySelectionModalAction::None
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
                PersonalitySelectionModalAction::None
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
                    && idx < Self::personalities().len()
                {
                    self.selected_idx = idx;
                    return self.selected_action();
                }
                PersonalitySelectionModalAction::None
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
            } => PersonalitySelectionModalAction::Exit,
            _ => PersonalitySelectionModalAction::None,
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
                "Select Personality".bold().into(),
                "Choose a communication style for Adam. Disable in /experimental."
                    .dim()
                    .into(),
                "".into(),
            ],
            item_groups: Self::personalities()
                .iter()
                .enumerate()
                .map(|(idx, personality)| self.item_lines(width, idx, *personality))
                .collect(),
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

    fn item_lines(&self, width: usize, idx: usize, personality: Personality) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let selected = self.selected_idx == idx;
        let marker = if selected { "›".cyan() } else { " ".into() };
        let number = format!("{}. ", idx + 1);
        let mut label = Self::personality_label(personality).to_string();
        if personality == self.current_personality {
            label.push_str(" (current)");
        }
        let label = if selected { label.bold() } else { label.into() };
        lines.push(vec![marker, " ".into(), number.into(), label].into());

        let wrapped = wrap(
            Self::personality_description(personality),
            Options::new(width)
                .initial_indent("   ")
                .subsequent_indent("   "),
        );
        for line in wrapped {
            lines.push(line.into_owned().dim().into());
        }

        lines
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

    fn selected_action(&self) -> PersonalitySelectionModalAction {
        let Some(personality) = Self::personalities().get(self.selected_idx).copied() else {
            return PersonalitySelectionModalAction::None;
        };
        PersonalitySelectionModalAction::Select { personality }
    }

    fn move_up(&mut self) {
        if self.selected_idx == 0 {
            self.selected_idx = Self::personalities().len().saturating_sub(1);
        } else {
            self.selected_idx -= 1;
        }
    }

    fn move_down(&mut self) {
        self.selected_idx = (self.selected_idx + 1) % Self::personalities().len();
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

    fn content_height(&self, width: usize) -> usize {
        let lines = self.render_lines(width);
        lines
            .header
            .len()
            .saturating_add(lines.item_groups.iter().map(Vec::len).sum::<usize>())
            .saturating_add(lines.footer.len())
    }

    fn personalities() -> [Personality; 2] {
        [Personality::Friendly, Personality::Pragmatic]
    }

    fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    fn personality_description(personality: Personality) -> &'static str {
        match personality {
            Personality::Friendly => "Warm, collaborative, and helpful.",
            Personality::Pragmatic => "Concise, task-focused, and direct.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_backend::VT100Backend;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;

    #[test]
    fn renders_centered_personality_modal() {
        let modal = PersonalitySelectionModal::new(Personality::Friendly);

        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Select Personality"));
        assert!(rendered.contains("Friendly (current)"));
        assert!(rendered.contains("Pragmatic"));
        assert!(rendered.contains("Enter"));
        assert!(rendered.contains("Esc"));
    }

    #[test]
    fn enter_selects_current_highlighted_personality() {
        let mut modal = PersonalitySelectionModal::new(Personality::Friendly);

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PersonalitySelectionModalAction::Select {
                personality: Personality::Friendly,
            }
        );
    }

    #[test]
    fn digit_selects_matching_personality() {
        let mut modal = PersonalitySelectionModal::new(Personality::Friendly);

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)),
            PersonalitySelectionModalAction::Select {
                personality: Personality::Pragmatic,
            }
        );

        let mut modal = PersonalitySelectionModal::new(Personality::Pragmatic);

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)),
            PersonalitySelectionModalAction::Select {
                personality: Personality::Friendly,
            }
        );
    }

    #[test]
    fn invalid_digit_shortcut_does_not_select_personality() {
        let mut modal = PersonalitySelectionModal::new(Personality::Friendly);

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE)),
            PersonalitySelectionModalAction::None
        );

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)),
            PersonalitySelectionModalAction::None
        );
    }

    #[test]
    fn up_down_wrap_selection() {
        let mut modal = PersonalitySelectionModal::new(Personality::Friendly);

        modal.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PersonalitySelectionModalAction::Select {
                personality: Personality::Pragmatic,
            }
        );

        modal.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PersonalitySelectionModalAction::Select {
                personality: Personality::Friendly,
            }
        );
    }

    #[test]
    fn escape_exits_without_selection() {
        let mut modal = PersonalitySelectionModal::new(Personality::Friendly);

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            PersonalitySelectionModalAction::Exit
        );
    }
}
