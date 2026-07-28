use super::TABLE_BODY_SEPARATOR_CHAR;
use super::TableCell;
use super::TableColumnKind;
use super::TableColumnMetrics;
use crate::product::tui_app::wrapping::word_wrap_line_grapheme_safe;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

const FIELD_LEADING_PADDING: usize = 1;
const FIELD_GAP: usize = 2;
const MIN_VALUE_WIDTH: usize = 3;
const MIN_ALIGNED_COMPACT_VALUE_WIDTH: usize = 12;
const MIN_ALIGNED_EXPANSIVE_VALUE_WIDTH: usize = 24;
const MIN_SCANNABLE_NARRATIVE_WIDTH: usize = 12;
const MIN_SCANNABLE_TOKEN_HEAVY_WIDTH: usize = 12;
const CRAMPED_EXPANSIVE_CELL_LINES: usize = 4;
const CATASTROPHIC_NARRATIVE_CELL_LINES: usize = 7;
const STACKED_VALUE_INDENT: usize = 2;

pub(super) fn should_render_records(
    rows: &[Vec<TableCell>],
    column_widths: &[usize],
    metrics: &[TableColumnMetrics],
) -> bool {
    if rows.is_empty() {
        return false;
    }

    let affected_rows = rows
        .iter()
        .filter(|row| {
            let contains_fragmented_value =
                row.iter()
                    .zip(column_widths)
                    .zip(metrics)
                    .any(|((cell, width), metrics)| {
                        let has_fragmented_token = cell
                            .plain_text()
                            .split_whitespace()
                            .any(|token| token.width() > *width);
                        match metrics.kind {
                            TableColumnKind::Compact => has_fragmented_token,
                            TableColumnKind::TokenHeavy => {
                                *width < MIN_SCANNABLE_TOKEN_HEAVY_WIDTH && has_fragmented_token
                            }
                            TableColumnKind::Narrative => false,
                        }
                    });

            contains_fragmented_value || expansive_cells_are_starved(row, column_widths, metrics)
        })
        .count();
    let threshold = 2.max(rows.len().div_ceil(3));

    affected_rows >= threshold
}

fn expansive_cells_are_starved(
    row: &[TableCell],
    column_widths: &[usize],
    metrics: &[TableColumnMetrics],
) -> bool {
    let expansive_cells: Vec<(TableColumnKind, usize, usize)> = row
        .iter()
        .zip(column_widths)
        .zip(metrics)
        .filter(|&((_cell, _width), metrics)| metrics.kind != TableColumnKind::Compact)
        .map(|((cell, width), metrics)| (metrics.kind, *width, wrap_cell(cell, *width).len()))
        .collect();

    expansive_cells
        .iter()
        .filter(|(_, _, height)| *height >= CRAMPED_EXPANSIVE_CELL_LINES)
        .count()
        >= 2
        || expansive_cells.iter().any(|(kind, width, height)| {
            *kind == TableColumnKind::Narrative
                && *width < MIN_SCANNABLE_NARRATIVE_WIDTH
                && *height >= CATASTROPHIC_NARRATIVE_CELL_LINES
        })
}

pub(super) fn render_records(
    headers: &[TableCell],
    rows: &[Vec<TableCell>],
    metrics: &[TableColumnMetrics],
    available_width: Option<usize>,
    label_style: Style,
    separator_style: Style,
) -> Vec<Line<'static>> {
    let label_width = headers
        .iter()
        .map(|header| header_label_line(header, label_style).width())
        .max()
        .unwrap_or(0);
    let minimum_value_width = if metrics
        .iter()
        .any(|metrics| metrics.kind != TableColumnKind::Compact)
    {
        MIN_ALIGNED_EXPANSIVE_VALUE_WIDTH
    } else {
        MIN_ALIGNED_COMPACT_VALUE_WIDTH
    };
    let aligned_fields = available_width.is_none_or(|width| {
        FIELD_LEADING_PADDING + label_width + FIELD_GAP + minimum_value_width <= width
    });
    let mut out = Vec::new();

    for (row_index, row) in rows.iter().enumerate() {
        for (header, value) in headers.iter().zip(row) {
            if aligned_fields {
                render_aligned_field(
                    &mut out,
                    header,
                    value,
                    label_width,
                    available_width,
                    label_style,
                );
            } else {
                render_stacked_field(&mut out, header, value, available_width, label_style);
            }
        }
        if row_index + 1 < rows.len() {
            let width = available_width.unwrap_or_else(|| widest_line_width(&out));
            out.push(Line::from(Span::styled(
                TABLE_BODY_SEPARATOR_CHAR.to_string().repeat(width),
                separator_style,
            )));
        }
    }

    out
}

fn render_aligned_field(
    out: &mut Vec<Line<'static>>,
    header: &TableCell,
    value: &TableCell,
    label_width: usize,
    available_width: Option<usize>,
    label_style: Style,
) {
    let value_indent = FIELD_LEADING_PADDING + label_width + FIELD_GAP;
    let value_width = available_width
        .map(|width| width.saturating_sub(value_indent).max(MIN_VALUE_WIDTH))
        .unwrap_or_else(|| cell_width(value).max(MIN_VALUE_WIDTH));
    let wrapped_value = wrap_cell(value, value_width);
    for (line_index, value_line) in wrapped_value.into_iter().enumerate() {
        let mut spans = Vec::new();
        if line_index == 0 {
            let mut label = header_label_line(header, label_style);
            let rendered_label_width = label.width();
            spans.push(Span::raw(" ".repeat(FIELD_LEADING_PADDING)));
            spans.append(&mut label.spans);
            spans.push(Span::raw(" ".repeat(
                label_width.saturating_sub(rendered_label_width) + FIELD_GAP,
            )));
        } else {
            spans.push(Span::raw(" ".repeat(value_indent)));
        }
        spans.extend(value_line.spans);
        out.push(Line::from(spans));
    }
}

fn render_stacked_field(
    out: &mut Vec<Line<'static>>,
    header: &TableCell,
    value: &TableCell,
    available_width: Option<usize>,
    label_style: Style,
) {
    let label = header_label_line(header, label_style);
    let (leading_padding, wrapped_label) = wrap_with_soft_indent(
        available_width,
        FIELD_LEADING_PADDING,
        label.width().max(1),
        |width| word_wrap_line_grapheme_safe(&label, width),
    );
    for label_line in wrapped_label {
        let mut spans = vec![Span::raw(" ".repeat(leading_padding))];
        spans.extend(label_line.spans);
        out.push(Line::from(spans));
    }

    let (value_indent, wrapped_value) = wrap_with_soft_indent(
        available_width,
        STACKED_VALUE_INDENT,
        cell_width(value).max(1),
        |width| wrap_cell(value, width),
    );
    for value_line in wrapped_value {
        let mut spans = vec![Span::raw(" ".repeat(value_indent))];
        spans.extend(value_line.spans);
        out.push(Line::from(spans));
    }
}

fn wrap_with_soft_indent<F>(
    available_width: Option<usize>,
    preferred_indent: usize,
    natural_width: usize,
    mut wrap: F,
) -> (usize, Vec<Line<'static>>)
where
    F: FnMut(usize) -> Vec<Line<'static>>,
{
    let Some(available_width) = available_width else {
        return (preferred_indent, wrap(natural_width.max(1)));
    };
    let mut indent = preferred_indent.min(available_width.saturating_sub(1));
    let mut wrapped = wrap(available_width.saturating_sub(indent).max(1));
    let widest = widest_line_width(&wrapped);
    let adjusted_indent = indent.min(available_width.saturating_sub(widest.min(available_width)));
    if adjusted_indent < indent {
        indent = adjusted_indent;
        wrapped = wrap(available_width.saturating_sub(indent).max(1));
    }
    (indent, wrapped)
}

fn header_label_line(header: &TableCell, label_style: Style) -> Line<'static> {
    let mut spans = Vec::new();
    for (line_index, line) in header.lines.iter().enumerate() {
        if line_index > 0 {
            spans.push(" ".into());
        }
        spans.extend(line.spans.iter().cloned().map(|mut span| {
            span.style = span.style.patch(label_style);
            span
        }));
    }
    Line::from(spans)
}

fn wrap_cell(cell: &TableCell, width: usize) -> Vec<Line<'static>> {
    if cell.lines.is_empty() {
        return vec![Line::default()];
    }

    let mut wrapped = Vec::new();
    for source_line in &cell.lines {
        let rendered = word_wrap_line_grapheme_safe(source_line, width);
        if rendered.is_empty() {
            wrapped.push(Line::default());
        } else {
            wrapped.extend(rendered);
        }
    }
    if wrapped.is_empty() {
        wrapped.push(Line::default());
    }
    wrapped
}

fn cell_width(cell: &TableCell) -> usize {
    cell.lines.iter().map(Line::width).max().unwrap_or(0)
}

fn widest_line_width(lines: &[Line<'static>]) -> usize {
    lines.iter().map(Line::width).max().unwrap_or(0)
}
