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
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::render::Insets;
use crate::render::RectExt as _;

pub(crate) struct ReviewModal {
    view_stack: Vec<Box<dyn BottomPaneView>>,
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
            if self.view_stack.len() <= 1 {
                return ReviewModalAction::Exit;
            }
            if let Some(view) = self.view_stack.last_mut() {
                match view.on_ctrl_c() {
                    CancellationEvent::Handled if view.is_complete() => {
                        self.view_stack.pop();
                    }
                    CancellationEvent::Handled => {}
                    CancellationEvent::NotHandled => view.handle_key_event(key_event),
                }
            }
            return ReviewModalAction::None;
        }

        let Some(view) = self.view_stack.last_mut() else {
            return ReviewModalAction::Exit;
        };
        view.handle_key_event(key_event);
        if view.is_complete() {
            self.view_stack.clear();
            ReviewModalAction::Exit
        } else {
            ReviewModalAction::None
        }
    }

    pub(crate) fn handle_paste(&mut self, pasted: String) {
        if let Some(view) = self.view_stack.last_mut() {
            view.handle_paste(pasted);
        }
    }

    pub(crate) async fn show_branch_picker(&mut self, cwd: &Path) {
        let branches = local_git_branches(cwd).await;
        let current_branch = current_branch_name(cwd)
            .await
            .unwrap_or_else(|| "(detached HEAD)".to_string());
        let items = branch_items(current_branch, branches);
        self.push_selection_view(SelectionViewParams {
            title: Some("Select a base branch".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            ..Default::default()
        });
    }

    pub(crate) async fn show_commit_picker(&mut self, cwd: &Path) {
        let commits = adam_agent::git_info::recent_commits(cwd, 100).await;
        self.push_commit_picker_with_entries(commits);
    }

    pub(crate) fn show_custom_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Custom review instructions".to_string(),
            "Type instructions and press Enter".to_string(),
            None,
            Box::new(move |prompt: String| {
                let trimmed = prompt.trim().to_string();
                if trimmed.is_empty() {
                    return;
                }
                tx.send(AppEvent::StartReview {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Custom {
                            instructions: trimmed,
                        },
                        user_facing_hint: None,
                    },
                });
            }),
        );
        self.view_stack.push(Box::new(view));
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

        if let Some(view) = self.view_stack.last() {
            view.render(content_area, buf);
        }
    }

    pub(crate) fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let modal_area = self.modal_area(area);
        let inner_area = modal_area.inset(Insets::vh(1, 1));
        let content_area = inner_area.inset(Insets::vh(1, 2));
        self.view_stack
            .last()
            .and_then(|view| view.cursor_pos(content_area))
    }

    fn push_review_preset_view(&mut self, cwd: PathBuf) {
        let mut items: Vec<SelectionItem> = Vec::new();

        items.push(SelectionItem {
            name: "Review against a base branch".to_string(),
            description: Some("(PR Style)".into()),
            actions: vec![Box::new({
                let cwd = cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewBranchPicker(cwd.clone()));
                }
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Review uncommitted changes".to_string(),
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::StartReview {
                    review_request: ReviewRequest {
                        target: ReviewTarget::UncommittedChanges,
                        user_facing_hint: None,
                    },
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Review a commit".to_string(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenReviewCommitPicker(cwd.clone()));
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Custom review instructions".to_string(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenReviewCustomPrompt);
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        self.push_selection_view(SelectionViewParams {
            title: Some("Select a review preset".into()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    fn push_selection_view(&mut self, params: SelectionViewParams) {
        self.view_stack.push(Box::new(ListSelectionView::new(
            params,
            self.app_event_tx.clone(),
        )));
    }

    fn push_commit_picker_with_entries(
        &mut self,
        entries: Vec<adam_agent::git_info::CommitLogEntry>,
    ) {
        self.push_selection_view(SelectionViewParams {
            title: Some("Select a commit to review".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items: commit_items(entries),
            is_searchable: true,
            search_placeholder: Some("Type to search commits".to_string()),
            ..Default::default()
        });
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
            .map(|view| view.desired_height(content_width))
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

fn branch_items(current_branch: String, branches: Vec<String>) -> Vec<SelectionItem> {
    let mut items: Vec<SelectionItem> = Vec::with_capacity(branches.len());
    for option in branches {
        let branch = option.clone();
        items.push(SelectionItem {
            name: format!("{current_branch} -> {branch}"),
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::StartReview {
                    review_request: ReviewRequest {
                        target: ReviewTarget::BaseBranch {
                            branch: branch.clone(),
                        },
                        user_facing_hint: None,
                    },
                });
            })],
            dismiss_on_select: true,
            search_value: Some(option),
            ..Default::default()
        });
    }
    items
}

fn commit_items(entries: Vec<adam_agent::git_info::CommitLogEntry>) -> Vec<SelectionItem> {
    let mut items: Vec<SelectionItem> = Vec::with_capacity(entries.len());
    for entry in entries {
        let subject = entry.subject.clone();
        let sha = entry.sha.clone();
        let search_val = format!("{subject} {sha}");

        items.push(SelectionItem {
            name: subject.clone(),
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::StartReview {
                    review_request: ReviewRequest {
                        target: ReviewTarget::Commit {
                            sha: sha.clone(),
                            title: Some(subject.clone()),
                        },
                        user_facing_hint: None,
                    },
                });
            })],
            dismiss_on_select: true,
            search_value: Some(search_val),
            ..Default::default()
        });
    }
    items
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

    #[test]
    fn renders_centered_review_modal() {
        let (modal, _rx) = make_modal();
        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Select a review preset"));
        assert!(rendered.contains("Review against a base branch"));
        assert!(rendered.contains("Review uncommitted changes"));
        assert!(rendered.contains("Review a commit"));
        assert!(rendered.contains("Custom review instructions"));
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
    fn branch_picker_formats_current_to_target_branch() {
        let (mut modal, _rx) = make_modal();
        modal.push_selection_view(SelectionViewParams {
            title: Some("Select a base branch".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items: branch_items("main".to_string(), vec!["feature/x".to_string()]),
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            ..Default::default()
        });

        let mut terminal = Terminal::new(VT100Backend::new(80, 24)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        assert!(terminal.backend().to_string().contains("main -> feature/x"));
    }
}
