use adam_agent::features::Feature;
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
use std::collections::BTreeMap;
use textwrap::Options;
use textwrap::wrap;

use crate::render::Insets;
use crate::render::RectExt as _;

pub(crate) struct ExperimentalFeatureItem {
    pub(crate) feature: Feature,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) enabled: bool,
}

pub(crate) struct ExperimentalFeaturesModal {
    features: Vec<ExperimentalFeatureItem>,
    initial_feature_states: BTreeMap<Feature, bool>,
    selected_idx: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExperimentalFeaturesModalAction {
    None,
    SaveAndClose { updates: Vec<(Feature, bool)> },
}

struct ModalRenderLines {
    header: Vec<Line<'static>>,
    item_groups: Vec<Vec<Line<'static>>>,
    footer: Vec<Line<'static>>,
}

impl ExperimentalFeaturesModal {
    pub(crate) fn new(features: Vec<ExperimentalFeatureItem>) -> Self {
        let initial_feature_states = features
            .iter()
            .map(|item| (item.feature, item.enabled))
            .collect();
        let selected_idx = (!features.is_empty()).then_some(0);
        Self {
            features,
            initial_feature_states,
            selected_idx,
        }
    }

    pub(crate) fn handle_key_event(
        &mut self,
        key_event: KeyEvent,
    ) -> ExperimentalFeaturesModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ExperimentalFeaturesModalAction::None;
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
                ExperimentalFeaturesModalAction::None
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
                ExperimentalFeaturesModalAction::None
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.toggle_selected();
                ExperimentalFeaturesModalAction::None
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => ExperimentalFeaturesModalAction::SaveAndClose {
                updates: self.updates(),
            },
            _ => ExperimentalFeaturesModalAction::None,
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
                "Experimental features".bold().into(),
                "Toggle experimental features. Changes are saved to config.toml."
                    .dim()
                    .into(),
                "".into(),
            ],
            item_groups: self.item_groups(width),
            footer: vec![
                "".into(),
                vec![
                    "Enter".cyan(),
                    " toggle   ".dim(),
                    "↑↓/jk".cyan(),
                    " move   ".dim(),
                    "Esc".cyan(),
                    " save".dim(),
                ]
                .into(),
            ],
        }
    }

    fn item_groups(&self, width: usize) -> Vec<Vec<Line<'static>>> {
        if self.features.is_empty() {
            return vec![vec![
                "  No experimental features available for now".dim().into(),
            ]];
        }

        self.features
            .iter()
            .enumerate()
            .map(|(idx, item)| self.item_lines(width, idx, item))
            .collect()
    }

    fn item_lines(
        &self,
        width: usize,
        idx: usize,
        item: &ExperimentalFeatureItem,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let selected = self.selected_idx == Some(idx);
        let marker = if selected { "›".cyan() } else { " ".into() };
        let enabled = if item.enabled { "x" } else { " " };
        let label = format!("[{enabled}] {}", item.name);
        let label = if selected { label.bold() } else { label.into() };
        lines.push(vec![marker, " ".into(), label].into());

        if !item.description.is_empty() {
            let wrapped = wrap(
                item.description.as_str(),
                Options::new(width)
                    .initial_indent("   ")
                    .subsequent_indent("   "),
            );
            for line in wrapped {
                lines.push(line.into_owned().dim().into());
            }
        }

        lines
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
        selected_idx: Option<usize>,
        visible_height: usize,
    ) -> usize {
        if item_groups.is_empty() || visible_height == 0 {
            return 0;
        }

        let Some(selected_idx) = selected_idx else {
            return 0;
        };
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

    fn toggle_selected(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };

        if let Some(item) = self.features.get_mut(selected_idx) {
            item.enabled = !item.enabled;
        }
    }

    fn updates(&self) -> Vec<(Feature, bool)> {
        self.features
            .iter()
            .filter_map(|item| {
                let initial_enabled = self.initial_feature_states.get(&item.feature)?;
                (*initial_enabled != item.enabled).then_some((item.feature, item.enabled))
            })
            .collect()
    }

    fn move_up(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        if selected_idx == 0 {
            self.selected_idx = Some(self.features.len().saturating_sub(1));
        } else {
            self.selected_idx = Some(selected_idx - 1);
        }
    }

    fn move_down(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        self.selected_idx = Some((selected_idx + 1) % self.features.len());
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

    use pretty_assertions::assert_eq;
    use ratatui::Terminal;

    use crate::test_backend::VT100Backend;

    fn item(
        feature: Feature,
        name: &str,
        description: &str,
        enabled: bool,
    ) -> ExperimentalFeatureItem {
        ExperimentalFeatureItem {
            feature,
            name: name.to_string(),
            description: description.to_string(),
            enabled,
        }
    }

    #[test]
    fn renders_centered_experimental_features_modal() {
        let modal = ExperimentalFeaturesModal::new(vec![
            item(
                Feature::GhostCommit,
                "Ghost snapshots",
                "Capture undo snapshots each turn.",
                false,
            ),
            item(
                Feature::ShellTool,
                "Shell tool",
                "Allow the model to run shell commands.",
                true,
            ),
        ]);

        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Experimental features"));
        assert!(rendered.contains("Ghost snapshots"));
        assert!(rendered.contains("Shell tool"));
        assert!(rendered.contains("[ ]"));
        assert!(rendered.contains("[x]"));
        assert!(rendered.contains("Enter"));
        assert!(rendered.contains("Esc"));
    }

    #[test]
    fn enter_toggles_selected_feature_and_escape_returns_update() {
        let mut modal = ExperimentalFeaturesModal::new(vec![item(
            Feature::GhostCommit,
            "Ghost snapshots",
            "Capture undo snapshots each turn.",
            false,
        )]);

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ExperimentalFeaturesModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ExperimentalFeaturesModalAction::SaveAndClose {
                updates: vec![(Feature::GhostCommit, true)],
            }
        );
    }

    #[test]
    fn reverted_toggle_returns_no_updates() {
        let mut modal = ExperimentalFeaturesModal::new(vec![item(
            Feature::GhostCommit,
            "Ghost snapshots",
            "Capture undo snapshots each turn.",
            false,
        )]);

        modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ExperimentalFeaturesModalAction::SaveAndClose { updates: vec![] }
        );
    }

    #[test]
    fn empty_feature_list_renders_empty_state() {
        let modal = ExperimentalFeaturesModal::new(vec![]);

        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("No experimental features available for now"));
    }

    #[test]
    fn up_down_wrap_selection() {
        let mut modal = ExperimentalFeaturesModal::new(vec![
            item(
                Feature::GhostCommit,
                "Ghost snapshots",
                "Capture undo snapshots each turn.",
                false,
            ),
            item(
                Feature::ShellTool,
                "Shell tool",
                "Allow the model to run shell commands.",
                true,
            ),
        ]);

        modal.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ExperimentalFeaturesModalAction::SaveAndClose {
                updates: vec![(Feature::ShellTool, false)],
            }
        );
    }
}
