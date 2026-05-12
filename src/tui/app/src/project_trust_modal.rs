use std::path::PathBuf;

use adam_agent::git_info::get_git_repo_root;
use adam_protocol::config_types::TrustLevel;
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
pub(crate) struct ProjectTrustModal {
    cwd: PathBuf,
    selected: TrustLevel,
    is_git_repo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectTrustModalAction {
    None,
    Selected(TrustLevel),
    Exit,
}

impl ProjectTrustModal {
    pub(crate) fn new(cwd: PathBuf) -> Self {
        let is_git_repo = get_git_repo_root(&cwd).is_some();
        let selected = if is_git_repo {
            TrustLevel::Trusted
        } else {
            TrustLevel::Untrusted
        };
        Self {
            cwd,
            selected,
            is_git_repo,
        }
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> ProjectTrustModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ProjectTrustModalAction::None;
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
                self.selected = TrustLevel::Trusted;
                ProjectTrustModalAction::None
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
                self.selected = TrustLevel::Untrusted;
                ProjectTrustModalAction::None
            }
            KeyEvent {
                code: KeyCode::Char('1'),
                modifiers: KeyModifiers::NONE,
                ..
            } => ProjectTrustModalAction::Selected(TrustLevel::Trusted),
            KeyEvent {
                code: KeyCode::Char('2'),
                modifiers: KeyModifiers::NONE,
                ..
            } => ProjectTrustModalAction::Selected(TrustLevel::Untrusted),
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => ProjectTrustModalAction::Selected(self.selected),
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => ProjectTrustModalAction::Exit,
            _ => ProjectTrustModalAction::None,
        }
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let modal_area = centered_modal_area(area);
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

        let content_width = content_area.width.max(1) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push("Trust this project?".bold().into());
        lines.push(self.cwd.display().to_string().dim().into());
        lines.push("".into());
        self.push_option_lines(
            &mut lines,
            content_width,
            TrustLevel::Trusted,
            "1. Trust this project",
            if self.is_git_repo {
                "Allow Adam to work in this folder with the standard project permissions."
            } else {
                "Allow Adam to work here without the extra first-run restriction."
            },
        );
        lines.push("".into());
        self.push_option_lines(
            &mut lines,
            content_width,
            TrustLevel::Untrusted,
            "2. Require approval",
            "Ask before edits and commands in this project.",
        );
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

    fn push_option_lines(
        &self,
        lines: &mut Vec<Line<'static>>,
        width: usize,
        trust_level: TrustLevel,
        label: &'static str,
        description: &'static str,
    ) {
        let selected = self.selected == trust_level;
        let marker = if selected { "›".cyan() } else { " ".into() };
        let label = if selected { label.bold() } else { label.into() };
        lines.push(vec![marker, " ".into(), label].into());

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

    #[cfg(test)]
    pub(crate) fn selected(&self) -> TrustLevel {
        self.selected
    }
}

fn centered_modal_area(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(76).max(area.width.min(44));
    let height = area
        .height
        .saturating_sub(2)
        .min(14)
        .max(area.height.min(8));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crossterm::event::KeyModifiers;
    use pretty_assertions::assert_eq;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render_to_string(modal: &ProjectTrustModal, area: Rect) -> String {
        let mut buf = Buffer::empty(area);
        modal.render(area, &mut buf);
        (0..area.height)
            .map(|row| {
                let mut line = String::new();
                for col in 0..area.width {
                    line.push_str(buf[(area.x + col, area.y + row)].symbol());
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn defaults_to_trusted_inside_git_repo() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(repo.path().join(".git")).expect("create git marker");

        let modal = ProjectTrustModal::new(repo.path().to_path_buf());

        assert_eq!(modal.selected(), TrustLevel::Trusted);
    }

    #[test]
    fn defaults_to_untrusted_outside_git_repo() {
        let cwd = tempfile::tempdir().expect("tempdir");

        let modal = ProjectTrustModal::new(cwd.path().to_path_buf());

        assert_eq!(modal.selected(), TrustLevel::Untrusted);
    }

    #[test]
    fn navigation_and_selection_actions() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let mut modal = ProjectTrustModal::new(cwd.path().to_path_buf());

        assert_eq!(
            modal.handle_key_event(key(KeyCode::Up)),
            ProjectTrustModalAction::None
        );
        assert_eq!(modal.selected(), TrustLevel::Trusted);
        assert_eq!(
            modal.handle_key_event(key(KeyCode::Down)),
            ProjectTrustModalAction::None
        );
        assert_eq!(modal.selected(), TrustLevel::Untrusted);
        assert_eq!(
            modal.handle_key_event(key(KeyCode::Char('1'))),
            ProjectTrustModalAction::Selected(TrustLevel::Trusted)
        );
        assert_eq!(
            modal.handle_key_event(key(KeyCode::Char('2'))),
            ProjectTrustModalAction::Selected(TrustLevel::Untrusted)
        );
        assert_eq!(
            modal.handle_key_event(key(KeyCode::Enter)),
            ProjectTrustModalAction::Selected(TrustLevel::Untrusted)
        );
    }

    #[test]
    fn escape_and_ctrl_c_exit() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let mut modal = ProjectTrustModal::new(cwd.path().to_path_buf());

        assert_eq!(
            modal.handle_key_event(key(KeyCode::Esc)),
            ProjectTrustModalAction::Exit
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ProjectTrustModalAction::Exit
        );
    }

    #[test]
    fn render_is_centered_modal_not_bottom_pane() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let modal = ProjectTrustModal::new(cwd.path().to_path_buf());
        let rendered = render_to_string(&modal, Rect::new(0, 0, 100, 30));

        assert!(
            rendered.lines().take(6).all(|line| line.trim().is_empty()),
            "modal should not start at the top:\n{rendered}"
        );
        assert!(
            rendered.contains("Trust this project?"),
            "expected modal title:\n{rendered}"
        );
        assert!(
            rendered.contains('╭') && rendered.contains('╯'),
            "expected rounded modal border:\n{rendered}"
        );
        assert!(
            !rendered.contains('┌') && !rendered.contains('┘'),
            "expected no square modal border:\n{rendered}"
        );
        assert!(
            rendered.contains("│ Trust this project?"),
            "expected one-cell horizontal padding inside border:\n{rendered}"
        );
        assert!(
            rendered.contains("› 2. Require approval"),
            "expected default non-git selection:\n{rendered}"
        );
    }
}
