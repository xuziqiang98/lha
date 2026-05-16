use adam_protocol::openai_models::ModelPreset;
use adam_protocol::openai_models::ReasoningEffort;
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
use strum::IntoEnumIterator;
use textwrap::Options;
use textwrap::wrap;

use crate::render::Insets;
use crate::render::RectExt as _;

#[derive(Debug, Clone)]
pub(crate) struct ModelSelectionModalContext {
    pub(crate) current_model: String,
    pub(crate) current_provider_id: String,
    pub(crate) effective_reasoning_effort: Option<ReasoningEffort>,
    pub(crate) custom_openai_base_url: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelSelectionModal {
    stage: ModelSelectionStage,
    selected_idx: usize,
    context: ModelSelectionModalContext,
}

#[derive(Debug, Clone)]
enum ModelSelectionStage {
    Quick {
        items: Vec<QuickModelItem>,
    },
    All {
        presets: Vec<ModelPreset>,
    },
    Reasoning {
        preset: Box<ModelPreset>,
        choices: Vec<EffortChoice>,
        default_choice: Option<ReasoningEffort>,
        highlight_choice: Option<ReasoningEffort>,
    },
}

#[derive(Debug, Clone)]
struct QuickModelItem {
    item: QuickModelItemKind,
    name: String,
    description: Option<String>,
    is_current: bool,
}

#[derive(Debug, Clone)]
enum QuickModelItemKind {
    Preset(Box<ModelPreset>),
    AllModels(Vec<ModelPreset>),
}

#[derive(Debug, Clone, Copy)]
struct EffortChoice {
    stored: Option<ReasoningEffort>,
    display: ReasoningEffort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelSelectionModalAction {
    None,
    Exit,
    PersistModelSelection {
        model: String,
        provider_id: Option<String>,
        effort: Option<ReasoningEffort>,
    },
}

impl ModelSelectionModal {
    pub(crate) fn new(
        presets: Vec<ModelPreset>,
        context: ModelSelectionModalContext,
    ) -> Option<Self> {
        let presets: Vec<ModelPreset> = presets
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .collect();
        if presets.is_empty() {
            return None;
        }

        let stage = Self::initial_stage(presets, &context);
        let selected_idx = Self::initial_selected_idx(&stage, &context);
        Some(Self {
            stage,
            selected_idx,
            context,
        })
    }

    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) -> ModelSelectionModalAction {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ModelSelectionModalAction::None;
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
                ModelSelectionModalAction::None
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
                ModelSelectionModalAction::None
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
                    && idx < self.item_count()
                {
                    self.selected_idx = idx;
                    return self.selected_action();
                }
                ModelSelectionModalAction::None
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
            } => ModelSelectionModalAction::Exit,
            _ => ModelSelectionModalAction::None,
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
        let mut lines = self.content_lines(width);
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

    fn initial_stage(
        presets: Vec<ModelPreset>,
        context: &ModelSelectionModalContext,
    ) -> ModelSelectionStage {
        let has_exact_provider_match = Self::has_exact_current_model_provider_match(
            &presets,
            context.current_model.as_str(),
            context.current_provider_id.as_str(),
        );
        let current_label = presets
            .iter()
            .find(|preset| {
                Self::is_current_model_preset_with_exact_provider_match(
                    preset,
                    has_exact_provider_match,
                    context.current_model.as_str(),
                    context.current_provider_id.as_str(),
                )
            })
            .map(|preset| preset.display_name.clone())
            .unwrap_or_else(|| context.current_model.clone());

        let (mut auto_presets, other_presets): (Vec<ModelPreset>, Vec<ModelPreset>) = presets
            .into_iter()
            .partition(|preset| Self::is_auto_model(preset.model.as_str()));
        if auto_presets.is_empty() {
            return ModelSelectionStage::All {
                presets: other_presets,
            };
        }

        auto_presets.sort_by_key(|preset| Self::auto_model_order(preset.model.as_str()));
        let mut items: Vec<QuickModelItem> = auto_presets
            .into_iter()
            .map(|preset| QuickModelItem {
                name: preset.display_name.clone(),
                description: (!preset.description.is_empty()).then_some(preset.description.clone()),
                is_current: Self::is_current_model_preset_with_exact_provider_match(
                    &preset,
                    has_exact_provider_match,
                    context.current_model.as_str(),
                    context.current_provider_id.as_str(),
                ),
                item: QuickModelItemKind::Preset(Box::new(preset)),
            })
            .collect();

        if !other_presets.is_empty() {
            let is_current = !items.iter().any(|item| item.is_current);
            items.push(QuickModelItem {
                item: QuickModelItemKind::AllModels(other_presets),
                name: "All models".to_string(),
                description: Some(format!(
                    "Choose a specific model and reasoning level (current: {current_label})"
                )),
                is_current,
            });
        }

        ModelSelectionStage::Quick { items }
    }

    fn content_lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let (title, subtitle) = match &self.stage {
            ModelSelectionStage::Quick { .. } => (
                "Select Model",
                "Pick a quick auto mode or browse all models.",
            ),
            ModelSelectionStage::All { .. } => (
                "Select Model and Effort",
                "Access legacy models by running adam -m <model_name> or in your config.toml",
            ),
            ModelSelectionStage::Reasoning { preset, .. } => {
                lines.push(
                    format!("Select Reasoning Level for {}", preset.model)
                        .bold()
                        .into(),
                );
                lines.push("".into());
                self.push_stage_items(&mut lines, width);
                return lines;
            }
        };

        lines.push(title.bold().into());
        lines.push(subtitle.dim().into());
        if let Some(base_url) = &self.context.custom_openai_base_url {
            lines.push(
                format!(
                    "Warning: OPENAI_BASE_URL is set to {base_url}. Selecting models may not be supported or work properly."
                )
                .red()
                .into(),
            );
        }
        lines.push("".into());
        self.push_stage_items(&mut lines, width);
        lines
    }

    fn push_stage_items(&self, lines: &mut Vec<Line<'static>>, width: usize) {
        match &self.stage {
            ModelSelectionStage::Quick { items } => {
                for (idx, item) in items.iter().enumerate() {
                    self.push_item_lines(
                        lines,
                        width,
                        idx,
                        item.name.as_str(),
                        item.description.as_deref(),
                        item.is_current,
                    );
                }
            }
            ModelSelectionStage::All { presets } => {
                let has_exact_provider_match = Self::has_exact_current_model_provider_match(
                    presets,
                    self.context.current_model.as_str(),
                    self.context.current_provider_id.as_str(),
                );
                for (idx, preset) in presets.iter().enumerate() {
                    let is_current = Self::is_current_model_preset_with_exact_provider_match(
                        preset,
                        has_exact_provider_match,
                        self.context.current_model.as_str(),
                        self.context.current_provider_id.as_str(),
                    );
                    let description =
                        (!preset.description.is_empty()).then_some(preset.description.as_str());
                    self.push_item_lines(
                        lines,
                        width,
                        idx,
                        preset.display_name.as_str(),
                        description,
                        is_current,
                    );
                }
            }
            ModelSelectionStage::Reasoning {
                preset,
                choices,
                default_choice,
                highlight_choice,
            } => {
                let warning_effort = Self::warning_effort(choices);
                let warn_for_model = Self::warn_for_model(preset.model.as_str());
                for (idx, choice) in choices.iter().enumerate() {
                    let effort = choice.display;
                    let mut name = Self::reasoning_effort_label(effort).to_string();
                    if choice.stored == *default_choice {
                        name.push_str(" (default)");
                    }

                    let description = choice
                        .stored
                        .and_then(|effort| {
                            preset
                                .supported_reasoning_efforts
                                .iter()
                                .find(|option| option.effort == effort)
                                .map(|option| option.description.as_str())
                        })
                        .filter(|text| !text.is_empty());
                    let warning = (warn_for_model && warning_effort == Some(effort)).then(|| {
                        let effort_label = Self::reasoning_effort_label(effort);
                        format!(
                            "⚠ {effort_label} reasoning effort can quickly consume Plus plan rate limits."
                        )
                    });
                    let combined_description = match (description, warning.as_deref()) {
                        (Some(description), Some(warning)) => {
                            Some(format!("{description}\n{warning}"))
                        }
                        (Some(description), None) => Some(description.to_string()),
                        (None, Some(warning)) => Some(warning.to_string()),
                        (None, None) => None,
                    };
                    let is_current = Self::is_current_model_for_selection(
                        preset.model.as_str(),
                        preset.model_provider_id.as_deref(),
                        self.context.current_model.as_str(),
                        self.context.current_provider_id.as_str(),
                    ) && choice.stored == *highlight_choice;
                    self.push_item_lines(
                        lines,
                        width,
                        idx,
                        name.as_str(),
                        combined_description.as_deref(),
                        is_current,
                    );
                }
            }
        }
    }

    fn push_item_lines(
        &self,
        lines: &mut Vec<Line<'static>>,
        width: usize,
        idx: usize,
        name: &str,
        description: Option<&str>,
        is_current: bool,
    ) {
        let selected = self.selected_idx == idx;
        let marker = if selected { "›".cyan() } else { " ".into() };
        let number = format!("{}. ", idx + 1);
        let label = if is_current {
            format!("{name} (current)")
        } else {
            name.to_string()
        };
        let label = if selected { label.bold() } else { label.into() };
        lines.push(vec![marker, " ".into(), number.into(), label].into());

        if let Some(description) = description {
            for segment in description.lines() {
                let wrapped = wrap(
                    segment,
                    Options::new(width)
                        .initial_indent("   ")
                        .subsequent_indent("   "),
                );
                for line in wrapped {
                    lines.push(line.into_owned().dim().into());
                }
            }
        }
    }

    fn selected_action(&mut self) -> ModelSelectionModalAction {
        match &self.stage {
            ModelSelectionStage::Quick { items } => {
                let Some(item) = items.get(self.selected_idx).cloned() else {
                    return ModelSelectionModalAction::None;
                };
                match item.item {
                    QuickModelItemKind::Preset(preset) => {
                        let effort = Some(preset.default_reasoning_effort);
                        Self::persist_action(*preset, effort)
                    }
                    QuickModelItemKind::AllModels(presets) => {
                        self.stage = ModelSelectionStage::All { presets };
                        self.selected_idx = Self::initial_selected_idx(&self.stage, &self.context);
                        ModelSelectionModalAction::None
                    }
                }
            }
            ModelSelectionStage::All { presets } => {
                let Some(preset) = presets.get(self.selected_idx).cloned() else {
                    return ModelSelectionModalAction::None;
                };
                let choices = Self::effort_choices(&preset);
                if choices.len() == 1 {
                    return Self::persist_action(preset, choices.first().and_then(|c| c.stored));
                }

                let default_choice =
                    Self::default_choice(&choices, preset.default_reasoning_effort);
                let is_current = Self::is_current_model_for_selection(
                    preset.model.as_str(),
                    preset.model_provider_id.as_deref(),
                    self.context.current_model.as_str(),
                    self.context.current_provider_id.as_str(),
                );
                let highlight_choice = if is_current {
                    self.context.effective_reasoning_effort
                } else {
                    default_choice
                };
                let selection_choice = highlight_choice.or(default_choice);
                self.selected_idx =
                    Self::initial_reasoning_idx(&choices, selection_choice).unwrap_or_default();
                self.stage = ModelSelectionStage::Reasoning {
                    preset: Box::new(preset),
                    choices,
                    default_choice,
                    highlight_choice,
                };
                ModelSelectionModalAction::None
            }
            ModelSelectionStage::Reasoning {
                preset, choices, ..
            } => {
                let Some(choice) = choices.get(self.selected_idx) else {
                    return ModelSelectionModalAction::None;
                };
                Self::persist_action((**preset).clone(), choice.stored)
            }
        }
    }

    fn persist_action(
        preset: ModelPreset,
        effort: Option<ReasoningEffort>,
    ) -> ModelSelectionModalAction {
        ModelSelectionModalAction::PersistModelSelection {
            model: preset.model,
            provider_id: preset.model_provider_id,
            effort,
        }
    }

    fn effort_choices(preset: &ModelPreset) -> Vec<EffortChoice> {
        let mut choices: Vec<EffortChoice> = Vec::new();
        for effort in ReasoningEffort::iter() {
            if preset
                .supported_reasoning_efforts
                .iter()
                .any(|option| option.effort == effort)
            {
                choices.push(EffortChoice {
                    stored: Some(effort),
                    display: effort,
                });
            }
        }
        if choices.is_empty() {
            choices.push(EffortChoice {
                stored: (preset.default_reasoning_effort != ReasoningEffort::None)
                    .then_some(preset.default_reasoning_effort),
                display: preset.default_reasoning_effort,
            });
        }
        choices
    }

    fn default_choice(
        choices: &[EffortChoice],
        default_effort: ReasoningEffort,
    ) -> Option<ReasoningEffort> {
        choices
            .iter()
            .any(|choice| choice.stored == Some(default_effort))
            .then_some(Some(default_effort))
            .flatten()
            .or_else(|| choices.iter().find_map(|choice| choice.stored))
            .or(Some(default_effort))
    }

    fn initial_reasoning_idx(
        choices: &[EffortChoice],
        selection_choice: Option<ReasoningEffort>,
    ) -> Option<usize> {
        choices
            .iter()
            .position(|choice| choice.stored == selection_choice)
            .or_else(|| {
                selection_choice
                    .and_then(|effort| choices.iter().position(|choice| choice.display == effort))
            })
    }

    fn warning_effort(choices: &[EffortChoice]) -> Option<ReasoningEffort> {
        if choices
            .iter()
            .any(|choice| choice.display == ReasoningEffort::XHigh)
        {
            Some(ReasoningEffort::XHigh)
        } else if choices
            .iter()
            .any(|choice| choice.display == ReasoningEffort::High)
        {
            Some(ReasoningEffort::High)
        } else {
            None
        }
    }

    fn warn_for_model(model: &str) -> bool {
        model.starts_with("gpt-5.1-codex")
            || model.starts_with("gpt-5.1-codex-max")
            || model.starts_with("gpt-5.2")
    }

    fn item_count(&self) -> usize {
        match &self.stage {
            ModelSelectionStage::Quick { items } => items.len(),
            ModelSelectionStage::All { presets } => presets.len(),
            ModelSelectionStage::Reasoning { choices, .. } => choices.len(),
        }
    }

    fn initial_selected_idx(
        stage: &ModelSelectionStage,
        context: &ModelSelectionModalContext,
    ) -> usize {
        match stage {
            ModelSelectionStage::Quick { items } => items
                .iter()
                .position(|item| item.is_current)
                .unwrap_or_default(),
            ModelSelectionStage::All { presets } => {
                let has_exact_provider_match = Self::has_exact_current_model_provider_match(
                    presets,
                    context.current_model.as_str(),
                    context.current_provider_id.as_str(),
                );
                presets
                    .iter()
                    .position(|preset| {
                        Self::is_current_model_preset_with_exact_provider_match(
                            preset,
                            has_exact_provider_match,
                            context.current_model.as_str(),
                            context.current_provider_id.as_str(),
                        )
                    })
                    .unwrap_or_default()
            }
            ModelSelectionStage::Reasoning { .. } => 0,
        }
    }

    fn move_up(&mut self) {
        if self.item_count() == 0 {
            return;
        }
        if self.selected_idx == 0 {
            self.selected_idx = self.item_count().saturating_sub(1);
        } else {
            self.selected_idx -= 1;
        }
    }

    fn move_down(&mut self) {
        let item_count = self.item_count();
        if item_count == 0 {
            return;
        }
        self.selected_idx = (self.selected_idx + 1) % item_count;
    }

    fn modal_area(&self, area: Rect) -> Rect {
        let width = area.width.saturating_sub(4).min(88).max(area.width.min(44));
        let content_width = width.saturating_sub(6).max(1) as usize;
        let desired_content_height = self.content_lines(content_width).len().saturating_add(1);
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

    fn is_auto_model(model: &str) -> bool {
        model.starts_with("codex-auto-")
    }

    fn auto_model_order(model: &str) -> usize {
        match model {
            "codex-auto-fast" => 0,
            "codex-auto-balanced" => 1,
            "codex-auto-thorough" => 2,
            _ => 3,
        }
    }

    fn reasoning_effort_label(effort: ReasoningEffort) -> &'static str {
        match effort {
            ReasoningEffort::None => "None",
            ReasoningEffort::Minimal => "Minimal",
            ReasoningEffort::Low => "Low",
            ReasoningEffort::Medium => "Medium",
            ReasoningEffort::High => "High",
            ReasoningEffort::XHigh => "Extra high",
        }
    }

    fn has_exact_current_model_provider_match(
        presets: &[ModelPreset],
        current_model: &str,
        current_provider_id: &str,
    ) -> bool {
        presets.iter().any(|candidate| {
            candidate.model == current_model
                && candidate.model_provider_id.as_deref() == Some(current_provider_id)
        })
    }

    fn is_current_model_preset_with_exact_provider_match(
        preset: &ModelPreset,
        has_exact_provider_match: bool,
        current_model: &str,
        current_provider_id: &str,
    ) -> bool {
        if !has_exact_provider_match {
            return Self::is_current_model_for_selection(
                preset.model.as_str(),
                preset.model_provider_id.as_deref(),
                current_model,
                current_provider_id,
            );
        }
        if current_model != preset.model {
            return false;
        }
        preset.model_provider_id.as_deref() == Some(current_provider_id)
    }

    fn is_current_model_for_selection(
        model: &str,
        provider_id: Option<&str>,
        current_model: &str,
        current_provider_id: &str,
    ) -> bool {
        if current_model != model {
            return false;
        }
        provider_id.is_none_or(|provider_id| current_provider_id == provider_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_backend::VT100Backend;
    use adam_protocol::openai_models::ReasoningEffortPreset;
    use pretty_assertions::assert_eq;
    use ratatui::Terminal;

    fn preset(model: &str, display_name: &str, efforts: Vec<ReasoningEffort>) -> ModelPreset {
        ModelPreset {
            id: model.to_string(),
            model: model.to_string(),
            model_provider_id: Some("openai".to_string()),
            display_name: display_name.to_string(),
            description: format!("{display_name} description"),
            default_reasoning_effort: efforts.first().copied().unwrap_or(ReasoningEffort::Medium),
            supported_reasoning_efforts: efforts
                .into_iter()
                .map(|effort| ReasoningEffortPreset {
                    effort,
                    description: format!("{effort} effort"),
                })
                .collect(),
            supports_personality: false,
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            supported_in_api: true,
        }
    }

    fn context(current_model: &str) -> ModelSelectionModalContext {
        ModelSelectionModalContext {
            current_model: current_model.to_string(),
            current_provider_id: "openai".to_string(),
            effective_reasoning_effort: Some(ReasoningEffort::High),
            custom_openai_base_url: None,
        }
    }

    #[test]
    fn renders_centered_quick_model_modal() {
        let modal = ModelSelectionModal::new(
            vec![
                preset("codex-auto-fast", "Fast", vec![ReasoningEffort::Low]),
                preset("gpt-5.2", "GPT 5.2", vec![ReasoningEffort::Medium]),
            ],
            context("codex-auto-fast"),
        )
        .expect("modal");

        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Select Model"));
        assert!(rendered.contains("Fast (current)"));
        assert!(rendered.contains("All models"));
    }

    #[test]
    fn quick_model_selection_persists_default_effort() {
        let mut modal = ModelSelectionModal::new(
            vec![preset(
                "codex-auto-fast",
                "Fast",
                vec![ReasoningEffort::Low],
            )],
            context("gpt-5"),
        )
        .expect("modal");

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelSelectionModalAction::PersistModelSelection {
                model: "codex-auto-fast".to_string(),
                provider_id: Some("openai".to_string()),
                effort: Some(ReasoningEffort::Low),
            }
        );
    }

    #[test]
    fn all_models_item_switches_stage() {
        let mut modal = ModelSelectionModal::new(
            vec![
                preset("codex-auto-fast", "Fast", vec![ReasoningEffort::Low]),
                preset("gpt-5.2", "GPT 5.2", vec![ReasoningEffort::Medium]),
            ],
            context("codex-auto-fast"),
        )
        .expect("modal");

        modal.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelSelectionModalAction::None
        );

        let mut terminal = Terminal::new(VT100Backend::new(100, 32)).expect("terminal");
        terminal
            .draw(|frame| modal.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        assert!(
            terminal
                .backend()
                .to_string()
                .contains("Select Model and Effort")
        );
    }

    #[test]
    fn all_models_multiple_efforts_opens_reasoning_stage_then_persists() {
        let mut modal = ModelSelectionModal::new(
            vec![preset(
                "gpt-5.2",
                "GPT 5.2",
                vec![ReasoningEffort::Medium, ReasoningEffort::High],
            )],
            context("gpt-5.2"),
        )
        .expect("modal");

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelSelectionModalAction::None
        );
        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ModelSelectionModalAction::PersistModelSelection {
                model: "gpt-5.2".to_string(),
                provider_id: Some("openai".to_string()),
                effort: Some(ReasoningEffort::High),
            }
        );
    }

    #[test]
    fn esc_exits_modal() {
        let mut modal = ModelSelectionModal::new(
            vec![preset(
                "codex-auto-fast",
                "Fast",
                vec![ReasoningEffort::Low],
            )],
            context("gpt-5"),
        )
        .expect("modal");

        assert_eq!(
            modal.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ModelSelectionModalAction::Exit
        );
    }
}
