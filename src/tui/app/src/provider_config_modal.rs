use std::path::PathBuf;

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

use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::provider_config_view::ProviderConfigView;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::Renderable;
use crate::tui::FrameRequester;

pub(crate) struct ProviderConfigModal {
    view: ProviderConfigView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderConfigModalAction {
    None,
    Exit,
}

impl ProviderConfigModal {
    pub(crate) fn new(
        adam_home: PathBuf,
        app_event_tx: AppEventSender,
        request_frame: FrameRequester,
    ) -> Self {
        Self {
            view: ProviderConfigView::new(adam_home, app_event_tx, request_frame),
        }
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> ProviderConfigModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ProviderConfigModalAction::None;
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
            return ProviderConfigModalAction::Exit;
        }

        self.view.handle_key_event(key_event);
        if self.view.is_complete() {
            ProviderConfigModalAction::Exit
        } else {
            ProviderConfigModalAction::None
        }
    }

    pub(crate) fn handle_paste(&mut self, pasted: String) {
        self.view.handle_paste(pasted);
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

        self.view.render(content_area, buf);
    }

    pub(crate) fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let modal_area = self.modal_area(area);
        let inner_area = modal_area.inset(Insets::vh(1, 1));
        let content_area = inner_area.inset(Insets::vh(1, 2));
        self.view.cursor_pos(content_area)
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
        let desired_content_height = self.view.desired_height(content_width);
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_backend::VT100Backend;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use tempfile::TempDir;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn renders_centered_provider_config_modal() {
        let adam_home = TempDir::new().expect("temp home");
        let (tx, _rx) = unbounded_channel();
        let modal = ProviderConfigModal::new(
            adam_home.path().to_path_buf(),
            AppEventSender::new(tx),
            FrameRequester::test_dummy(),
        );

        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Configure a custom API provider"));
        assert!(rendered.contains("Step 1/6: Provider ID"));
        assert!(rendered.contains("~/.adam/models.json"));
    }

    #[test]
    fn ctrl_c_exits_modal() {
        let adam_home = TempDir::new().expect("temp home");
        let (tx, _rx) = unbounded_channel();
        let mut modal = ProviderConfigModal::new(
            adam_home.path().to_path_buf(),
            AppEventSender::new(tx),
            FrameRequester::test_dummy(),
        );

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ProviderConfigModalAction::Exit
        );
    }
}
