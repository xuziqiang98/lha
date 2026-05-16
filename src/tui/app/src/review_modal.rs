use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;

use adam_agent::git_info::current_branch_name;
use adam_agent::git_info::local_git_branches;
use adam_agent::protocol::ReviewRequest;
use adam_agent::protocol::ReviewTarget;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;
use textwrap::Options;
use textwrap::wrap;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::TextArea;
use crate::bottom_pane::TextAreaState;
use crate::bottom_pane::popup_consts::MAX_POPUP_ROWS;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::line_utils::push_owned_lines;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_line;

pub(crate) struct ReviewModal {
    view_stack: Vec<ReviewModalScreen>,
    app_event_tx: AppEventSender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewModalAction {
    None,
    Exit,
}

impl ReviewModal {
    pub(crate) fn new(cwd: PathBuf, app_event_tx: AppEventSender) -> Self {
        let mut modal = Self {
            view_stack: Vec::new(),
            app_event_tx,
        };
        modal.push_review_preset_view(cwd);
        modal
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> ReviewModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ReviewModalAction::None;
        }

        if matches!(
            key_event,
            KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            }
        ) {
            return ReviewModalAction::Exit;
        }

        if key_event.code == KeyCode::Esc {
            return self.handle_escape();
        }

        let Some(screen) = self.view_stack.last_mut() else {
            return ReviewModalAction::Exit;
        };
        match screen.handle_key_event(key_event) {
            ReviewScreenAction::None => ReviewModalAction::None,
            ReviewScreenAction::Activate(action) => self.activate(action),
        }
    }

    pub(crate) fn handle_paste(&mut self, pasted: String) {
        if let Some(screen) = self.view_stack.last_mut() {
            screen.handle_paste(pasted);
        }
    }

    pub(crate) async fn show_branch_picker(&mut self, cwd: &Path) {
        let branches = local_git_branches(cwd).await;
        let current_branch = current_branch_name(cwd)
            .await
            .unwrap_or_else(|| "(detached HEAD)".to_string());
        self.push_branch_picker_with_entries(current_branch, branches);
    }

    pub(crate) async fn show_commit_picker(&mut self, cwd: &Path) {
        let commits = adam_agent::git_info::recent_commits(cwd, 100).await;
        self.push_commit_picker_with_entries(commits);
    }

    pub(crate) fn show_custom_prompt(&mut self) {
        self.view_stack
            .push(ReviewModalScreen::CustomPrompt(ReviewPromptScreen::new()));
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

        if let Some(screen) = self.view_stack.last() {
            screen.render(content_area, buf);
        }
    }

    pub(crate) fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let modal_area = self.modal_area(area);
        let inner_area = modal_area.inset(Insets::vh(1, 1));
        let content_area = inner_area.inset(Insets::vh(1, 2));
        self.view_stack
            .last()
            .and_then(|screen| screen.cursor_pos(content_area))
    }

    fn handle_escape(&mut self) -> ReviewModalAction {
        if self.view_stack.len() <= 1 {
            ReviewModalAction::Exit
        } else {
            self.view_stack.pop();
            ReviewModalAction::None
        }
    }

    fn activate(&mut self, action: ReviewListAction) -> ReviewModalAction {
        match action {
            ReviewListAction::OpenBranchPicker(cwd) => {
                self.app_event_tx
                    .send(AppEvent::OpenReviewBranchPicker(cwd));
                ReviewModalAction::None
            }
            ReviewListAction::OpenCommitPicker(cwd) => {
                self.app_event_tx
                    .send(AppEvent::OpenReviewCommitPicker(cwd));
                ReviewModalAction::None
            }
            ReviewListAction::OpenCustomPrompt => {
                self.app_event_tx.send(AppEvent::OpenReviewCustomPrompt);
                ReviewModalAction::None
            }
            ReviewListAction::StartReview(review_request) => {
                self.app_event_tx
                    .send(AppEvent::StartReview { review_request });
                ReviewModalAction::Exit
            }
        }
    }

    fn push_review_preset_view(&mut self, cwd: PathBuf) {
        self.view_stack
            .push(ReviewModalScreen::List(ReviewListScreen::presets(cwd)));
    }

    fn push_branch_picker_with_entries(&mut self, current_branch: String, branches: Vec<String>) {
        self.view_stack
            .push(ReviewModalScreen::List(ReviewListScreen::branches(
                current_branch,
                branches,
            )));
    }

    fn push_commit_picker_with_entries(
        &mut self,
        entries: Vec<adam_agent::git_info::CommitLogEntry>,
    ) {
        self.view_stack
            .push(ReviewModalScreen::List(ReviewListScreen::commits(entries)));
    }

    fn modal_area(&self, area: Rect) -> Rect {
        let horizontal_margin = 4;
        let vertical_margin = 2;
        let width = area
            .width
            .saturating_sub(horizontal_margin)
            .min(88)
            .max(area.width.min(44));
        let content_width = width.saturating_sub(6);
        let desired_content_height = self
            .view_stack
            .last()
            .map(|screen| screen.desired_height(content_width))
            .unwrap_or(0);
        let height = desired_content_height
            .saturating_add(4)
            .min(area.height.saturating_sub(vertical_margin))
            .max(area.height.min(12));

        Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        }
    }
}

enum ReviewModalScreen {
    List(ReviewListScreen),
    CustomPrompt(ReviewPromptScreen),
}

impl ReviewModalScreen {
    fn handle_key_event(&mut self, key_event: KeyEvent) -> ReviewScreenAction {
        match self {
            Self::List(screen) => screen.handle_key_event(key_event),
            Self::CustomPrompt(screen) => screen.handle_key_event(key_event),
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        match self {
            Self::List(screen) => screen.handle_paste(pasted),
            Self::CustomPrompt(screen) => screen.handle_paste(pasted),
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::List(screen) => screen.render(area, buf),
            Self::CustomPrompt(screen) => screen.render(area, buf),
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        match self {
            Self::List(screen) => screen.desired_height(width),
            Self::CustomPrompt(screen) => screen.desired_height(width),
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        match self {
            Self::List(_) => None,
            Self::CustomPrompt(screen) => screen.cursor_pos(area),
        }
    }
}

enum ReviewScreenAction {
    None,
    Activate(ReviewListAction),
}

struct ReviewListScreen {
    title: String,
    subtitle: Option<String>,
    items: Vec<ReviewListItem>,
    selected_idx: usize,
    scroll_top: usize,
    search: Option<ReviewSearchState>,
}

impl ReviewListScreen {
    fn presets(cwd: PathBuf) -> Self {
        let items = vec![
            ReviewListItem {
                name: "Review against a base branch (PR Style)".to_string(),
                description: None,
                search_value: None,
                action: ReviewListAction::OpenBranchPicker(cwd.clone()),
            },
            ReviewListItem {
                name: "Review uncommitted changes".to_string(),
                description: None,
                search_value: None,
                action: ReviewListAction::StartReview(ReviewRequest {
                    target: ReviewTarget::UncommittedChanges,
                    user_facing_hint: None,
                }),
            },
            ReviewListItem {
                name: "Review a commit".to_string(),
                description: None,
                search_value: None,
                action: ReviewListAction::OpenCommitPicker(cwd),
            },
            ReviewListItem {
                name: "Custom review instructions".to_string(),
                description: None,
                search_value: None,
                action: ReviewListAction::OpenCustomPrompt,
            },
        ];
        Self {
            title: "Select a review preset".to_string(),
            subtitle: None,
            items,
            selected_idx: 0,
            scroll_top: 0,
            search: None,
        }
    }

    fn branches(current_branch: String, branches: Vec<String>) -> Self {
        let items = branches
            .into_iter()
            .map(|branch| ReviewListItem {
                name: format!("{current_branch} -> {branch}"),
                description: None,
                search_value: Some(branch.clone()),
                action: ReviewListAction::StartReview(ReviewRequest {
                    target: ReviewTarget::BaseBranch { branch },
                    user_facing_hint: None,
                }),
            })
            .collect();
        Self {
            title: "Select a base branch".to_string(),
            subtitle: None,
            items,
            selected_idx: 0,
            scroll_top: 0,
            search: Some(ReviewSearchState {
                query: String::new(),
                placeholder: "Type to search branches".to_string(),
            }),
        }
    }

    fn commits(entries: Vec<adam_agent::git_info::CommitLogEntry>) -> Self {
        let items = entries
            .into_iter()
            .map(|entry| {
                let subject = entry.subject;
                let sha = entry.sha;
                ReviewListItem {
                    name: subject.clone(),
                    description: None,
                    search_value: Some(format!("{subject} {sha}")),
                    action: ReviewListAction::StartReview(ReviewRequest {
                        target: ReviewTarget::Commit {
                            sha,
                            title: Some(subject),
                        },
                        user_facing_hint: None,
                    }),
                }
            })
            .collect();
        Self {
            title: "Select a commit to review".to_string(),
            subtitle: None,
            items,
            selected_idx: 0,
            scroll_top: 0,
            search: Some(ReviewSearchState {
                query: String::new(),
                placeholder: "Type to search commits".to_string(),
            }),
        }
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) -> ReviewScreenAction {
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
                ReviewScreenAction::None
            }
            KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.search.is_none() => {
                self.move_up();
                ReviewScreenAction::None
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
                ReviewScreenAction::None
            }
            KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.search.is_none() => {
                self.move_down();
                ReviewScreenAction::None
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self
                .selected_action()
                .map(ReviewScreenAction::Activate)
                .unwrap_or(ReviewScreenAction::None),
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.search.is_none() && c.is_ascii_digit() => {
                let Some(idx) = c.to_digit(10).and_then(|digit| {
                    usize::try_from(digit)
                        .ok()
                        .and_then(|digit| digit.checked_sub(1))
                }) else {
                    return ReviewScreenAction::None;
                };
                if idx >= self.items.len() {
                    return ReviewScreenAction::None;
                }
                self.selected_idx = idx;
                self.ensure_visible();
                self.selected_action()
                    .map(ReviewScreenAction::Activate)
                    .unwrap_or(ReviewScreenAction::None)
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if self.search.is_some()
                && !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.update_search_query(|query| query.push(c));
                ReviewScreenAction::None
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } if self.search.is_some() => {
                self.update_search_query(|query| {
                    query.pop();
                });
                ReviewScreenAction::None
            }
            _ => ReviewScreenAction::None,
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        if self.search.is_some() && !pasted.is_empty() {
            self.update_search_query(|query| query.push_str(&pasted));
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        let header_height = 1u16
            .saturating_add(u16::from(self.subtitle.is_some() || self.search.is_some()))
            .saturating_add(1);
        let rows_height =
            self.visible_indices()
                .iter()
                .enumerate()
                .fold(0u16, |height, (visible_idx, idx)| {
                    height.saturating_add(self.item_height(
                        width,
                        self.scroll_top + visible_idx,
                        &self.items[*idx],
                    ))
                });
        header_height
            .saturating_add(rows_height.max(1))
            .saturating_add(2)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line<'static>> = vec![self.title.clone().bold().into()];
        if let Some(search) = &self.search {
            let text = if search.query.is_empty() {
                search.placeholder.clone()
            } else {
                format!("Search: {}", search.query)
            };
            let line = if search.query.is_empty() {
                text.dim()
            } else {
                text.cyan()
            };
            lines.push(line.into());
        } else if let Some(subtitle) = &self.subtitle {
            lines.push(subtitle.clone().dim().into());
        }
        lines.push("".into());

        let visible_indices = self.visible_indices();
        if visible_indices.is_empty() {
            lines.push("no matches".dim().italic().into());
        } else {
            let width = area.width.max(1) as usize;
            for (visible_idx, item_idx) in visible_indices.into_iter().enumerate() {
                let actual_visible_idx = self.scroll_top + visible_idx;
                self.push_item_lines(&mut lines, width, actual_visible_idx, &self.items[item_idx]);
            }
        }

        lines.push("".into());
        lines.push(self.footer_line());

        Paragraph::new(lines).render(area, buf);
    }

    fn push_item_lines(
        &self,
        lines: &mut Vec<Line<'static>>,
        width: usize,
        visible_idx: usize,
        item: &ReviewListItem,
    ) {
        lines.extend(self.item_label_lines(width, visible_idx, item));

        if let Some(description) = &item.description {
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
    }

    fn item_label_lines(
        &self,
        width: usize,
        visible_idx: usize,
        item: &ReviewListItem,
    ) -> Vec<Line<'static>> {
        let selected = self.selected_idx == visible_idx;
        let marker = if selected { "›".cyan() } else { " ".into() };
        let mut prefix_spans = vec![marker, " ".into()];
        if self.search.is_none() {
            prefix_spans.push(format!("{}. ", visible_idx + 1).into());
        }
        let initial_indent: Line<'static> = prefix_spans.into();
        let prefix_width = initial_indent.width();
        let subsequent_indent: Line<'static> = " ".repeat(prefix_width).into();
        let label: Line<'static> = if selected {
            item.name.clone().bold().into()
        } else {
            item.name.clone().into()
        };
        let wrapped = word_wrap_line(
            &label,
            RtOptions::new(width.max(1))
                .initial_indent(initial_indent)
                .subsequent_indent(subsequent_indent),
        );
        let mut out = Vec::new();
        push_owned_lines(&wrapped, &mut out);
        out
    }

    fn footer_line(&self) -> Line<'static> {
        let back_label = if self.search.is_some() {
            "back"
        } else {
            "exit"
        };
        if self.search.is_some() {
            vec![
                "Enter".cyan(),
                " select   ".dim(),
                "type".cyan(),
                " search   ".dim(),
                "↑↓".cyan(),
                " move   ".dim(),
                "Esc".cyan(),
                format!(" {back_label}").dim(),
            ]
            .into()
        } else {
            vec![
                "Enter".cyan(),
                " select   ".dim(),
                "↑↓/jk".cyan(),
                " move   ".dim(),
                "Esc".cyan(),
                format!(" {back_label}").dim(),
            ]
            .into()
        }
    }

    fn item_height(&self, width: u16, visible_idx: usize, item: &ReviewListItem) -> u16 {
        let description_height = item.description.as_ref().map_or(0, |description| {
            wrap(
                description,
                Options::new(width.max(1) as usize)
                    .initial_indent("   ")
                    .subsequent_indent("   "),
            )
            .len() as u16
        });
        (self
            .item_label_lines(width.max(1) as usize, visible_idx, item)
            .len() as u16)
            .saturating_add(description_height)
    }

    fn move_up(&mut self) {
        let len = self.filtered_indices().len();
        if len == 0 {
            self.selected_idx = 0;
            self.scroll_top = 0;
            return;
        }
        if self.selected_idx == 0 {
            self.selected_idx = len.saturating_sub(1);
        } else {
            self.selected_idx -= 1;
        }
        self.ensure_visible();
    }

    fn move_down(&mut self) {
        let len = self.filtered_indices().len();
        if len == 0 {
            self.selected_idx = 0;
            self.scroll_top = 0;
            return;
        }
        self.selected_idx = (self.selected_idx + 1) % len;
        self.ensure_visible();
    }

    fn selected_action(&self) -> Option<ReviewListAction> {
        self.filtered_indices()
            .get(self.selected_idx)
            .and_then(|idx| self.items.get(*idx))
            .map(|item| item.action.clone())
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.filtered_indices()
            .into_iter()
            .skip(self.scroll_top)
            .take(MAX_POPUP_ROWS)
            .collect()
    }

    fn filtered_indices(&self) -> Vec<usize> {
        let Some(search) = &self.search else {
            return (0..self.items.len()).collect();
        };
        let query = search.query.trim().to_lowercase();
        if query.is_empty() {
            return (0..self.items.len()).collect();
        }
        self.items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let haystack = item.search_value.as_ref().unwrap_or(&item.name);
                haystack.to_lowercase().contains(&query).then_some(idx)
            })
            .collect()
    }

    fn update_search_query(&mut self, update: impl FnOnce(&mut String)) {
        let previously_selected_actual_idx =
            self.filtered_indices().get(self.selected_idx).copied();
        if let Some(search) = &mut self.search {
            update(&mut search.query);
        }
        self.reselect_after_filter_change(previously_selected_actual_idx);
    }

    fn reselect_after_filter_change(&mut self, previously_selected_actual_idx: Option<usize>) {
        let filtered = self.filtered_indices();
        self.selected_idx = previously_selected_actual_idx
            .and_then(|actual_idx| filtered.iter().position(|idx| *idx == actual_idx))
            .unwrap_or(0);
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        let len = self.filtered_indices().len();
        if len == 0 {
            self.selected_idx = 0;
            self.scroll_top = 0;
            return;
        }
        self.selected_idx = self.selected_idx.min(len.saturating_sub(1));
        if self.selected_idx < self.scroll_top {
            self.scroll_top = self.selected_idx;
        } else if self.selected_idx >= self.scroll_top.saturating_add(MAX_POPUP_ROWS) {
            self.scroll_top = self
                .selected_idx
                .saturating_add(1)
                .saturating_sub(MAX_POPUP_ROWS);
        }
    }
}

struct ReviewSearchState {
    query: String,
    placeholder: String,
}

struct ReviewListItem {
    name: String,
    description: Option<String>,
    search_value: Option<String>,
    action: ReviewListAction,
}

#[derive(Clone)]
enum ReviewListAction {
    OpenBranchPicker(PathBuf),
    OpenCommitPicker(PathBuf),
    OpenCustomPrompt,
    StartReview(ReviewRequest),
}

struct ReviewPromptScreen {
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
}

impl ReviewPromptScreen {
    fn new() -> Self {
        Self {
            textarea: TextArea::new(),
            textarea_state: RefCell::new(TextAreaState::default()),
        }
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) -> ReviewScreenAction {
        match key_event {
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                let instructions = self.textarea.text().trim().to_string();
                if instructions.is_empty() {
                    ReviewScreenAction::None
                } else {
                    ReviewScreenAction::Activate(ReviewListAction::StartReview(ReviewRequest {
                        target: ReviewTarget::Custom { instructions },
                        user_facing_hint: None,
                    }))
                }
            }
            other => {
                self.textarea.input(other);
                ReviewScreenAction::None
            }
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        self.textarea.insert_str(&pasted);
    }

    fn desired_height(&self, width: u16) -> u16 {
        5u16.saturating_add(self.input_height(width))
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let input_height = self.input_height(area.width);
        let lines: Vec<Line<'static>> = vec![
            "Custom review instructions".bold().into(),
            "Type instructions and press Enter".dim().into(),
            "".into(),
        ];
        Paragraph::new(lines).render(
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: 3,
            },
            buf,
        );

        let input_area = self.input_area(area, input_height);
        Block::default()
            .title("Instructions")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .render(input_area, buf);
        let textarea_rect = input_area.inset(Insets::vh(1, 1));
        StatefulWidgetRef::render_ref(
            &(&self.textarea),
            textarea_rect,
            buf,
            &mut self.textarea_state.borrow_mut(),
        );
        if self.textarea.text().is_empty() {
            Paragraph::new("Type instructions and press Enter".dim()).render(textarea_rect, buf);
        }

        let footer_y = input_area
            .y
            .saturating_add(input_area.height)
            .saturating_add(1);
        if footer_y < area.y.saturating_add(area.height) {
            Paragraph::new(Line::from(vec![
                "Enter".cyan(),
                " submit   ".dim(),
                "Shift+Enter".cyan(),
                " newline   ".dim(),
                "Esc".cyan(),
                " back".dim(),
            ]))
            .render(
                Rect {
                    x: area.x,
                    y: footer_y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let input_area = self.input_area(area, self.input_height(area.width));
        let textarea_rect = input_area.inset(Insets::vh(1, 1));
        let state = *self.textarea_state.borrow();
        self.textarea.cursor_pos_with_state(textarea_rect, state)
    }

    fn input_height(&self, width: u16) -> u16 {
        let textarea_width = width.saturating_sub(2).max(1);
        self.textarea
            .desired_height(textarea_width)
            .saturating_add(2)
            .clamp(3, 10)
    }

    fn input_area(&self, area: Rect, input_height: u16) -> Rect {
        Rect {
            x: area.x,
            y: area.y.saturating_add(3),
            width: area.width,
            height: input_height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use tokio::sync::mpsc::unbounded_channel;

    use crate::test_backend::VT100Backend;

    fn make_modal() -> (ReviewModal, tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = unbounded_channel();
        (
            ReviewModal::new(PathBuf::from("/tmp"), AppEventSender::new(tx)),
            rx,
        )
    }

    fn assert_next_branch_review(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        branch: &str,
    ) {
        match rx.try_recv() {
            Ok(AppEvent::StartReview { review_request }) => {
                assert_eq!(
                    review_request,
                    ReviewRequest {
                        target: ReviewTarget::BaseBranch {
                            branch: branch.to_string(),
                        },
                        user_facing_hint: None,
                    }
                );
            }
            other => panic!("unexpected app event: {other:?}"),
        }
    }

    fn line_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn renders_centered_review_modal() {
        let (modal, _rx) = make_modal();
        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Select a review preset"));
        assert!(rendered.contains("Review against a base branch (PR Style)"));
        assert!(!rendered.contains("   (PR Style)"));
        assert!(rendered.contains("Review uncommitted changes"));
        assert!(rendered.contains("Review a commit"));
        assert!(rendered.contains("Custom review instructions"));
        assert!(rendered.contains("↑↓/jk"));
    }

    #[test]
    fn ctrl_c_exits_modal() {
        let (mut modal, _rx) = make_modal();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ReviewModalAction::Exit
        );
    }

    #[test]
    fn ctrl_d_exits_modal() {
        let (mut modal, _rx) = make_modal();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            ReviewModalAction::Exit
        );
    }

    #[test]
    fn esc_exits_parent_menu() {
        let (mut modal, _rx) = make_modal();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );
    }

    #[test]
    fn custom_prompt_escape_returns_to_parent_then_exits() {
        let (mut modal, _rx) = make_modal();
        modal.show_custom_prompt();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ReviewModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );
    }

    #[test]
    fn custom_prompt_submit_sends_review_event() {
        let (mut modal, mut rx) = make_modal();
        modal.show_custom_prompt();
        modal.handle_paste("  please audit dependencies  ".to_string());

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );

        match rx.try_recv() {
            Ok(AppEvent::StartReview { review_request }) => {
                assert_eq!(
                    review_request,
                    ReviewRequest {
                        target: ReviewTarget::Custom {
                            instructions: "please audit dependencies".to_string(),
                        },
                        user_facing_hint: None,
                    }
                );
            }
            other => panic!("unexpected app event: {other:?}"),
        }
    }

    #[test]
    fn digit_shortcut_selects_root_preset_item() {
        let (mut modal, mut rx) = make_modal();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );

        match rx.try_recv() {
            Ok(AppEvent::StartReview { review_request }) => {
                assert_eq!(
                    review_request,
                    ReviewRequest {
                        target: ReviewTarget::UncommittedChanges,
                        user_facing_hint: None,
                    }
                );
            }
            other => panic!("unexpected app event: {other:?}"),
        }
    }

    #[test]
    fn commit_picker_shows_subjects_without_timestamps() {
        let (mut modal, _rx) = make_modal();
        modal.push_commit_picker_with_entries(vec![
            adam_agent::git_info::CommitLogEntry {
                sha: "1111111deadbeef".to_string(),
                timestamp: 0,
                subject: "Add new feature X".to_string(),
            },
            adam_agent::git_info::CommitLogEntry {
                sha: "2222222cafebabe".to_string(),
                timestamp: 0,
                subject: "Fix bug Y".to_string(),
            },
        ]);

        let mut terminal = Terminal::new(VT100Backend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Add new feature X"));
        assert!(rendered.contains("Fix bug Y"));
        let lowered = rendered.to_lowercase();
        assert!(
            !lowered.contains("ago")
                && !lowered.contains(" second")
                && !lowered.contains(" minute")
                && !lowered.contains(" hour")
                && !lowered.contains(" day"),
            "expected no relative time in commit picker output: {rendered:?}"
        );
    }

    #[test]
    fn commit_picker_typing_filters_results() {
        let (mut modal, _rx) = make_modal();
        modal.push_commit_picker_with_entries(vec![
            adam_agent::git_info::CommitLogEntry {
                sha: "1111111deadbeef".to_string(),
                timestamp: 0,
                subject: "Add new feature X".to_string(),
            },
            adam_agent::git_info::CommitLogEntry {
                sha: "2222222cafebabe".to_string(),
                timestamp: 0,
                subject: "Fix bug Y".to_string(),
            },
        ]);
        modal.handle_key_event(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT));

        let mut terminal = Terminal::new(VT100Backend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Fix bug Y"));
        assert!(!rendered.contains("Add new feature X"));
    }

    #[test]
    fn searchable_branch_picker_accepts_lowercase_j_and_k() {
        let (mut modal, _rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec!["jz/fix-login".to_string(), "z/fix-login".to_string()],
        );
        modal.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        modal.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));

        let mut terminal = Terminal::new(VT100Backend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("main -> jz/fix-login"));
        assert!(!rendered.contains("main -> z/fix-login"));

        let (mut modal, _rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec!["tk/fix-login".to_string(), "t/fix-login".to_string()],
        );
        modal.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        modal.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));

        let mut terminal = Terminal::new(VT100Backend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("main -> tk/fix-login"));
        assert!(!rendered.contains("main -> t/fix-login"));
    }

    #[test]
    fn searchable_branch_picker_supports_ctrl_p_ctrl_n_navigation() {
        let (mut modal, mut rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec![
                "feature/a".to_string(),
                "feature/b".to_string(),
                "feature/c".to_string(),
            ],
        );

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            ReviewModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );
        assert_next_branch_review(&mut rx, "feature/b");

        let (mut modal, mut rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec![
                "feature/a".to_string(),
                "feature/b".to_string(),
                "feature/c".to_string(),
            ],
        );

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            ReviewModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );
        assert_next_branch_review(&mut rx, "feature/c");
    }

    #[test]
    fn searchable_branch_picker_supports_ctrl_p_ctrl_n_fallback_chars() {
        let (mut modal, mut rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec![
                "feature/a".to_string(),
                "feature/b".to_string(),
                "feature/c".to_string(),
            ],
        );

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('\u{000e}'), KeyModifiers::NONE)),
            ReviewModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );
        assert_next_branch_review(&mut rx, "feature/b");

        let (mut modal, mut rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec![
                "feature/a".to_string(),
                "feature/b".to_string(),
                "feature/c".to_string(),
            ],
        );

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('\u{0010}'), KeyModifiers::NONE)),
            ReviewModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );
        assert_next_branch_review(&mut rx, "feature/c");
    }

    #[test]
    fn search_refinement_preserves_selected_matching_branch() {
        let (mut modal, mut rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec![
                "feature/api-first".to_string(),
                "feature/api-second".to_string(),
                "feature/ui".to_string(),
            ],
        );
        modal.handle_paste("feature".to_string());
        modal.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        modal.handle_paste("/api".to_string());

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );
        assert_next_branch_review(&mut rx, "feature/api-second");
    }

    #[test]
    fn search_refinement_falls_back_to_first_match_when_selection_drops_out() {
        let (mut modal, mut rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec![
                "feature/api-first".to_string(),
                "feature/api-second".to_string(),
                "feature/ui".to_string(),
            ],
        );
        modal.handle_paste("feature".to_string());
        modal.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        modal.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        modal.handle_paste("/api".to_string());

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );
        assert_next_branch_review(&mut rx, "feature/api-first");
    }

    #[test]
    fn root_preset_keeps_j_navigation() {
        let (mut modal, mut rx) = make_modal();

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            ReviewModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReviewModalAction::Exit
        );

        match rx.try_recv() {
            Ok(AppEvent::StartReview { review_request }) => {
                assert_eq!(
                    review_request,
                    ReviewRequest {
                        target: ReviewTarget::UncommittedChanges,
                        user_facing_hint: None,
                    }
                );
            }
            other => panic!("unexpected app event: {other:?}"),
        }
    }

    #[test]
    fn branch_picker_formats_current_to_target_branch() {
        let (mut modal, _rx) = make_modal();
        modal.push_branch_picker_with_entries("main".to_string(), vec!["feature/x".to_string()]);

        let mut terminal = Terminal::new(VT100Backend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        assert!(terminal.backend().to_string().contains("main -> feature/x"));
    }

    #[test]
    fn branch_picker_wraps_long_branch_labels() {
        let screen = ReviewListScreen::branches(
            "main".to_string(),
            vec!["feature/really-long-review-modal-branch-name-with-distinct-suffix".to_string()],
        );

        let lines = screen.item_label_lines(24, 0, &screen.items[0]);

        assert!(lines.len() > 1);
        assert!(line_text(&lines).contains("distinct-suffix"));
    }

    #[test]
    fn commit_picker_wraps_long_commit_subjects() {
        let screen = ReviewListScreen::commits(vec![adam_agent::git_info::CommitLogEntry {
            sha: "1111111deadbeef".to_string(),
            timestamp: 0,
            subject: "Implement really long review modal commit subject with distinct suffix"
                .to_string(),
        }]);

        let lines = screen.item_label_lines(24, 0, &screen.items[0]);

        assert!(lines.len() > 1);
        assert!(line_text(&lines).contains("suffix"));
    }

    #[test]
    fn desired_height_accounts_for_wrapped_review_items() {
        let short = ReviewListScreen::branches("main".to_string(), vec!["feature/x".to_string()]);
        let long = ReviewListScreen::branches(
            "main".to_string(),
            vec!["feature/really-long-review-modal-branch-name-with-distinct-suffix".to_string()],
        );

        assert!(long.desired_height(24) > short.desired_height(24));
    }

    #[test]
    fn searchable_paste_updates_query() {
        let (mut modal, _rx) = make_modal();
        modal.push_branch_picker_with_entries(
            "main".to_string(),
            vec!["feature/x".to_string(), "release/y".to_string()],
        );
        modal.handle_paste("release".to_string());

        let mut terminal = Terminal::new(VT100Backend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("main -> release/y"));
        assert!(!rendered.contains("main -> feature/x"));
    }
}
