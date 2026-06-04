use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::RwLock;
use std::sync::RwLockReadGuard;
use std::sync::RwLockWriteGuard;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use textwrap::Options as WrapOptions;

use crate::product::tui_app::app_event::AppEvent;
use crate::product::tui_app::app_event_sender::AppEventSender;
use crate::product::tui_app::provider_config::ApiKeyInputState;
use crate::product::tui_app::provider_config::ApiProviderDialect;
use crate::product::tui_app::provider_config::ApiProviderWizardStep;
use crate::product::tui_app::provider_config::CustomProviderConfig;
use crate::product::tui_app::provider_config::current_step_value_mut;
use crate::product::tui_app::provider_config::persist_custom_provider_config;
use crate::product::tui_app::provider_config::snapshot_custom_provider_config;
use crate::product::tui_app::provider_config::validate_current_step;
use crate::product::tui_app::render::renderable::Renderable;
use crate::product::tui_app::tui::FrameRequester;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::standard_popup_hint_line;
use super::textarea::TextArea;
use super::textarea::TextAreaState;

pub(crate) struct ProviderConfigView {
    state: Arc<RwLock<ApiKeyInputState>>,
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
    complete: bool,
    lha_home: PathBuf,
    app_event_tx: AppEventSender,
    request_frame: FrameRequester,
}

impl ProviderConfigView {
    pub(crate) fn new(
        lha_home: PathBuf,
        app_event_tx: AppEventSender,
        request_frame: FrameRequester,
    ) -> Self {
        let mut textarea = TextArea::new();
        textarea.set_text_clearing_elements("");
        Self {
            state: Arc::new(RwLock::new(ApiKeyInputState::default())),
            textarea,
            textarea_state: RefCell::new(TextAreaState::default()),
            complete: false,
            lha_home,
            app_event_tx,
            request_frame,
        }
    }

    fn sync_textarea_from_state(&mut self) {
        let state = read_state(&self.state);
        let text = match state.step {
            ApiProviderWizardStep::ProviderId => state.provider_id.clone(),
            ApiProviderWizardStep::ConversationDialect => String::new(),
            ApiProviderWizardStep::BaseUrl => state.base_url.clone(),
            ApiProviderWizardStep::ApiKey => state.api_key.clone(),
            ApiProviderWizardStep::Model => state.model.clone(),
            ApiProviderWizardStep::ContextWindow => state.model_context_window.clone(),
        };
        drop(state);
        self.textarea.set_text_clearing_elements(&text);
        self.textarea_state.replace(TextAreaState::default());
    }

    fn update_state_from_textarea(&self, state: &mut ApiKeyInputState) {
        if let Some(field) = current_step_value_mut(state) {
            *field = self.textarea.text().to_string();
        }
    }

    fn start_save(&mut self, config: CustomProviderConfig) {
        {
            let mut state = write_state(&self.state);
            state.validating = true;
            state.error = None;
        }

        let state = Arc::clone(&self.state);
        let lha_home = self.lha_home.clone();
        let app_event_tx = self.app_event_tx.clone();
        let request_frame = self.request_frame.clone();
        tokio::spawn(async move {
            match persist_custom_provider_config(&lha_home, &config).await {
                Ok(()) => {
                    app_event_tx.send(AppEvent::CustomProviderConfigured(config));
                }
                Err(err) => {
                    let mut state = state.write().unwrap_or_else(PoisonError::into_inner);
                    state.validating = false;
                    state.error = Some(err);
                    request_frame.schedule_frame();
                }
            }
        });
    }

    fn input_height(&self, area: Rect, state: &ApiKeyInputState) -> u16 {
        let inner_width = area.width.saturating_sub(2).max(1);
        match state.step {
            ApiProviderWizardStep::ConversationDialect => 5,
            _ => self
                .textarea
                .desired_height(inner_width)
                .clamp(1, 4)
                .saturating_add(2),
        }
    }

    fn layout_parts(&self, area: Rect, state: &ApiKeyInputState) -> ProviderConfigLayout {
        let input_height = self.input_height(area, state);
        let full_intro_height = intro_height(area.width, state, true);
        let compact_intro_height = intro_height(area.width, state, false);
        let full_height = full_intro_height
            .saturating_add(1)
            .saturating_add(input_height)
            .saturating_add(FOOTER_MIN_HEIGHT);
        let show_summary = area.height >= full_height;
        let intro_height = if show_summary {
            full_intro_height
        } else {
            compact_intro_height
        };
        ProviderConfigLayout {
            input_height,
            intro_height,
            show_summary,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProviderConfigLayout {
    input_height: u16,
    intro_height: u16,
    show_summary: bool,
}

const FOOTER_MIN_HEIGHT: u16 = 4;
const INTRO_COPY: &str =
    "This saves the provider to ~/.lha/models.json and selects the model for future sessions.";

fn read_state(state: &RwLock<ApiKeyInputState>) -> RwLockReadGuard<'_, ApiKeyInputState> {
    state.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_state(state: &RwLock<ApiKeyInputState>) -> RwLockWriteGuard<'_, ApiKeyInputState> {
    state.write().unwrap_or_else(PoisonError::into_inner)
}

fn intro_height(width: u16, state: &ApiKeyInputState, show_summary: bool) -> u16 {
    intro_lines(width, state, show_summary).len() as u16
}

fn intro_lines(width: u16, state: &ApiKeyInputState, show_summary: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = vec![
        vec!["> ".into(), "Configure a custom API provider".bold()].into(),
        "".into(),
        format!("  Step {}/6: {}", state.step.index(), state.step.title()).into(),
    ];
    lines.extend(wrapped_intro_copy(width));
    if show_summary {
        lines.push("".into());
        lines.extend(summary_lines(state));
    }
    if state.validating {
        lines.push("".into());
        lines.push(
            "  Validating provider settings and saving config.toml..."
                .cyan()
                .into(),
        );
    }
    lines
}

fn wrapped_intro_copy(width: u16) -> Vec<Line<'static>> {
    let options = WrapOptions::new(width.max(1) as usize)
        .initial_indent("  ")
        .subsequent_indent("  ");
    textwrap::wrap(INTRO_COPY, &options)
        .into_iter()
        .map(|line| line.to_string().into())
        .collect()
}

fn summary_lines(state: &ApiKeyInputState) -> Vec<Line<'static>> {
    vec![
        format!(
            "  Provider ID: {}",
            display_optional_value(&state.provider_id, "<not set>")
        )
        .into(),
        format!("  Dialect: {}", state.dialect.label()).into(),
        format!(
            "  Base URL: {}",
            display_optional_value(&state.base_url, "<not set>")
        )
        .into(),
        format!("  API key: {}", mask_secret(&state.api_key)).into(),
        format!(
            "  Model: {}",
            display_optional_value(&state.model, "<not set>")
        )
        .into(),
        format!(
            "  Context Window: {}",
            display_optional_value(&state.model_context_window, "<not set>")
        )
        .into(),
    ]
}

impl BottomPaneView for ProviderConfigView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        let mut config_to_save: Option<CustomProviderConfig> = None;
        let mut should_complete = false;
        let mut should_sync_textarea = false;
        let mut should_request_frame = false;

        {
            let mut state = write_state(&self.state);
            if state.validating {
                return;
            }

            self.update_state_from_textarea(&mut state);

            match key_event.code {
                KeyCode::Esc => {
                    if let Some(previous_step) = state.step.previous() {
                        state.step = previous_step;
                        state.error = None;
                        should_sync_textarea = true;
                    } else {
                        should_complete = true;
                    }
                    should_request_frame = true;
                }
                KeyCode::Enter => {
                    if let Err(err) = validate_current_step(&state) {
                        state.error = Some(err);
                        should_request_frame = true;
                    } else if let Some(next_step) = state.step.next() {
                        state.step = next_step;
                        state.error = None;
                        should_sync_textarea = true;
                        should_request_frame = true;
                    } else {
                        match snapshot_custom_provider_config(&state) {
                            Ok(config) => config_to_save = Some(config),
                            Err(err) => {
                                state.error = Some(err);
                                should_request_frame = true;
                            }
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k')
                    if state.step == ApiProviderWizardStep::ConversationDialect =>
                {
                    state.dialect = state.dialect.previous();
                    state.error = None;
                    should_request_frame = true;
                }
                KeyCode::Down | KeyCode::Char('j')
                    if state.step == ApiProviderWizardStep::ConversationDialect =>
                {
                    state.dialect = state.dialect.next();
                    state.error = None;
                    should_request_frame = true;
                }
                KeyCode::Left | KeyCode::Right
                    if state.step == ApiProviderWizardStep::ConversationDialect =>
                {
                    state.dialect.toggle();
                    state.error = None;
                    should_request_frame = true;
                }
                KeyCode::Char(c) if state.step == ApiProviderWizardStep::ConversationDialect => {
                    if let Some(dialect) = ApiProviderDialect::from_shortcut_digit(c)
                        .or_else(|| ApiProviderDialect::from_shortcut_letter(c))
                    {
                        state.dialect = dialect;
                        state.error = None;
                        should_request_frame = true;
                    }
                }
                _ if state.step != ApiProviderWizardStep::ConversationDialect => {
                    if key_event
                        .modifiers
                        .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SUPER)
                    {
                        return;
                    }
                    self.textarea.input(key_event);
                    self.update_state_from_textarea(&mut state);
                    state.error = None;
                    should_request_frame = true;
                }
                _ => {}
            }
        }

        if should_sync_textarea {
            self.sync_textarea_from_state();
        }
        if should_complete {
            self.complete = true;
        }
        if let Some(config) = config_to_save {
            self.start_save(config);
            self.request_frame.schedule_frame();
        } else if should_request_frame {
            self.request_frame.schedule_frame();
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        let pasted = pasted.trim();
        if pasted.is_empty() {
            return false;
        }

        let mut state = write_state(&self.state);
        if state.validating || state.step == ApiProviderWizardStep::ConversationDialect {
            return true;
        }

        self.textarea.set_text_clearing_elements(pasted);
        self.textarea_state.replace(TextAreaState::default());
        self.update_state_from_textarea(&mut state);
        state.error = None;
        drop(state);
        self.request_frame.schedule_frame();
        true
    }
}

impl Renderable for ProviderConfigView {
    fn desired_height(&self, width: u16) -> u16 {
        let state = read_state(&self.state);
        let input_height = self.input_height(
            Rect {
                x: 0,
                y: 0,
                width,
                height: 0,
            },
            &state,
        );
        intro_height(width, &state, true) + input_height + 5
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let state = read_state(&self.state).clone();
        let layout = self.layout_parts(area, &state);
        let [intro_area, _spacer_area, input_area, footer_area] = Layout::vertical([
            Constraint::Length(layout.intro_height),
            Constraint::Length(1),
            Constraint::Length(layout.input_height),
            Constraint::Min(FOOTER_MIN_HEIGHT),
        ])
        .areas(area);

        Paragraph::new(intro_lines(area.width, &state, layout.show_summary))
            .wrap(Wrap { trim: false })
            .render(intro_area, buf);

        match state.step {
            ApiProviderWizardStep::ConversationDialect => {
                let lines = ApiProviderDialect::all()
                    .into_iter()
                    .enumerate()
                    .map(|(idx, option)| {
                        let selected = option == state.dialect;
                        let prefix = if selected { ">" } else { " " };
                        if selected {
                            vec![
                                format!("{prefix} {}. ", idx + 1).cyan().dim(),
                                option.label().cyan(),
                            ]
                            .into()
                        } else {
                            format!("  {}. {}", idx + 1, option.label()).into()
                        }
                    })
                    .collect::<Vec<Line>>();

                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .title(state.step.title())
                            .borders(Borders::ALL)
                            .border_type(BorderType::Rounded)
                            .border_style(Style::default().fg(Color::Cyan)),
                    )
                    .render(input_area, buf);
            }
            _ => {
                Block::default()
                    .title(state.step.title())
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .render(input_area, buf);
                let textarea_rect = Rect {
                    x: input_area.x.saturating_add(1),
                    y: input_area.y.saturating_add(1),
                    width: input_area.width.saturating_sub(2),
                    height: input_area.height.saturating_sub(2),
                };
                StatefulWidgetRef::render_ref(
                    &(&self.textarea),
                    textarea_rect,
                    buf,
                    &mut self.textarea_state.borrow_mut(),
                );
                if self.textarea.text().is_empty() {
                    Paragraph::new(Line::from(state.step.placeholder().dim()))
                        .render(textarea_rect, buf);
                }
            }
        }

        let mut footer_lines: Vec<Line> = vec![
            if state.validating {
                "  Saving...".dim().into()
            } else if state.step.next().is_none() {
                "  Press Enter to validate and save".dim().into()
            } else {
                "  Press Enter to continue".dim().into()
            },
            if state.step == ApiProviderWizardStep::ConversationDialect {
                "  Press 1/2/3 or Up/Down to choose".dim().into()
            } else {
                "  Type or paste to edit".dim().into()
            },
            standard_popup_hint_line(),
        ];
        if let Some(error) = &state.error {
            footer_lines.push("".into());
            footer_lines.push(error.as_str().red().into());
        }
        Paragraph::new(footer_lines)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let state = read_state(&self.state);
        if state.step == ApiProviderWizardStep::ConversationDialect || state.validating {
            return None;
        }

        let layout = self.layout_parts(area, &state);
        let textarea_rect = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(layout.intro_height).saturating_add(2),
            width: area.width.saturating_sub(2),
            height: layout.input_height.saturating_sub(2),
        };
        let textarea_state = *self.textarea_state.borrow();
        self.textarea
            .cursor_pos_with_state(textarea_rect, textarea_state)
    }
}

fn display_optional_value(value: &str, placeholder: &str) -> String {
    if value.trim().is_empty() {
        placeholder.to_string()
    } else {
        value.to_string()
    }
}

fn mask_secret(value: &str) -> String {
    if value.trim().is_empty() {
        "<not set>".to_string()
    } else {
        "********".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc::unbounded_channel;

    fn test_view() -> ProviderConfigView {
        let (tx, _rx) = unbounded_channel();
        ProviderConfigView::new(
            PathBuf::from("/tmp"),
            AppEventSender::new(tx),
            FrameRequester::test_dummy(),
        )
    }

    fn state_snapshot(view: &ProviderConfigView) -> ApiKeyInputState {
        view.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn row_text(buf: &Buffer, row: u16, width: u16) -> String {
        (0..width)
            .map(|col| buf[(col, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn rendered_lines(view: &ProviderConfigView, area: Rect) -> Vec<String> {
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        (0..area.height)
            .map(|row| row_text(&buf, row, area.width))
            .collect()
    }

    fn rendered_text(view: &ProviderConfigView, area: Rect) -> String {
        rendered_lines(view, area).join("\n")
    }

    #[test]
    fn esc_on_first_step_cancels_view() {
        let mut view = test_view();

        view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(view.is_complete());
    }

    #[test]
    fn enter_and_esc_follow_wizard_steps() {
        let mut view = test_view();
        assert!(view.handle_paste("custom_1".to_string()));

        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            state_snapshot(&view).step,
            ApiProviderWizardStep::ConversationDialect
        );

        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(state_snapshot(&view).step, ApiProviderWizardStep::BaseUrl);

        view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            state_snapshot(&view).step,
            ApiProviderWizardStep::ConversationDialect
        );
    }

    #[test]
    fn provider_wizard_reaches_optional_context_window_step() {
        let mut view = test_view();

        assert!(view.handle_paste("custom_1".to_string()));
        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(view.handle_paste("https://example.com/v1".to_string()));
        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(view.handle_paste("sk-test".to_string()));
        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(view.handle_paste("gpt-test".to_string()));
        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            state_snapshot(&view).step,
            ApiProviderWizardStep::ContextWindow
        );
    }

    #[test]
    fn paste_and_dialect_shortcuts_update_state() {
        let mut view = test_view();

        assert!(view.handle_paste(" custom_1 ".to_string()));
        assert_eq!(state_snapshot(&view).provider_id, "custom_1");

        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(state_snapshot(&view).dialect, ApiProviderDialect::Chat);

        view.handle_key_event(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert_eq!(state_snapshot(&view).dialect, ApiProviderDialect::Chat);

        view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        assert_eq!(state_snapshot(&view).dialect, ApiProviderDialect::Responses);

        view.handle_key_event(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(state_snapshot(&view).dialect, ApiProviderDialect::Messages);

        view.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert_eq!(state_snapshot(&view).dialect, ApiProviderDialect::Messages);

        view.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state_snapshot(&view).dialect, ApiProviderDialect::Chat);

        view.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state_snapshot(&view).dialect, ApiProviderDialect::Messages);
    }

    #[test]
    fn dialect_step_uses_three_single_line_options() {
        let view = test_view();
        {
            let mut state = write_state(&view.state);
            state.step = ApiProviderWizardStep::ConversationDialect;
        }

        let area = Rect::new(0, 0, 120, view.desired_height(120));
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        let lines = (0..area.height)
            .map(|row| row_text(&buf, row, area.width))
            .collect::<Vec<_>>();
        let dialect_row = lines
            .iter()
            .position(|line| line.starts_with("╭Dialect"))
            .expect("dialect border row");

        assert_eq!(lines[dialect_row + 1].contains("chat"), true);
        assert_eq!(lines[dialect_row + 2].contains("responses"), true);
        assert_eq!(lines[dialect_row + 3].contains("messages"), true);
        assert_eq!(
            lines
                .iter()
                .any(|line| line.contains("Compatible with chat completions style APIs")),
            false
        );
        assert_eq!(
            lines
                .iter()
                .any(|line| line.contains("Compatible with Responses API style backends")),
            false
        );
        assert_eq!(
            lines
                .iter()
                .any(|line| line.contains("Anthropic-compatible Messages API at /v1/messages")),
            false
        );
    }

    #[test]
    fn small_height_hides_summary_and_keeps_input() {
        let view = test_view();
        let area = Rect::new(0, 0, 80, 15);

        let rendered = rendered_text(&view, area);

        assert_eq!(rendered.contains("Provider ID:"), false);
        assert_eq!(rendered.contains("Dialect:"), false);
        assert_eq!(rendered.contains("Base URL:"), false);
        assert_eq!(rendered.contains("API key:"), false);
        assert_eq!(rendered.contains("Model:"), false);
        assert_eq!(rendered.contains("Context Window:"), false);
        assert_eq!(rendered.contains("╭Provider ID"), true);
        assert_eq!(rendered.contains("my-provider"), true);
        assert_eq!(rendered.contains("Press Enter to continue"), true);
    }

    #[test]
    fn comfortable_height_keeps_summary() {
        let view = test_view();
        let area = Rect::new(0, 0, 80, view.desired_height(80));

        let rendered = rendered_text(&view, area);

        assert_eq!(rendered.contains("Provider ID:"), true);
        assert_eq!(rendered.contains("Dialect:"), true);
        assert_eq!(rendered.contains("Base URL:"), true);
        assert_eq!(rendered.contains("API key:"), true);
        assert_eq!(rendered.contains("Model:"), true);
        assert_eq!(rendered.contains("Context Window:"), true);
    }

    #[test]
    fn cursor_pos_uses_compact_layout() {
        let view = test_view();
        let area = Rect::new(0, 0, 80, 15);
        let cursor_pos = view.cursor_pos(area).expect("cursor position");
        let lines = rendered_lines(&view, area);
        let input_row = lines
            .iter()
            .position(|line| line.contains("╭Provider ID"))
            .expect("input border row") as u16;

        assert_eq!(cursor_pos.1, input_row + 1);
    }
}
