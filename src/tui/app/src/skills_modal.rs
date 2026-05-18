use std::path::Path;
use std::path::PathBuf;

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
use crate::skills_helpers::match_skill;

const SEARCH_PROMPT_PREFIX: &str = "> ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillsModalItem {
    pub(crate) name: String,
    pub(crate) skill_name: String,
    pub(crate) description: String,
    pub(crate) enabled: bool,
    pub(crate) path: PathBuf,
}

pub(crate) struct SkillsModal {
    items: Vec<SkillsModalItem>,
    selected_idx: Option<usize>,
    search_query: String,
    filtered_indices: Vec<usize>,
    error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SkillsModalAction {
    None,
    Exit,
    Toggle { path: PathBuf, enabled: bool },
}

struct ModalRenderLines {
    header: Vec<Line<'static>>,
    item_groups: Vec<Vec<Line<'static>>>,
    footer: Vec<Line<'static>>,
}

impl SkillsModal {
    pub(crate) fn new(items: Vec<SkillsModalItem>) -> Option<Self> {
        if items.is_empty() {
            return None;
        }

        let mut modal = Self {
            items,
            selected_idx: Some(0),
            search_query: String::new(),
            filtered_indices: Vec::new(),
            error_message: None,
        };
        modal.apply_filter();
        Some(modal)
    }

    pub(crate) fn set_skill_enabled(&mut self, path: &Path, enabled: bool) {
        if let Some(item) = self.items.iter_mut().find(|item| item.path == path) {
            item.enabled = enabled;
        }
        self.error_message = None;
    }

    pub(crate) fn set_error_message(&mut self, message: String) {
        self.error_message = Some(message);
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> SkillsModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return SkillsModalAction::None;
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
            } => {
                self.move_up();
                SkillsModalAction::None
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
            } => {
                self.move_down();
                SkillsModalAction::None
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                self.search_query.pop();
                self.apply_filter();
                SkillsModalAction::None
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.toggle_selected(),
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => SkillsModalAction::Exit,
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.search_query.push(c);
                self.apply_filter();
                SkillsModalAction::None
            }
            _ => SkillsModalAction::None,
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
        let min_list_height = if lines.item_groups.is_empty() {
            0
        } else {
            content_area.height.min(1)
        };
        let footer_height =
            (lines.footer.len() as u16).min(content_area.height.saturating_sub(min_list_height));
        let header_height = (lines.header.len() as u16).min(
            content_area
                .height
                .saturating_sub(footer_height)
                .saturating_sub(min_list_height),
        );
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

    fn apply_filter(&mut self) {
        let previously_selected = self
            .selected_idx
            .and_then(|visible_idx| self.filtered_indices.get(visible_idx).copied());

        let filter = self.search_query.trim();
        if filter.is_empty() {
            self.filtered_indices = (0..self.items.len()).collect();
        } else {
            let mut matches: Vec<(usize, i32)> = Vec::new();
            for (idx, item) in self.items.iter().enumerate() {
                if let Some((_indices, score)) =
                    match_skill(filter, item.name.as_str(), item.skill_name.as_str())
                {
                    matches.push((idx, score));
                }
            }

            matches.sort_by(|a, b| {
                a.1.cmp(&b.1).then_with(|| {
                    let an = self.items[a.0].name.as_str();
                    let bn = self.items[b.0].name.as_str();
                    an.cmp(bn)
                })
            });

            self.filtered_indices = matches.into_iter().map(|(idx, _score)| idx).collect();
        }

        self.selected_idx = previously_selected
            .and_then(|actual_idx| {
                self.filtered_indices
                    .iter()
                    .position(|idx| *idx == actual_idx)
            })
            .or_else(|| (!self.filtered_indices.is_empty()).then_some(0));
    }

    fn move_up(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        let len = self.filtered_indices.len();
        if len == 0 {
            self.selected_idx = None;
        } else if selected_idx == 0 {
            self.selected_idx = Some(len.saturating_sub(1));
        } else {
            self.selected_idx = Some(selected_idx - 1);
        }
    }

    fn move_down(&mut self) {
        let Some(selected_idx) = self.selected_idx else {
            return;
        };
        let len = self.filtered_indices.len();
        if len == 0 {
            self.selected_idx = None;
        } else {
            self.selected_idx = Some((selected_idx + 1) % len);
        }
    }

    fn toggle_selected(&mut self) -> SkillsModalAction {
        let Some(selected_idx) = self.selected_idx else {
            return SkillsModalAction::None;
        };
        let Some(actual_idx) = self.filtered_indices.get(selected_idx).copied() else {
            return SkillsModalAction::None;
        };
        let Some(item) = self.items.get_mut(actual_idx) else {
            return SkillsModalAction::None;
        };

        SkillsModalAction::Toggle {
            path: item.path.clone(),
            enabled: !item.enabled,
        }
    }

    fn render_lines(&self, width: usize) -> ModalRenderLines {
        let mut footer = Vec::new();
        if let Some(message) = &self.error_message {
            footer.push("".into());
            let error = format!("Error: {message}");
            for line in wrap(error.as_str(), Options::new(width)) {
                footer.push(line.into_owned().red().into());
            }
        }
        footer.push("".into());
        footer.push(
            vec![
                "Space/Enter".cyan(),
                " toggle   ".dim(),
                "↑↓/Ctrl-N/P".cyan(),
                " move   ".dim(),
                "Esc".cyan(),
                " close".dim(),
            ]
            .into(),
        );

        ModalRenderLines {
            header: vec![
                "Skills".bold().into(),
                "Turn skills on or off. Changes are saved automatically."
                    .dim()
                    .into(),
                "".into(),
                "Search skills".dim().into(),
                self.search_line(),
                "".into(),
            ],
            item_groups: self.item_groups(width),
            footer,
        }
    }

    fn search_line(&self) -> Line<'static> {
        if self.search_query.is_empty() {
            Line::from(vec![SEARCH_PROMPT_PREFIX.dim()])
        } else {
            Line::from(vec![
                SEARCH_PROMPT_PREFIX.dim(),
                self.search_query.clone().into(),
            ])
        }
    }

    fn item_groups(&self, width: usize) -> Vec<Vec<Line<'static>>> {
        if self.filtered_indices.is_empty() {
            return vec![vec!["  No matching skills".dim().into()]];
        }

        self.filtered_indices
            .iter()
            .enumerate()
            .filter_map(|(visible_idx, actual_idx)| {
                self.items
                    .get(*actual_idx)
                    .map(|item| self.item_lines(width, visible_idx, item))
            })
            .collect()
    }

    fn item_lines(
        &self,
        width: usize,
        visible_idx: usize,
        item: &SkillsModalItem,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let selected = self.selected_idx == Some(visible_idx);
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

        let mut group_starts = Vec::with_capacity(item_groups.len());
        let mut total_lines = 0usize;
        for group in item_groups {
            group_starts.push(total_lines);
            total_lines = total_lines.saturating_add(group.len());
        }

        let max_scroll = total_lines.saturating_sub(visible_height);
        let selected_top = group_starts[selected_idx];
        let selected_height = item_groups[selected_idx].len().max(1);
        let selected_bottom = selected_top.saturating_add(selected_height);

        if selected_height >= visible_height {
            return selected_top.min(max_scroll);
        }
        if selected_bottom <= visible_height {
            0
        } else {
            let earliest_start = selected_bottom.saturating_sub(visible_height);
            group_starts
                .iter()
                .copied()
                .rev()
                .find(|start| {
                    *start >= earliest_start && *start <= selected_top && *start <= max_scroll
                })
                .unwrap_or_else(|| selected_top.min(max_scroll))
        }
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

    use crate::test_backend::VT100Backend;

    fn item(name: &str, skill_name: &str, description: &str, enabled: bool) -> SkillsModalItem {
        SkillsModalItem {
            name: name.to_string(),
            skill_name: skill_name.to_string(),
            description: description.to_string(),
            enabled,
            path: PathBuf::from(format!("/tmp/skills/{skill_name}.toml")),
        }
    }

    fn modal() -> SkillsModal {
        SkillsModal::new(vec![
            item(
                "Repo Scout",
                "repo_scout",
                "Summarize the repo layout",
                true,
            ),
            item(
                "Changelog Writer",
                "changelog_writer",
                "Draft release notes",
                false,
            ),
        ])
        .expect("modal")
    }

    fn render_modal_with_size(modal: &SkillsModal, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(VT100Backend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        terminal.backend().to_string()
    }

    fn render_modal(modal: &SkillsModal) -> String {
        render_modal_with_size(modal, 100, 32)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn line_group(len: usize) -> Vec<Line<'static>> {
        (0..len).map(|idx| format!("line {idx}").into()).collect()
    }

    #[test]
    fn renders_centered_skills_modal() {
        let rendered = render_modal(&modal());
        assert_snapshot!("skills_modal_basic", rendered);
    }

    #[test]
    fn renders_at_least_one_skill_on_short_terminal() {
        let rendered = render_modal_with_size(&modal(), 100, 14);

        assert!(rendered.contains("Repo Scout"));
    }

    #[test]
    fn filters_skills_by_query() {
        let mut modal = modal();
        modal.handle_key_event(key(KeyCode::Char('c')));
        modal.handle_key_event(key(KeyCode::Char('h')));
        let rendered = render_modal(&modal);

        assert!(rendered.contains("Changelog Writer"));
        assert!(!rendered.contains("Repo Scout"));
    }

    #[test]
    fn backspace_updates_filter() {
        let mut modal = modal();
        modal.handle_key_event(key(KeyCode::Char('c')));
        modal.handle_key_event(key(KeyCode::Char('h')));
        modal.handle_key_event(key(KeyCode::Backspace));
        modal.handle_key_event(key(KeyCode::Backspace));
        let rendered = render_modal(&modal);

        assert!(rendered.contains("Changelog Writer"));
        assert!(rendered.contains("Repo Scout"));
    }

    #[test]
    fn lowercase_j_and_k_update_search_query() {
        let mut modal = modal();
        modal.handle_key_event(key(KeyCode::Char('j')));
        modal.handle_key_event(key(KeyCode::Char('k')));

        assert_eq!(modal.search_query, "jk");
    }

    #[test]
    fn ctrl_n_and_ctrl_p_move_selection() {
        let mut modal = modal();
        modal.handle_key_event(key_with_modifiers(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(modal.selected_idx, Some(1));

        modal.handle_key_event(key_with_modifiers(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(modal.selected_idx, Some(0));
    }

    #[test]
    fn space_toggles_selected_skill() {
        let mut modal = modal();
        let action = modal.handle_key_event(key(KeyCode::Char(' ')));

        assert_eq!(
            action,
            SkillsModalAction::Toggle {
                path: PathBuf::from("/tmp/skills/repo_scout.toml"),
                enabled: false,
            }
        );
        assert!(modal.items[0].enabled);
    }

    #[test]
    fn enter_toggles_selected_skill() {
        let mut modal = modal();
        let action = modal.handle_key_event(key(KeyCode::Enter));

        assert_eq!(
            action,
            SkillsModalAction::Toggle {
                path: PathBuf::from("/tmp/skills/repo_scout.toml"),
                enabled: false,
            }
        );
        assert!(modal.items[0].enabled);
    }

    #[test]
    fn set_skill_enabled_updates_checkbox_after_save() {
        let mut modal = modal();
        let path = PathBuf::from("/tmp/skills/repo_scout.toml");

        modal.set_skill_enabled(&path, false);

        assert_eq!(
            modal.items[0],
            SkillsModalItem {
                name: "Repo Scout".to_string(),
                skill_name: "repo_scout".to_string(),
                description: "Summarize the repo layout".to_string(),
                enabled: false,
                path,
            }
        );
    }

    #[test]
    fn error_message_renders_inside_modal() {
        let mut modal = modal();
        modal.set_error_message("could not save config".to_string());

        let rendered = render_modal(&modal);

        assert!(rendered.contains("Error: could not save config"));
    }

    #[test]
    fn escape_exits_modal() {
        let mut modal = modal();
        assert_eq!(
            modal.handle_key_event(key(KeyCode::Esc)),
            SkillsModalAction::Exit
        );
    }

    #[test]
    fn no_matches_render_empty_state() {
        let mut modal = modal();
        modal.handle_key_event(key(KeyCode::Char('z')));
        let rendered = render_modal(&modal);

        assert!(rendered.contains("No matching skills"));
        assert!(!rendered.contains("Repo Scout"));
        assert!(!rendered.contains("Changelog Writer"));
    }

    #[test]
    fn list_scroll_top_aligns_to_item_group_start() {
        let item_groups = vec![line_group(4), line_group(3), line_group(1)];

        assert_eq!(
            SkillsModal::list_scroll_top_for_selected_item(&item_groups, Some(0), 3),
            0
        );
        assert_eq!(
            SkillsModal::list_scroll_top_for_selected_item(&item_groups, Some(1), 3),
            4
        );
        assert_eq!(
            SkillsModal::list_scroll_top_for_selected_item(&item_groups, Some(2), 3),
            5
        );
    }

    #[test]
    fn list_scroll_top_clamps_tall_selected_item_to_max_scroll() {
        let item_groups = vec![line_group(2), line_group(6)];

        assert_eq!(
            SkillsModal::list_scroll_top_for_selected_item(&item_groups, Some(1), 3),
            2
        );
    }

    #[test]
    fn list_scroll_top_keeps_middle_selection_group_aligned() {
        let item_groups = vec![line_group(4), line_group(3), line_group(3)];

        assert_eq!(
            SkillsModal::list_scroll_top_for_selected_item(&item_groups, Some(1), 3),
            4
        );
    }
}
