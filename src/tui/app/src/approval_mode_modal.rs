use adam_agent::protocol::AskForApproval;
use adam_agent::protocol::SandboxPolicy;
use adam_common::approval_presets::ApprovalPreset;
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
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;

#[derive(Debug, Clone)]
pub(crate) struct ApprovalModeItem {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) is_current: bool,
    pub(crate) disabled_reason: Option<String>,
    pub(crate) action: ApprovalModeAction,
}

#[derive(Debug, Clone)]
pub(crate) enum ApprovalModeAction {
    ApplyPreset {
        approval: AskForApproval,
        sandbox: SandboxPolicy,
    },
    OpenApprovals,
    OpenPermissions,
    OpenFullAccessConfirmation {
        preset: ApprovalPreset,
        return_to_permissions: bool,
    },
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    OpenWorldWritableWarningConfirmation {
        preset: Option<ApprovalPreset>,
        sample_paths: Vec<String>,
        extra_count: usize,
        failed_scan: bool,
    },
    #[cfg(target_os = "windows")]
    OpenWindowsSandboxEnablePrompt {
        preset: ApprovalPreset,
    },
    ConfirmFullAccess {
        approval: AskForApproval,
        sandbox: SandboxPolicy,
        remember: bool,
    },
    ConfirmWorldWritable {
        preset: Option<ApprovalPreset>,
        remember: bool,
    },
    #[cfg(target_os = "windows")]
    BeginWindowsSandboxElevatedSetup {
        preset: ApprovalPreset,
        counter: Option<&'static str>,
    },
    #[cfg(target_os = "windows")]
    EnableWindowsSandboxForAgentMode {
        preset: ApprovalPreset,
        mode: WindowsSandboxEnableMode,
        counter: Option<&'static str>,
    },
    #[cfg(target_os = "windows")]
    StayInCurrentWindowsMode {
        read_only_preset: Option<ApprovalPreset>,
        counter: &'static str,
    },
}

pub(crate) struct ApprovalModeModal {
    header: Vec<Line<'static>>,
    items: Vec<ApprovalModeItem>,
    selected_idx: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApprovalModeModalAction {
    None,
    Exit,
    Selected(ApprovalModeAction),
}

struct ModalRenderLines {
    header: Vec<Line<'static>>,
    item_groups: Vec<Vec<Line<'static>>>,
    footer: Vec<Line<'static>>,
}

impl PartialEq for ApprovalModeAction {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl Eq for ApprovalModeAction {}

impl ApprovalModeModal {
    pub(crate) fn new(header: Vec<Line<'static>>, items: Vec<ApprovalModeItem>) -> Self {
        let selected_idx = Self::initial_selected_idx(&items);
        Self {
            header,
            items,
            selected_idx,
        }
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> ApprovalModeModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ApprovalModeModalAction::None;
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
                ApprovalModeModalAction::None
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
                ApprovalModeModalAction::None
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.selected_action(),
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => ApprovalModeModalAction::Exit,
            _ => ApprovalModeModalAction::None,
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
        let reserved_list_height = if lines.item_groups.is_empty() {
            0
        } else {
            1.min(content_area.height)
        };
        let footer_height = (lines.footer.len() as u16)
            .min(content_area.height.saturating_sub(reserved_list_height));
        let header_height = (lines.header.len() as u16).min(
            content_area
                .height
                .saturating_sub(footer_height)
                .saturating_sub(reserved_list_height),
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

    fn render_lines(&self, width: usize) -> ModalRenderLines {
        ModalRenderLines {
            header: self.header_lines(width),
            item_groups: self.item_groups(width),
            footer: vec![
                "".into(),
                vec![
                    "Enter".cyan(),
                    " select   ".dim(),
                    "↑↓/jk".cyan(),
                    " move   ".dim(),
                    "Esc".cyan(),
                    " close".dim(),
                ]
                .into(),
            ],
        }
    }

    fn initial_selected_idx(items: &[ApprovalModeItem]) -> Option<usize> {
        items
            .iter()
            .position(|item| item.is_current && Self::item_is_enabled(item))
            .or_else(|| items.iter().position(Self::item_is_enabled))
    }

    fn item_is_enabled(item: &ApprovalModeItem) -> bool {
        item.disabled_reason.is_none()
    }

    fn header_lines(&self, width: usize) -> Vec<Line<'static>> {
        word_wrap_lines(self.header.iter(), RtOptions::new(width))
    }

    fn item_groups(&self, width: usize) -> Vec<Vec<Line<'static>>> {
        if self.items.is_empty() {
            return vec![vec!["  No permission presets available".dim().into()]];
        }

        self.items
            .iter()
            .enumerate()
            .map(|(idx, item)| self.item_lines(width, idx, item))
            .collect()
    }

    fn item_lines(&self, width: usize, idx: usize, item: &ApprovalModeItem) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let selected = self.selected_idx == Some(idx);
        let disabled = item.disabled_reason.is_some();
        let marker = if selected { "›".cyan() } else { " ".into() };
        let label = if disabled {
            item.name.clone().dim()
        } else if selected {
            item.name.clone().bold()
        } else {
            item.name.clone().into()
        };
        let current = item.is_current.then_some(" (current)".dim());
        let mut first_line = vec![marker, " ".into(), label];
        if let Some(current) = current {
            first_line.push(current);
        }
        lines.push(first_line.into());

        if let Some(description) = item.description.as_deref().filter(|text| !text.is_empty()) {
            let wrapped = wrap(
                description,
                Options::new(width)
                    .initial_indent("  ")
                    .subsequent_indent("  "),
            );
            for line in wrapped {
                lines.push(line.into_owned().dim().into());
            }
        }

        if let Some(reason) = item.disabled_reason.as_deref() {
            let wrapped = wrap(
                reason,
                Options::new(width)
                    .initial_indent("  Disabled: ")
                    .subsequent_indent("            "),
            );
            for line in wrapped {
                lines.push(line.into_owned().red().into());
            }
        }

        lines
    }

    fn selected_action(&self) -> ApprovalModeModalAction {
        let Some(idx) = self.selected_idx else {
            return ApprovalModeModalAction::None;
        };
        let Some(item) = self.items.get(idx) else {
            return ApprovalModeModalAction::None;
        };
        if item.disabled_reason.is_some() {
            return ApprovalModeModalAction::None;
        }
        ApprovalModeModalAction::Selected(item.action.clone())
    }

    fn move_up(&mut self) {
        self.selected_idx = match self.selected_idx {
            Some(selected) => self.previous_enabled_idx(selected),
            None => self.last_enabled_idx(),
        };
    }

    fn move_down(&mut self) {
        self.selected_idx = match self.selected_idx {
            Some(selected) => self.next_enabled_idx(selected),
            None => self.first_enabled_idx(),
        };
    }

    fn first_enabled_idx(&self) -> Option<usize> {
        self.items.iter().position(Self::item_is_enabled)
    }

    fn last_enabled_idx(&self) -> Option<usize> {
        self.items.iter().rposition(Self::item_is_enabled)
    }

    fn next_enabled_idx(&self, selected: usize) -> Option<usize> {
        let len = self.items.len();
        if len == 0 {
            return None;
        }
        let selected = selected % len;
        (1..=len)
            .map(|offset| (selected + offset) % len)
            .find(|idx| Self::item_is_enabled(&self.items[*idx]))
    }

    fn previous_enabled_idx(&self, selected: usize) -> Option<usize> {
        let len = self.items.len();
        if len == 0 {
            return None;
        }
        let selected = selected % len;
        (1..=len)
            .map(|offset| (selected + len - offset) % len)
            .find(|idx| Self::item_is_enabled(&self.items[*idx]))
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
            selected_top.min(max_scroll)
        } else if selected_bottom > visible_height {
            selected_bottom
                .saturating_sub(visible_height)
                .min(max_scroll)
        } else if selected_top > max_scroll {
            max_scroll
        } else {
            0
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
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;

    use crate::test_backend::VT100Backend;

    fn item(name: &str, is_current: bool) -> ApprovalModeItem {
        ApprovalModeItem {
            name: name.to_string(),
            description: Some("Adam can read and edit files in the current workspace.".to_string()),
            is_current,
            disabled_reason: None,
            action: ApprovalModeAction::ApplyPreset {
                approval: AskForApproval::OnRequest,
                sandbox: SandboxPolicy::ReadOnly,
            },
        }
    }

    fn disabled_item(name: &str, is_current: bool) -> ApprovalModeItem {
        ApprovalModeItem {
            disabled_reason: Some("Policy cannot be changed here".to_string()),
            ..item(name, is_current)
        }
    }

    #[test]
    fn new_selects_current_enabled_item() {
        let modal = ApprovalModeModal::new(
            vec!["Update Model Permissions".bold().into(), "".into()],
            vec![
                item("Read Only", false),
                item("Default", true),
                item("Full Access", false),
            ],
        );

        assert_eq!(modal.selected_idx, Some(1));
    }

    #[test]
    fn new_falls_back_to_first_enabled_item_when_current_is_disabled() {
        let modal = ApprovalModeModal::new(
            vec!["Update Model Permissions".bold().into(), "".into()],
            vec![
                disabled_item("Read Only", true),
                item("Default", false),
                item("Full Access", false),
            ],
        );

        assert_eq!(modal.selected_idx, Some(1));
    }

    #[test]
    fn new_has_no_selection_when_all_items_are_disabled() {
        let mut modal = ApprovalModeModal::new(
            vec!["Update Model Permissions".bold().into(), "".into()],
            vec![
                disabled_item("Read Only", true),
                disabled_item("Default", false),
            ],
        );

        assert_eq!(modal.selected_idx, None);
        assert_eq!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Enter)),
            ApprovalModeModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Down)),
            ApprovalModeModalAction::None
        );
        assert_eq!(modal.selected_idx, None);
        assert_eq!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Up)),
            ApprovalModeModalAction::None
        );
        assert_eq!(modal.selected_idx, None);
    }

    #[test]
    fn navigation_skips_disabled_items() {
        let mut modal = ApprovalModeModal::new(
            vec!["Update Model Permissions".bold().into(), "".into()],
            vec![
                item("Read Only", true),
                disabled_item("Default", false),
                item("Full Access", false),
            ],
        );

        assert_eq!(modal.selected_idx, Some(0));

        assert_eq!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Down)),
            ApprovalModeModalAction::None
        );
        assert_eq!(modal.selected_idx, Some(2));

        assert_eq!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Down)),
            ApprovalModeModalAction::None
        );
        assert_eq!(modal.selected_idx, Some(0));

        assert_eq!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Up)),
            ApprovalModeModalAction::None
        );
        assert_eq!(modal.selected_idx, Some(2));

        assert_eq!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Up)),
            ApprovalModeModalAction::None
        );
        assert_eq!(modal.selected_idx, Some(0));
    }

    #[test]
    fn render_lines_wraps_header_content() {
        let modal = ApprovalModeModal::new(
            vec![
                "Enable full access?".bold().into(),
                vec![
                    "When Adam runs with full access, it can edit any file on your computer and run commands with network, without your approval. ".into(),
                    "Exercise caution when enabling full access. This significantly increases the risk of data loss, leaks, or unexpected behavior.".red(),
                ]
                .into(),
                "".into(),
            ],
            vec![item("Cancel", false)],
        );

        let lines = modal.render_lines(32);
        let header_text = lines
            .header
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(lines.header.len() > modal.header.len());
        assert!(lines.header.iter().all(|line| line.width() <= 32));
        assert!(header_text.contains("When Adam runs with full access"));
        assert!(header_text.contains("Exercise caution"));
        assert!(header_text.contains("unexpected behavior"));
    }

    #[test]
    fn render_keeps_list_visible_when_wrapped_header_is_tall() {
        let modal = ApprovalModeModal::new(
            vec![
                "Enable full access?".bold().into(),
                vec![
                    "When Adam runs with full access, it can edit any file on your computer and run commands with network, without your approval. ".into(),
                    "Exercise caution when enabling full access. This significantly increases the risk of data loss, leaks, or unexpected behavior.".red(),
                ]
                .into(),
                "".into(),
            ],
            vec![
                ApprovalModeItem {
                    name: "Yes, continue anyway".to_string(),
                    description: Some("Apply full access for this session".to_string()),
                    is_current: false,
                    disabled_reason: None,
                    action: ApprovalModeAction::ConfirmFullAccess {
                        approval: AskForApproval::Never,
                        sandbox: SandboxPolicy::DangerFullAccess,
                        remember: false,
                    },
                },
                ApprovalModeItem {
                    name: "Cancel".to_string(),
                    description: Some("Go back without enabling full access".to_string()),
                    is_current: false,
                    disabled_reason: None,
                    action: ApprovalModeAction::OpenApprovals,
                },
            ],
        );
        let mut terminal = Terminal::new(VT100Backend::new(80, 12)).expect("terminal");

        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        let rendered = terminal.backend().to_string();

        assert!(rendered.contains("Enable full access?"));
        assert!(rendered.contains("Yes, continue anyway"));
    }

    #[test]
    fn enter_selects_enabled_item() {
        let mut modal = ApprovalModeModal::new(
            vec!["Update Model Permissions".bold().into(), "".into()],
            vec![item("Read Only", true)],
        );

        assert!(matches!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Enter)),
            ApprovalModeModalAction::Selected(ApprovalModeAction::ApplyPreset { .. })
        ));
    }

    #[test]
    fn disabled_item_does_not_select() {
        let mut disabled = item("Full Access", false);
        disabled.disabled_reason = Some("Policy cannot be changed here".to_string());
        let mut modal = ApprovalModeModal::new(
            vec!["Update Model Permissions".bold().into(), "".into()],
            vec![disabled],
        );

        assert_eq!(
            modal.handle_key_event(KeyEvent::from(KeyCode::Enter)),
            ApprovalModeModalAction::None
        );
    }

    #[test]
    fn renders_centered_approval_mode_modal() {
        let modal = ApprovalModeModal::new(
            vec![
                "Update Model Permissions".bold().into(),
                "Choose what Adam can do without approval.".dim().into(),
                "".into(),
            ],
            vec![item("Read Only", true), item("Default", false)],
        );
        let mut terminal = Terminal::new(VT100Backend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        let rendered = terminal.backend().to_string();

        assert!(rendered.contains("Update Model Permissions"));
        assert!(rendered.contains("Read Only"));
        assert!(rendered.contains("(current)"));
        assert!(rendered.contains("Enter"));
    }
}
