use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use lha_protocol::config_types::IdentityKind;
use lha_protocol::config_types::IdentityMask;
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
pub(crate) struct IdentityModal {
    items: Vec<IdentityMask>,
    selected_idx: usize,
    current_kind: Option<IdentityKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IdentityModalAction {
    None,
    Selected(IdentityMask),
    Exit,
}

impl IdentityModal {
    pub(crate) fn new(
        items: Vec<IdentityMask>,
        current_kind: Option<IdentityKind>,
    ) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        let selected_idx = current_kind
            .and_then(|kind| items.iter().position(|item| item.kind == Some(kind)))
            .unwrap_or(0);
        Some(Self {
            items,
            selected_idx,
            current_kind,
        })
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> IdentityModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return IdentityModalAction::None;
        }

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_up();
                IdentityModalAction::None
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_down();
                IdentityModalAction::None
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
                IdentityModalAction::None
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
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => IdentityModalAction::Exit,
            _ => IdentityModalAction::None,
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

        let content_area = inner_area.inset(Insets::vh(1, 1));
        if content_area.is_empty() {
            return;
        }

        let mut lines: Vec<Line<'static>> = vec![
            "Select Identity".bold().into(),
            "Pick how LHA should behave for future turns.".dim().into(),
            "".into(),
        ];
        let content_width = content_area.width.max(1) as usize;
        for (idx, item) in self.items.iter().enumerate() {
            self.push_item_lines(&mut lines, content_width, idx, item);
        }
        lines.push("".into());
        lines.push(
            vec![
                "Enter".cyan(),
                " select   ".dim(),
                "↑↓/jk".cyan(),
                " move   ".dim(),
                "Esc".cyan(),
                " exit".dim(),
            ]
            .into(),
        );

        Paragraph::new(lines).render(content_area, buf);
    }

    fn push_item_lines(
        &self,
        lines: &mut Vec<Line<'static>>,
        width: usize,
        idx: usize,
        item: &IdentityMask,
    ) {
        let selected = self.selected_idx == idx;
        let current = self.current_kind == item.kind;
        let marker = if selected { "›".cyan() } else { " ".into() };
        let number = format!("{}. ", idx + 1);
        let label = if current {
            format!("{} (current)", item.name)
        } else {
            item.name.clone()
        };
        let label = if selected { label.bold() } else { label.into() };
        lines.push(vec![marker, " ".into(), number.into(), label].into());

        let description = identity_description(item.kind);
        let wrapped = wrap(
            description,
            Options::new(width)
                .initial_indent("   ")
                .subsequent_indent("   "),
        );
        for line in wrapped {
            lines.push(line.into_owned().dim().into());
        }
    }

    fn selected_action(&self) -> IdentityModalAction {
        self.items
            .get(self.selected_idx)
            .cloned()
            .map(IdentityModalAction::Selected)
            .unwrap_or(IdentityModalAction::None)
    }

    fn move_up(&mut self) {
        if self.selected_idx == 0 {
            self.selected_idx = self.items.len().saturating_sub(1);
        } else {
            self.selected_idx -= 1;
        }
    }

    fn move_down(&mut self) {
        self.selected_idx = (self.selected_idx + 1) % self.items.len();
    }

    fn modal_area(&self, area: Rect) -> Rect {
        let width = area.width.saturating_sub(4).min(76).max(area.width.min(44));
        let content_lines = self.items.len().saturating_mul(2).saturating_add(5);
        let desired_height = content_lines
            .saturating_add(4)
            .try_into()
            .unwrap_or(u16::MAX);
        let height = area
            .height
            .saturating_sub(2)
            .min(desired_height)
            .max(area.height.min(8));
        Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        }
    }
}

fn identity_description(kind: Option<IdentityKind>) -> &'static str {
    match kind {
        Some(IdentityKind::Nobody) => "Default assistant identity.",
        Some(IdentityKind::Planner) => "Plans work and asks before implementation.",
        Some(IdentityKind::Programmer) => "Implements code changes directly.",
        Some(IdentityKind::Explorer) => "Explores code in a read-only isolated job.",
        Some(IdentityKind::Reviewer) => "Reviews code in a read-only isolated job.",
        None => "Custom assistant identity.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use ratatui::Terminal;

    use crate::test_backend::VT100Backend;

    fn mask(kind: IdentityKind, name: &str) -> IdentityMask {
        IdentityMask {
            kind: Some(kind),
            name: name.to_string(),
            model: None,
            reasoning_effort: None,
            developer_instructions: None,
            capabilities: lha_protocol::config_types::IdentityCapabilities { write_tools: false },
        }
    }

    #[test]
    fn enter_selects_current_item() {
        let mut modal = IdentityModal::new(
            vec![
                mask(IdentityKind::Nobody, "Nobody"),
                mask(IdentityKind::Planner, "Planner"),
            ],
            Some(IdentityKind::Planner),
        )
        .expect("modal");

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            IdentityModalAction::Selected(mask(IdentityKind::Planner, "Planner"))
        );
    }

    #[test]
    fn renders_centered_identity_modal() {
        let modal = IdentityModal::new(
            vec![
                mask(IdentityKind::Nobody, "Nobody"),
                mask(IdentityKind::Planner, "Planner"),
                mask(IdentityKind::Programmer, "Programmer"),
                mask(IdentityKind::Explorer, "Explorer"),
                mask(IdentityKind::Reviewer, "Reviewer"),
            ],
            Some(IdentityKind::Nobody),
        )
        .expect("modal");

        let mut terminal = Terminal::new(VT100Backend::new(100, 30)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Select Identity"));
        assert!(rendered.contains("Nobody (current)"));
        assert!(rendered.contains("Planner"));
        assert!(rendered.contains("Programmer"));
        assert!(rendered.contains("Enter"));
    }
}
