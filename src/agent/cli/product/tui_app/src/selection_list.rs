use crate::product::tui_app::render::renderable::Renderable;
use crate::product::tui_app::render::renderable::RowRenderable;
use ratatui::style::Style;
use ratatui::style::Styled as _;
use ratatui::style::Stylize as _;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use unicode_width::UnicodeWidthStr;

pub(crate) fn selection_option_row(
    index: usize,
    label: String,
    is_selected: bool,
) -> Box<dyn Renderable> {
    selection_option_row_with_dim(index, label, is_selected, false)
}

pub(crate) fn compact_selection_option_row(
    index: usize,
    label: String,
    is_selected: bool,
) -> Box<dyn Renderable> {
    selection_option_row_with_label_style(index, label, is_selected, false, true)
}

pub(crate) fn selection_option_row_with_dim(
    index: usize,
    label: String,
    is_selected: bool,
    dim: bool,
) -> Box<dyn Renderable> {
    selection_option_row_with_label_style(index, label, is_selected, dim, false)
}

fn selection_option_row_with_label_style(
    index: usize,
    label: String,
    is_selected: bool,
    dim: bool,
    style_label_only: bool,
) -> Box<dyn Renderable> {
    let prefix = if is_selected {
        format!("› {}. ", index + 1)
    } else {
        format!("  {}. ", index + 1)
    };
    let style = if is_selected {
        Style::default().cyan()
    } else if dim {
        Style::default().dim()
    } else {
        Style::default()
    };
    let prefix_width = UnicodeWidthStr::width(prefix.as_str()) as u16;
    let mut row = RowRenderable::new();
    row.push(prefix_width, prefix.set_style(style));
    let label = if style_label_only {
        Paragraph::new(Span::from(label).set_style(style))
    } else {
        Paragraph::new(label).style(style)
    };
    row.push(u16::MAX, label.wrap(Wrap { trim: false }));
    row.into()
}
