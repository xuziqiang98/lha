use std::sync::Arc;

use crate::chatwidget::ActiveCellTranscriptKey;
use crate::history_cell::HistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::mouse::MouseScrollState;
use crate::mouse::ScrollDirection;
use crate::render::Insets;
use crate::render::renderable::InsetRenderable;
use crate::render::renderable::Renderable;
use crate::style::transcript_selection_style;
use crate::style::user_message_style;
use crate::terminal_palette;
use crate::transcript_selection::TranscriptSelection;
use crate::transcript_selection::TranscriptSelectionPoint;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Text;
use ratatui::widgets::Block;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

const DRAG_AUTOSCROLL_LINES_PER_TICK: isize = 1;
const DRAG_AUTOSCROLL_EDGE_ROWS: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiveTailKey {
    width: u16,
    revision: u64,
    is_stream_continuation: bool,
    animation_tick: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScrollAnchor {
    chunk_index: usize,
    intra_chunk_row: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DragAutoScroll {
    column: u16,
    direction: ScrollDirection,
    lines_per_tick: isize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TranscriptScroll {
    Up,
    Down,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    Home,
    End,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TranscriptRenderMode {
    #[default]
    Display,
    Transcript,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TranscriptMouseOutcome {
    Ignored,
    Scrolled,
    SelectionChanged,
    SelectionCompleted(Option<String>),
}

#[derive(Default)]
pub(crate) struct TranscriptView {
    mode: TranscriptRenderMode,
    cells: Vec<Arc<dyn HistoryCell>>,
    renderables: Vec<Box<dyn Renderable>>,
    scroll_offset: usize,
    stick_to_bottom: bool,
    user_scrolled_during_stream: bool,
    last_content_height: Option<usize>,
    last_rendered_height: Option<usize>,
    pending_scroll_chunk: Option<usize>,
    highlight_cell: Option<usize>,
    live_tail_key: Option<LiveTailKey>,
    live_tail_lines: Vec<Line<'static>>,
    last_width: Option<u16>,
    last_area: Option<Rect>,
    last_top_line: usize,
    last_total_lines: usize,
    last_padding_top: usize,
    selection: TranscriptSelection,
    drag_autoscroll: Option<DragAutoScroll>,
}

impl TranscriptView {
    pub(crate) fn new(cells: Vec<Arc<dyn HistoryCell>>, mode: TranscriptRenderMode) -> Self {
        Self {
            mode,
            renderables: Self::render_cells(&cells, None, mode),
            cells,
            scroll_offset: 0,
            stick_to_bottom: true,
            user_scrolled_during_stream: false,
            last_content_height: None,
            last_rendered_height: None,
            pending_scroll_chunk: None,
            highlight_cell: None,
            live_tail_key: None,
            live_tail_lines: Vec::new(),
            last_width: None,
            last_area: None,
            last_top_line: 0,
            last_total_lines: 0,
            last_padding_top: 0,
            selection: TranscriptSelection::default(),
            drag_autoscroll: None,
        }
    }

    #[cfg(test)]
    fn new_transcript(cells: Vec<Arc<dyn HistoryCell>>) -> Self {
        Self::new(cells, TranscriptRenderMode::Transcript)
    }

    pub(crate) fn insert_cell(&mut self, cell: Arc<dyn HistoryCell>) {
        let follow_bottom = self.is_scrolled_to_bottom();
        let spacing_width = self.last_width.unwrap_or(u16::MAX).max(1);
        let had_prior_cells = self.has_visible_committed_cells(spacing_width);
        let inserted_cell_visible = self.mode.is_visible(cell.as_ref(), spacing_width);
        let tail_renderable = self.take_live_tail_renderable();
        self.cells.push(cell);
        self.renderables = Self::render_cells(&self.cells, self.highlight_cell, self.mode);
        if let Some(tail) = tail_renderable {
            let tail = if !had_prior_cells
                && inserted_cell_visible
                && self
                    .live_tail_key
                    .is_some_and(|key| !key.is_stream_continuation)
            {
                Box::new(InsetRenderable::new(tail, Insets::tlbr(1, 0, 0, 0)))
                    as Box<dyn Renderable>
            } else {
                tail
            };
            self.renderables.push(tail);
        }
        if follow_bottom {
            self.stick_to_bottom = true;
            self.user_scrolled_during_stream = false;
        }
    }

    pub(crate) fn set_highlight_cell(&mut self, cell: Option<usize>) {
        self.highlight_cell = cell;
        self.rebuild_renderables();
        if let Some(idx) = self.highlight_cell {
            self.pending_scroll_chunk = Some(idx);
        }
    }

    pub(crate) fn sync_live_tail(
        &mut self,
        width: u16,
        active_key: Option<ActiveCellTranscriptKey>,
        compute_lines: impl FnOnce(u16) -> Option<Vec<Line<'static>>>,
    ) {
        let next_key = active_key.map(|key| LiveTailKey {
            width,
            revision: key.revision,
            is_stream_continuation: key.is_stream_continuation,
            animation_tick: key.animation_tick,
        });

        if self.live_tail_key == next_key {
            return;
        }

        let follow_bottom = self.is_scrolled_to_bottom();
        self.take_live_tail_renderable();
        self.live_tail_key = next_key;
        self.live_tail_lines.clear();

        if let Some(key) = next_key {
            let lines = compute_lines(width).unwrap_or_default();
            if !lines.is_empty() {
                self.renderables.push(Self::live_tail_renderable(
                    lines.clone(),
                    self.has_visible_committed_cells(width),
                    key.is_stream_continuation,
                ));
                self.live_tail_lines = lines;
            }
        }

        if follow_bottom {
            self.stick_to_bottom = true;
            self.user_scrolled_during_stream = false;
        }
    }

    pub(crate) fn desired_height(&self, width: u16) -> u16 {
        self.renderables
            .iter()
            .fold(0_u32, |acc, renderable| {
                acc.saturating_add(u32::from(renderable.desired_height(width)))
            })
            .min(u32::from(u16::MAX)) as u16
    }

    pub(crate) fn render_inline(&mut self, area: Rect, buf: &mut Buffer) {
        self.render_area(area, buf, false, true);
    }

    pub(crate) fn render_overlay_content(&mut self, area: Rect, buf: &mut Buffer) {
        self.render_area(area, buf, true, false);
    }

    pub(crate) fn handle_mouse_event(
        &mut self,
        mouse: MouseEvent,
        scroll_state: &mut MouseScrollState,
    ) -> TranscriptMouseOutcome {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if !self.last_area_contains(mouse.column, mouse.row) {
                    return TranscriptMouseOutcome::Ignored;
                }
                let update = scroll_state.on_scroll(ScrollDirection::Up);
                if self.apply_scroll_delta(update.delta_lines) {
                    TranscriptMouseOutcome::Scrolled
                } else {
                    TranscriptMouseOutcome::Ignored
                }
            }
            MouseEventKind::ScrollDown => {
                if !self.last_area_contains(mouse.column, mouse.row) {
                    return TranscriptMouseOutcome::Ignored;
                }
                let update = scroll_state.on_scroll(ScrollDirection::Down);
                if self.apply_scroll_delta(update.delta_lines) {
                    TranscriptMouseOutcome::Scrolled
                } else {
                    TranscriptMouseOutcome::Ignored
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(point) = self.selection_point_from_position(mouse.column, mouse.row) {
                    self.selection.anchor = Some(point);
                    self.selection.head = Some(point);
                    self.selection.dragging = true;
                    self.drag_autoscroll = None;
                    TranscriptMouseOutcome::SelectionChanged
                } else if self.selection.is_active() {
                    self.selection.clear();
                    self.drag_autoscroll = None;
                    TranscriptMouseOutcome::SelectionChanged
                } else {
                    TranscriptMouseOutcome::Ignored
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if !self.selection.dragging {
                    return TranscriptMouseOutcome::Ignored;
                }

                if let Some((point, autoscroll)) =
                    self.drag_selection_point_from_position(mouse.column, mouse.row)
                {
                    self.selection.head = Some(point);
                    self.drag_autoscroll = autoscroll;
                    TranscriptMouseOutcome::SelectionChanged
                } else {
                    TranscriptMouseOutcome::Ignored
                }
            }
            MouseEventKind::Up(MouseButton::Left) if self.selection.dragging => {
                self.selection.dragging = false;
                self.drag_autoscroll = None;
                TranscriptMouseOutcome::SelectionCompleted(self.selection_to_text())
            }
            _ => TranscriptMouseOutcome::Ignored,
        }
    }

    pub(crate) fn advance_drag_autoscroll(&mut self, area: Rect) -> bool {
        let Some(autoscroll) = self.drag_autoscroll else {
            return false;
        };
        if !self.selection.dragging || area.is_empty() {
            self.drag_autoscroll = None;
            return false;
        }

        let delta = match autoscroll.direction {
            ScrollDirection::Up => -autoscroll.lines_per_tick,
            ScrollDirection::Down => autoscroll.lines_per_tick,
        };
        let changed_scroll = self.apply_scroll_delta(delta);
        let Some(point) = self.edge_selection_point_for_top(
            area,
            autoscroll.direction,
            autoscroll.column,
            self.scroll_offset,
        ) else {
            self.drag_autoscroll = None;
            return changed_scroll;
        };
        let changed_selection = self.selection.head != Some(point);
        self.selection.head = Some(point);
        changed_scroll || changed_selection
    }

    pub(crate) fn drag_autoscroll_active(&self) -> bool {
        self.drag_autoscroll.is_some() && self.selection.dragging
    }

    pub(crate) fn apply_scroll(&mut self, command: TranscriptScroll) -> bool {
        let old = self.scroll_offset;
        let old_stick_to_bottom = self.stick_to_bottom;
        let old_user_scrolled_during_stream = self.user_scrolled_during_stream;
        let was_at_bottom = self.is_scrolled_to_bottom();
        match command {
            TranscriptScroll::Up => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                self.update_after_upward_scroll(old, was_at_bottom);
            }
            TranscriptScroll::Down => {
                self.stick_to_bottom = false;
                self.user_scrolled_during_stream = false;
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            TranscriptScroll::PageUp => {
                let page = self.page_height();
                self.scroll_offset = self.scroll_offset.saturating_sub(page);
                self.update_after_upward_scroll(old, was_at_bottom);
            }
            TranscriptScroll::PageDown => {
                self.stick_to_bottom = false;
                self.user_scrolled_during_stream = false;
                let page = self.page_height();
                self.scroll_offset = self.scroll_offset.saturating_add(page);
            }
            TranscriptScroll::HalfPageUp => {
                let half_page = self.page_height().saturating_add(1) / 2;
                self.scroll_offset = self.scroll_offset.saturating_sub(half_page);
                self.update_after_upward_scroll(old, was_at_bottom);
            }
            TranscriptScroll::HalfPageDown => {
                self.stick_to_bottom = false;
                self.user_scrolled_during_stream = false;
                let half_page = self.page_height().saturating_add(1) / 2;
                self.scroll_offset = self.scroll_offset.saturating_add(half_page);
            }
            TranscriptScroll::Home => {
                self.scroll_offset = 0;
                self.update_after_upward_scroll(old, was_at_bottom);
            }
            TranscriptScroll::End => {
                self.stick_to_bottom = true;
                self.user_scrolled_during_stream = false;
            }
        }
        self.scroll_offset != old
            || self.stick_to_bottom != old_stick_to_bottom
            || self.user_scrolled_during_stream != old_user_scrolled_during_stream
    }

    pub(crate) fn apply_scroll_delta(&mut self, delta_lines: isize) -> bool {
        if delta_lines == 0 {
            return false;
        }
        let old = self.scroll_offset;
        let old_stick_to_bottom = self.stick_to_bottom;
        let old_user_scrolled_during_stream = self.user_scrolled_during_stream;
        let was_at_bottom = self.is_scrolled_to_bottom();
        let page_height = self.page_height();
        let max_scroll = self
            .last_rendered_height
            .unwrap_or_else(|| self.content_height(self.last_width.unwrap_or(1).max(1)))
            .saturating_sub(page_height);

        let current = if self.stick_to_bottom {
            max_scroll
        } else {
            self.scroll_offset.min(max_scroll)
        };

        if delta_lines < 0 {
            let before = self.scroll_offset;
            self.scroll_offset = current.saturating_sub(delta_lines.unsigned_abs());
            if self.scroll_offset != before {
                self.stick_to_bottom = false;
                self.user_scrolled_during_stream = true;
            } else if was_at_bottom {
                self.stick_to_bottom = true;
                self.user_scrolled_during_stream = false;
            }
        } else {
            self.scroll_offset = current.saturating_add(delta_lines as usize).min(max_scroll);
            if self.scroll_offset >= max_scroll {
                self.stick_to_bottom = true;
                self.user_scrolled_during_stream = false;
            } else {
                self.stick_to_bottom = false;
                self.user_scrolled_during_stream = true;
            }
        }

        self.scroll_offset != old
            || self.stick_to_bottom != old_stick_to_bottom
            || self.user_scrolled_during_stream != old_user_scrolled_during_stream
    }

    pub(crate) fn is_scrolled_to_bottom(&self) -> bool {
        if self.stick_to_bottom {
            return true;
        }
        if self.user_scrolled_during_stream {
            return false;
        }
        let Some(height) = self.last_content_height else {
            return false;
        };
        if self.renderables.is_empty() {
            return true;
        }
        let Some(total_height) = self.last_rendered_height else {
            return false;
        };
        if total_height <= height {
            return true;
        }
        let max_scroll = total_height.saturating_sub(height);
        self.scroll_offset >= max_scroll
    }

    pub(crate) fn scroll_percent(&self) -> u8 {
        let Some(content_height) = self.last_content_height else {
            return 100;
        };
        let Some(total_height) = self.last_rendered_height else {
            return 100;
        };
        if total_height == 0 {
            return 100;
        }
        let max_scroll = total_height.saturating_sub(content_height);
        if max_scroll == 0 {
            100
        } else {
            (((self.scroll_offset.min(max_scroll)) as f32 / max_scroll as f32) * 100.0).round()
                as u8
        }
    }

    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    #[cfg(test)]
    pub(crate) fn set_scroll_offset(&mut self, scroll_offset: usize) {
        self.scroll_offset = scroll_offset;
        self.stick_to_bottom = scroll_offset == usize::MAX;
        self.user_scrolled_during_stream = scroll_offset != usize::MAX;
    }

    #[cfg(test)]
    pub(crate) fn content_area(&self, area: Rect) -> Rect {
        let mut area = area;
        area.y = area.y.saturating_add(1);
        area.height = area.height.saturating_sub(2);
        area
    }

    #[cfg(test)]
    pub(crate) fn page_height_for_area(&self, area: Rect) -> usize {
        self.last_content_height
            .unwrap_or_else(|| self.content_area(area).height as usize)
    }

    fn render_area(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        fill_empty_rows: bool,
        bottom_align_if_short: bool,
    ) {
        let width = area.width.max(1);
        let follow_bottom = self.is_scrolled_to_bottom();
        if follow_bottom {
            self.stick_to_bottom = true;
        } else if self.last_width != Some(width)
            && let Some(last_width) = self.last_width
            && let Some(anchor) = self.anchor_for_offset(last_width)
        {
            self.scroll_offset = self.offset_for_anchor(anchor, width);
        }

        self.last_width = Some(width);
        self.last_content_height = Some(area.height as usize);

        let content_height = self.content_height(width);
        self.last_rendered_height = Some(content_height);
        if self.stick_to_bottom {
            self.scroll_offset = content_height.saturating_sub(area.height as usize);
        }
        let mut moved_for_highlight = false;
        if let Some(idx) = self.pending_scroll_chunk.take() {
            let before = self.scroll_offset;
            self.ensure_chunk_visible(idx, area, width);
            moved_for_highlight = self.scroll_offset != before;
        }
        let max_scroll = content_height.saturating_sub(area.height as usize);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
        if moved_for_highlight && self.scroll_offset < max_scroll {
            self.stick_to_bottom = false;
            self.user_scrolled_during_stream = true;
        } else if max_scroll > 0
            && self.scroll_offset >= max_scroll
            && !self.user_scrolled_during_stream
        {
            self.stick_to_bottom = true;
            self.user_scrolled_during_stream = false;
        }

        Clear.render(area, buf);

        let mut y = -(self.scroll_offset as isize);
        if bottom_align_if_short && content_height < area.height as usize {
            y += area.height.saturating_sub(content_height as u16) as isize;
        }

        let mut drawn_bottom = area.y;
        for renderable in &self.renderables {
            let top = y;
            let height = renderable.desired_height(width) as isize;
            y += height;
            let bottom = y;
            if bottom <= 0 {
                continue;
            }
            if top >= area.height as isize {
                break;
            }
            if top < 0 {
                let drawn = render_offset_content(area, buf, &**renderable, (-top) as u16);
                drawn_bottom = drawn_bottom.max(area.y + drawn);
            } else {
                let draw_height = (height as u16).min(area.height.saturating_sub(top as u16));
                let draw_area = Rect::new(area.x, area.y + top as u16, area.width, draw_height);
                renderable.render(draw_area, buf);
                drawn_bottom = drawn_bottom.max(draw_area.y.saturating_add(draw_area.height));
            }
        }

        if fill_empty_rows {
            for y in drawn_bottom..area.bottom() {
                if area.width == 0 {
                    break;
                }
                buf[(area.x, y)] = Cell::from('~');
                for x in area.x + 1..area.right() {
                    buf[(x, y)] = Cell::from(' ');
                }
            }
        }

        self.last_area = Some(area);
        self.last_top_line = self.scroll_offset;
        self.last_total_lines = content_height;
        self.last_padding_top = if bottom_align_if_short && content_height < area.height as usize {
            area.height.saturating_sub(content_height as u16) as usize
        } else {
            0
        };
        self.apply_selection_highlight(area, buf);
    }

    fn apply_selection_highlight(&self, area: Rect, buf: &mut Buffer) {
        let Some((start, end)) = self.selection.ordered_endpoints() else {
            return;
        };
        if area.width == 0 || area.height == 0 {
            return;
        }

        let selection_style = transcript_selection_style();
        for y in area.y..area.bottom() {
            let visible_row = y.saturating_sub(area.y) as usize;
            if visible_row < self.last_padding_top {
                continue;
            }
            let line_index = self
                .last_top_line
                .saturating_add(visible_row.saturating_sub(self.last_padding_top));
            if line_index < start.line_index || line_index > end.line_index {
                continue;
            }
            let (col_start, col_end) = if start.line_index == end.line_index {
                (start.column.min(end.column), end.column.max(start.column))
            } else if line_index == start.line_index {
                (start.column, area.width as usize)
            } else if line_index == end.line_index {
                (0, end.column)
            } else {
                (0, area.width as usize)
            };
            if col_start == col_end {
                continue;
            }
            let x_start = area.x.saturating_add((col_start as u16).min(area.width));
            let x_end = area.x.saturating_add((col_end as u16).min(area.width));
            for x in x_start..x_end {
                let style = buf[(x, y)].style().patch(selection_style);
                buf[(x, y)].set_style(style);
            }
        }
    }

    fn last_area_contains(&self, column: u16, row: u16) -> bool {
        self.last_area.is_some_and(|area| {
            column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
        })
    }

    fn selection_point_from_position(
        &self,
        column: u16,
        row: u16,
    ) -> Option<TranscriptSelectionPoint> {
        let area = self.last_area?;
        if column < area.x || column >= area.right() || row < area.y || row >= area.bottom() {
            return None;
        }
        if self.last_total_lines == 0 {
            return None;
        }

        let row = row.saturating_sub(area.y) as usize;
        if row < self.last_padding_top {
            return None;
        }
        let line_index = self
            .last_top_line
            .saturating_add(row.saturating_sub(self.last_padding_top))
            .min(self.last_total_lines.saturating_sub(1));
        let column = column.saturating_sub(area.x) as usize;
        Some(TranscriptSelectionPoint { line_index, column })
    }

    fn drag_selection_point_from_position(
        &self,
        column: u16,
        row: u16,
    ) -> Option<(TranscriptSelectionPoint, Option<DragAutoScroll>)> {
        let area = self.last_area?;
        if area.is_empty() || self.last_total_lines == 0 {
            return None;
        }

        let clamped_column = clamp_to_area_column(area, column);
        if row < area.y {
            let point = self.edge_selection_point(area, ScrollDirection::Up, clamped_column)?;
            return Some((
                point,
                Some(DragAutoScroll {
                    column: clamped_column,
                    direction: ScrollDirection::Up,
                    lines_per_tick: DRAG_AUTOSCROLL_LINES_PER_TICK,
                }),
            ));
        }
        if row >= area.bottom() {
            let point = self.edge_selection_point(area, ScrollDirection::Down, clamped_column)?;
            return Some((
                point,
                Some(DragAutoScroll {
                    column: clamped_column,
                    direction: ScrollDirection::Down,
                    lines_per_tick: DRAG_AUTOSCROLL_LINES_PER_TICK,
                }),
            ));
        }

        let row_offset = row.saturating_sub(area.y);
        if (row_offset as usize) < self.last_padding_top {
            return None;
        }

        let point = self.selection_point_from_position(clamped_column, row)?;
        let autoscroll = if row_offset < DRAG_AUTOSCROLL_EDGE_ROWS {
            Some(DragAutoScroll {
                column: clamped_column,
                direction: ScrollDirection::Up,
                lines_per_tick: DRAG_AUTOSCROLL_LINES_PER_TICK,
            })
        } else if area.height.saturating_sub(row_offset).saturating_sub(1)
            < DRAG_AUTOSCROLL_EDGE_ROWS
        {
            Some(DragAutoScroll {
                column: clamped_column,
                direction: ScrollDirection::Down,
                lines_per_tick: DRAG_AUTOSCROLL_LINES_PER_TICK,
            })
        } else {
            None
        };
        Some((point, autoscroll))
    }

    fn edge_selection_point(
        &self,
        area: Rect,
        direction: ScrollDirection,
        column: u16,
    ) -> Option<TranscriptSelectionPoint> {
        self.edge_selection_point_for_top(area, direction, column, self.last_top_line)
    }

    fn edge_selection_point_for_top(
        &self,
        area: Rect,
        direction: ScrollDirection,
        column: u16,
        top_line: usize,
    ) -> Option<TranscriptSelectionPoint> {
        if area.is_empty() || self.last_total_lines == 0 {
            return None;
        }

        let visible_rows = area.height as usize;
        if visible_rows <= self.last_padding_top {
            return None;
        }
        let content_rows = visible_rows.saturating_sub(self.last_padding_top);
        let row = match direction {
            ScrollDirection::Up => self.last_padding_top,
            ScrollDirection::Down => self
                .last_padding_top
                .saturating_add(content_rows.saturating_sub(1)),
        };
        let line_index = top_line
            .saturating_add(row.saturating_sub(self.last_padding_top))
            .min(self.last_total_lines.saturating_sub(1));
        let column = clamp_to_area_column(area, column).saturating_sub(area.x) as usize;
        Some(TranscriptSelectionPoint { line_index, column })
    }

    fn selection_to_text(&self) -> Option<String> {
        let (start, end) = self.selection.ordered_endpoints()?;
        let width = self.last_area?.width.max(1);
        let lines = self.semantic_plain_lines_for_width(width);
        if lines.is_empty() {
            return None;
        }

        let end_index = end.line_index.min(lines.len().saturating_sub(1));
        let start_index = start.line_index.min(end_index);
        let mut selected_lines = Vec::new();
        for (line_index, line_text) in lines
            .iter()
            .enumerate()
            .take(end_index + 1)
            .skip(start_index)
        {
            let line_width = UnicodeWidthStr::width(line_text.as_str());
            let (col_start, col_end) = if start_index == end_index {
                (start.column.min(end.column), end.column.max(start.column))
            } else if line_index == start_index {
                (start.column, line_width)
            } else if line_index == end_index {
                (0, end.column)
            } else {
                (0, line_width)
            };
            selected_lines.push(slice_display_columns(line_text, col_start, col_end));
        }
        Some(selected_lines.join("\n"))
    }

    fn semantic_plain_lines_for_width(&self, width: u16) -> Vec<String> {
        let width = width.max(1);
        let mut lines = Vec::new();
        let mut has_visible_prior_cell = false;
        for cell in &self.cells {
            let cell_lines = self.mode.lines(cell.as_ref(), width);
            if cell_lines.is_empty() {
                continue;
            }
            if has_visible_prior_cell && !cell.is_stream_continuation() {
                lines.push(String::new());
            }
            push_plain_lines(&mut lines, cell_lines);
            has_visible_prior_cell = true;
        }

        if let Some(key) = self.live_tail_key
            && !self.live_tail_lines.is_empty()
        {
            if has_visible_prior_cell && !key.is_stream_continuation {
                lines.push(String::new());
            }
            push_plain_lines(&mut lines, self.live_tail_lines.clone());
        }
        lines
    }

    fn update_after_upward_scroll(&mut self, old_offset: usize, was_at_bottom: bool) {
        if self.scroll_offset != old_offset {
            self.stick_to_bottom = false;
            self.user_scrolled_during_stream = true;
        } else if was_at_bottom {
            self.stick_to_bottom = true;
            self.user_scrolled_during_stream = false;
        }
    }

    fn render_cells(
        cells: &[Arc<dyn HistoryCell>],
        highlight_cell: Option<usize>,
        mode: TranscriptRenderMode,
    ) -> Vec<Box<dyn Renderable>> {
        let mut has_visible_prior_cell = false;
        cells
            .iter()
            .enumerate()
            .map(|(idx, cell)| {
                let is_visible = mode.is_visible(cell.as_ref(), u16::MAX);
                let top_gap =
                    is_visible && has_visible_prior_cell && !cell.is_stream_continuation();
                if is_visible {
                    has_visible_prior_cell = true;
                }
                let renderable: Box<dyn Renderable> = if cell.as_any().is::<UserHistoryCell>() {
                    Box::new(CachedRenderable::new(CellRenderable {
                        cell: cell.clone(),
                        mode,
                        top_gap,
                        style: if highlight_cell == Some(idx) {
                            user_message_style().reversed()
                        } else {
                            user_message_style()
                        },
                        cache: std::cell::RefCell::new(None),
                    }))
                } else {
                    Box::new(CachedRenderable::new(CellRenderable {
                        cell: cell.clone(),
                        mode,
                        top_gap,
                        style: Style::default(),
                        cache: std::cell::RefCell::new(None),
                    }))
                };
                renderable
            })
            .collect()
    }

    fn rebuild_renderables(&mut self) {
        let tail_renderable = self.take_live_tail_renderable();
        self.renderables = Self::render_cells(&self.cells, self.highlight_cell, self.mode);
        if let Some(tail) = tail_renderable {
            self.renderables.push(tail);
        }
    }

    fn take_live_tail_renderable(&mut self) -> Option<Box<dyn Renderable>> {
        (self.renderables.len() > self.cells.len()).then(|| self.renderables.pop())?
    }

    fn live_tail_renderable(
        lines: Vec<Line<'static>>,
        has_prior_cells: bool,
        is_stream_continuation: bool,
    ) -> Box<dyn Renderable> {
        let paragraph = Paragraph::new(Text::from(lines));
        let mut renderable: Box<dyn Renderable> = Box::new(CachedRenderable::new(paragraph));
        if has_prior_cells && !is_stream_continuation {
            renderable = Box::new(InsetRenderable::new(renderable, Insets::tlbr(1, 0, 0, 0)));
        }
        renderable
    }

    fn has_visible_committed_cells(&self, width: u16) -> bool {
        self.cells
            .iter()
            .any(|cell| self.mode.is_visible(cell.as_ref(), width))
    }

    fn content_height(&self, width: u16) -> usize {
        self.renderables
            .iter()
            .map(|renderable| renderable.desired_height(width) as usize)
            .sum()
    }

    fn page_height(&self) -> usize {
        self.last_content_height.unwrap_or(1)
    }

    fn ensure_chunk_visible(&mut self, idx: usize, area: Rect, width: u16) {
        if area.height == 0 || idx >= self.renderables.len() {
            return;
        }

        let first = self
            .renderables
            .iter()
            .take(idx)
            .map(|renderable| renderable.desired_height(width) as usize)
            .sum::<usize>();
        let last = first + self.renderables[idx].desired_height(width) as usize;
        let current_top = self.scroll_offset;
        let current_bottom = current_top.saturating_add(area.height.saturating_sub(1) as usize);
        if first < current_top {
            self.scroll_offset = first;
        } else if last > current_bottom {
            self.scroll_offset = last.saturating_sub(area.height.saturating_sub(1) as usize);
        }
    }

    fn anchor_for_offset(&self, width: u16) -> Option<ScrollAnchor> {
        if self.renderables.is_empty() {
            return None;
        }

        let mut start = 0usize;
        for (chunk_index, renderable) in self.renderables.iter().enumerate() {
            let height = renderable.desired_height(width) as usize;
            let end = start + height;
            if self.scroll_offset < end {
                return Some(ScrollAnchor {
                    chunk_index,
                    intra_chunk_row: self.scroll_offset.saturating_sub(start),
                });
            }
            start = end;
        }

        self.renderables.last().map(|renderable| ScrollAnchor {
            chunk_index: self.renderables.len().saturating_sub(1),
            intra_chunk_row: renderable.desired_height(width).saturating_sub(1) as usize,
        })
    }

    fn offset_for_anchor(&self, anchor: ScrollAnchor, width: u16) -> usize {
        if self.renderables.is_empty() {
            return 0;
        }

        let chunk_index = anchor
            .chunk_index
            .min(self.renderables.len().saturating_sub(1));
        let top = self
            .renderables
            .iter()
            .take(chunk_index)
            .map(|renderable| renderable.desired_height(width) as usize)
            .sum::<usize>();
        let chunk_height = self.renderables[chunk_index].desired_height(width) as usize;
        top + anchor.intra_chunk_row.min(chunk_height.saturating_sub(1))
    }
}

struct CachedRenderable {
    renderable: Box<dyn Renderable>,
    height: std::cell::Cell<Option<u16>>,
    last_width: std::cell::Cell<Option<u16>>,
}

impl CachedRenderable {
    fn new(renderable: impl Into<Box<dyn Renderable>>) -> Self {
        Self {
            renderable: renderable.into(),
            height: std::cell::Cell::new(None),
            last_width: std::cell::Cell::new(None),
        }
    }
}

impl Renderable for CachedRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.renderable.render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        if self.last_width.get() != Some(width) {
            let height = self.renderable.desired_height(width);
            self.height.set(Some(height));
            self.last_width.set(Some(width));
        }
        self.height.get().unwrap_or(0)
    }
}

#[derive(Debug)]
struct CellRenderable {
    cell: Arc<dyn HistoryCell>,
    mode: TranscriptRenderMode,
    top_gap: bool,
    style: Style,
    cache: std::cell::RefCell<Option<CellRenderCache>>,
}

#[derive(Clone, Debug)]
struct CellRenderState {
    lines: Vec<Line<'static>>,
    height: u16,
}

#[derive(Clone, Debug)]
struct CellRenderCache {
    width: u16,
    palette_version: u64,
    state: CellRenderState,
}

impl CellRenderable {
    fn render_state(&self, width: u16) -> CellRenderState {
        let palette_version = terminal_palette::palette_version();
        if let Some(cache) = self.cache.borrow().as_ref()
            && cache.width == width
            && cache.palette_version == palette_version
        {
            return cache.state.clone();
        }

        let lines = self.mode.lines(self.cell.as_ref(), width);
        let height = rendered_lines_height(&lines, width)
            .max(self.mode.desired_height(self.cell.as_ref(), width));
        let state = CellRenderState { lines, height };
        self.cache.replace(Some(CellRenderCache {
            width,
            palette_version,
            state: state.clone(),
        }));
        state
    }
}

impl Renderable for CellRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let state = self.render_state(area.width);
        if state.height == 0 {
            return;
        }
        let area = if self.top_gap {
            Rect::new(
                area.x,
                area.y.saturating_add(1),
                area.width,
                area.height.saturating_sub(1),
            )
        } else {
            area
        };
        let style = self
            .cell
            .block_style()
            .unwrap_or_default()
            .patch(self.style);
        Block::default().style(style).render(area, buf);
        Paragraph::new(Text::from(state.lines))
            .style(style)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let height = self.render_state(width).height;
        if height == 0 {
            0
        } else {
            height.saturating_add(u16::from(self.top_gap))
        }
    }
}

impl TranscriptRenderMode {
    fn lines(self, cell: &dyn HistoryCell, width: u16) -> Vec<Line<'static>> {
        match self {
            TranscriptRenderMode::Display => cell.display_lines(width),
            TranscriptRenderMode::Transcript => cell.transcript_lines(width),
        }
    }

    fn desired_height(self, cell: &dyn HistoryCell, width: u16) -> u16 {
        match self {
            TranscriptRenderMode::Display => cell.desired_height(width),
            TranscriptRenderMode::Transcript => cell.desired_transcript_height(width),
        }
    }

    fn is_visible(self, cell: &dyn HistoryCell, width: u16) -> bool {
        self.desired_height(cell, width) > 0
    }
}

fn rendered_lines_height(lines: &[Line<'static>], width: u16) -> u16 {
    if lines.is_empty() {
        return 0;
    }
    if let [line] = lines
        && line
            .spans
            .iter()
            .all(|span| span.content.chars().all(char::is_whitespace))
    {
        return 1;
    }

    Paragraph::new(Text::from(lines.to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .try_into()
        .unwrap_or(u16::MAX)
}

fn render_offset_content(
    area: Rect,
    buf: &mut Buffer,
    renderable: &dyn Renderable,
    scroll_offset: u16,
) -> u16 {
    let height = renderable.desired_height(area.width);
    let mut tall_buf = Buffer::empty(Rect::new(
        0,
        0,
        area.width,
        height.min(area.height + scroll_offset),
    ));
    renderable.render(*tall_buf.area(), &mut tall_buf);
    let copy_height = area
        .height
        .min(tall_buf.area().height.saturating_sub(scroll_offset));
    for y in 0..copy_height {
        let src_y = y + scroll_offset;
        for x in 0..area.width {
            buf[(area.x + x, area.y + y)] = tall_buf[(x, src_y)].clone();
        }
    }

    copy_height
}

fn clamp_to_area_column(area: Rect, column: u16) -> u16 {
    if area.width == 0 {
        return area.x;
    }
    column.clamp(area.x, area.right().saturating_sub(1))
}

fn push_plain_lines<I>(out: &mut Vec<String>, lines: I)
where
    I: IntoIterator<Item = Line<'static>>,
{
    out.extend(lines.into_iter().map(|line| line_to_plain_text(&line)));
}

fn line_to_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn slice_display_columns(text: &str, col_start: usize, col_end: usize) -> String {
    if col_start >= col_end {
        return String::new();
    }

    let mut out = String::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        let next_col = col.saturating_add(width);
        if next_col > col_start && col < col_end {
            out.push(ch);
        }
        col = next_col;
        if col >= col_end {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use crossterm::event::MouseEvent;
    use crossterm::event::MouseEventKind;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    #[derive(Debug)]
    struct TestCell(&'static str);

    #[derive(Debug)]
    struct OwnedTestCell(String);

    #[derive(Debug)]
    struct FixedHeightCell(usize);

    #[derive(Debug)]
    struct MultiLineTestCell(Vec<&'static str>);

    #[derive(Debug)]
    struct StyledBlockCell;

    #[derive(Debug)]
    struct SplitRenderCell;

    #[derive(Debug)]
    struct HiddenDisplayCell;

    #[derive(Debug)]
    struct CountingDisplayCell {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl HistoryCell for TestCell {
        fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
            textwrap::wrap(self.0, usize::from(width.max(1)))
                .into_iter()
                .map(|line| line.to_string().into())
                .collect()
        }
    }

    impl HistoryCell for OwnedTestCell {
        fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
            textwrap::wrap(&self.0, usize::from(width.max(1)))
                .into_iter()
                .map(|line| line.to_string().into())
                .collect()
        }
    }

    impl HistoryCell for FixedHeightCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            (0..self.0).map(|_| "x".into()).collect()
        }
    }

    impl HistoryCell for MultiLineTestCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            self.0.iter().map(|line| (*line).into()).collect()
        }
    }

    impl HistoryCell for StyledBlockCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            vec!["styled".into()]
        }

        fn block_style(&self) -> Option<Style> {
            Some(Style::default().bg(Color::Blue))
        }
    }

    impl HistoryCell for SplitRenderCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            vec!["display".into()]
        }

        fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
            vec!["transcript".into()]
        }
    }

    impl HistoryCell for HiddenDisplayCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            Vec::new()
        }

        fn desired_height(&self, _width: u16) -> u16 {
            0
        }

        fn transcript_lines(&self, _width: u16) -> Vec<Line<'static>> {
            vec!["hidden transcript".into()]
        }
    }

    impl HistoryCell for CountingDisplayCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            vec!["counted".into()]
        }

        fn desired_height(&self, _width: u16) -> u16 {
            1
        }
    }

    fn area_lines(buf: &Buffer, area: Rect) -> Vec<String> {
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect()
    }

    fn render_test_view(view: &mut TranscriptView, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render_inline(area, &mut buf);
        buf
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn select_columns(
        view: &mut TranscriptView,
        start_line: usize,
        start_column: usize,
        end_line: usize,
        end_column: usize,
    ) -> Option<String> {
        view.selection.anchor = Some(TranscriptSelectionPoint {
            line_index: start_line,
            column: start_column,
        });
        view.selection.head = Some(TranscriptSelectionPoint {
            line_index: end_line,
            column: end_column,
        });
        view.selection_to_text()
    }

    fn counting_display_view() -> (TranscriptView, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let view = TranscriptView::new(
            vec![Arc::new(CountingDisplayCell {
                calls: calls.clone(),
            })],
            TranscriptRenderMode::Display,
        );
        calls.store(0, std::sync::atomic::Ordering::Relaxed);
        (view, calls)
    }

    #[test]
    fn render_reuses_display_lines_within_frame() {
        let (mut view, calls) = counting_display_view();

        let _ = render_test_view(&mut view, 20, 3);

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn render_reuses_display_lines_across_frames() {
        let (mut view, calls) = counting_display_view();

        let _ = render_test_view(&mut view, 20, 3);
        let _ = render_test_view(&mut view, 20, 3);

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn render_cache_invalidates_on_width_change() {
        let (mut view, calls) = counting_display_view();

        let _ = render_test_view(&mut view, 20, 3);
        let _ = render_test_view(&mut view, 30, 3);

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn desired_height_and_render_share_cached_lines() {
        let (mut view, calls) = counting_display_view();

        assert_eq!(view.desired_height(20), 1);
        let _ = render_test_view(&mut view, 20, 3);

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn display_mode_renders_display_lines() {
        let mut view = TranscriptView::new(
            vec![Arc::new(SplitRenderCell)],
            TranscriptRenderMode::Display,
        );
        let buf = render_test_view(&mut view, 20, 3);

        let rendered = area_lines(&buf, Rect::new(0, 0, 20, 3)).join("\n");
        assert!(rendered.contains("display"));
        assert!(!rendered.contains("transcript"));
    }

    #[test]
    fn transcript_mode_renders_transcript_lines() {
        let mut view = TranscriptView::new(
            vec![Arc::new(SplitRenderCell)],
            TranscriptRenderMode::Transcript,
        );
        let buf = render_test_view(&mut view, 20, 3);

        let rendered = area_lines(&buf, Rect::new(0, 0, 20, 3)).join("\n");
        assert!(rendered.contains("transcript"));
        assert!(!rendered.contains("display"));
    }

    #[test]
    fn display_mode_skips_display_empty_cells_without_extra_gap() {
        let mut view = TranscriptView::new(
            vec![
                Arc::new(TestCell("first")) as Arc<dyn HistoryCell>,
                Arc::new(HiddenDisplayCell) as Arc<dyn HistoryCell>,
                Arc::new(TestCell("second")) as Arc<dyn HistoryCell>,
            ],
            TranscriptRenderMode::Display,
        );
        let buf = render_test_view(&mut view, 20, 5);

        assert_eq!(view.desired_height(20), 3);
        assert_eq!(
            area_lines(&buf, Rect::new(0, 2, 20, 3)),
            vec![
                "first               ".to_string(),
                "                    ".to_string(),
                "second              ".to_string(),
            ]
        );
        assert_eq!(
            view.semantic_plain_lines_for_width(20),
            vec!["first".to_string(), String::new(), "second".to_string(),]
        );
    }

    #[test]
    fn display_mode_live_tail_does_not_add_gap_after_only_hidden_cells() {
        let mut view = TranscriptView::new(
            vec![Arc::new(HiddenDisplayCell) as Arc<dyn HistoryCell>],
            TranscriptRenderMode::Display,
        );
        view.sync_live_tail(
            20,
            Some(ActiveCellTranscriptKey {
                revision: 1,
                is_stream_continuation: false,
                animation_tick: None,
            }),
            |_| Some(vec!["tail".into()]),
        );
        let buf = render_test_view(&mut view, 20, 2);

        assert_eq!(view.desired_height(20), 1);
        assert_eq!(
            area_lines(&buf, Rect::new(0, 1, 20, 1)),
            vec!["tail                ".to_string()]
        );
    }

    #[test]
    fn display_mode_insert_hidden_cell_before_live_tail_does_not_add_gap() {
        let mut view = TranscriptView::new(Vec::new(), TranscriptRenderMode::Display);
        view.sync_live_tail(
            20,
            Some(ActiveCellTranscriptKey {
                revision: 1,
                is_stream_continuation: false,
                animation_tick: None,
            }),
            |_| Some(vec!["tail".into()]),
        );

        view.insert_cell(Arc::new(HiddenDisplayCell));
        let buf = render_test_view(&mut view, 20, 1);

        assert_eq!(view.desired_height(20), 1);
        assert_eq!(
            area_lines(&buf, Rect::new(0, 0, 20, 1)),
            vec!["tail                ".to_string()]
        );
    }

    #[test]
    fn display_mode_insert_first_visible_cell_before_live_tail_adds_gap() {
        let mut view = TranscriptView::new(Vec::new(), TranscriptRenderMode::Display);
        view.sync_live_tail(
            20,
            Some(ActiveCellTranscriptKey {
                revision: 1,
                is_stream_continuation: false,
                animation_tick: None,
            }),
            |_| Some(vec!["tail".into()]),
        );

        view.insert_cell(Arc::new(TestCell("first")));
        let buf = render_test_view(&mut view, 20, 3);

        assert_eq!(view.desired_height(20), 3);
        assert_eq!(
            area_lines(&buf, Rect::new(0, 0, 20, 3)),
            vec![
                "first               ".to_string(),
                "                    ".to_string(),
                "tail                ".to_string(),
            ]
        );
    }

    #[test]
    fn resize_keeps_anchor_when_scrolled_up() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell(
            "alpha beta gamma delta epsilon zeta",
        ))]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 3));
        view.render_inline(Rect::new(0, 0, 8, 3), &mut buf);
        view.apply_scroll(TranscriptScroll::Home);
        view.apply_scroll(TranscriptScroll::Down);

        let mut wide = Buffer::empty(Rect::new(0, 0, 14, 3));
        view.render_inline(Rect::new(0, 0, 14, 3), &mut wide);

        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn mouse_wheel_scrolls_transcript_in_area() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(FixedHeightCell(20))]);
        let _ = render_test_view(&mut view, 10, 5);
        let at_tail = view.scroll_offset();

        let mut scroll = MouseScrollState::default();
        let outcome = view.handle_mouse_event(mouse(MouseEventKind::ScrollUp, 1, 1), &mut scroll);

        assert_eq!(outcome, TranscriptMouseOutcome::Scrolled);
        assert!(view.scroll_offset() < at_tail);
    }

    #[test]
    fn block_style_fills_transcript_cell_width() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(StyledBlockCell)]);
        let buf = render_test_view(&mut view, 12, 3);

        for x in 0..12 {
            assert_eq!(buf[(x, 2)].style().bg, Some(Color::Blue));
        }
    }

    #[test]
    fn default_cells_do_not_gain_block_background() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell("plain"))]);
        let buf = render_test_view(&mut view, 12, 3);

        for x in 0..12 {
            assert_ne!(buf[(x, 2)].style().bg, Some(Color::Blue));
        }
    }

    #[test]
    fn mouse_wheel_ignores_sidebar_area() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(FixedHeightCell(20))]);
        let _ = render_test_view(&mut view, 10, 5);
        let before = view.scroll_offset();

        let mut scroll = MouseScrollState::default();
        let outcome = view.handle_mouse_event(mouse(MouseEventKind::ScrollUp, 12, 1), &mut scroll);

        assert_eq!(outcome, TranscriptMouseOutcome::Ignored);
        assert_eq!(view.scroll_offset(), before);
    }

    #[test]
    fn mouse_drag_selection_copies_transcript_only_text() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell("alpha beta gamma"))]);
        let _ = render_test_view(&mut view, 20, 3);
        let mut scroll = MouseScrollState::default();

        assert_eq!(
            view.handle_mouse_event(
                mouse(
                    MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    0,
                    2
                ),
                &mut scroll,
            ),
            TranscriptMouseOutcome::SelectionChanged
        );
        assert_eq!(
            view.handle_mouse_event(
                mouse(
                    MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                    5,
                    2
                ),
                &mut scroll,
            ),
            TranscriptMouseOutcome::SelectionChanged
        );
        let outcome = view.handle_mouse_event(
            mouse(
                MouseEventKind::Up(crossterm::event::MouseButton::Left),
                5,
                2,
            ),
            &mut scroll,
        );

        assert_eq!(
            outcome,
            TranscriptMouseOutcome::SelectionCompleted(Some("alpha".to_string()))
        );
    }

    #[test]
    fn selection_copy_preserves_cjk_without_inserted_cell_spaces() {
        let text = "已修复 review 指出的节奏问题";
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell(text))]);
        let _ = render_test_view(&mut view, 40, 3);

        assert_eq!(
            select_columns(&mut view, 0, 0, 0, UnicodeWidthStr::width(text)),
            Some(text.to_string())
        );
    }

    #[test]
    fn selection_copy_slices_cjk_by_display_columns() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell(
            "已修复 review 指出的节奏问题",
        ))]);
        let _ = render_test_view(&mut view, 40, 3);

        assert_eq!(
            select_columns(&mut view, 0, 0, 0, UnicodeWidthStr::width("已修复")),
            Some("已修复".to_string())
        );
    }

    #[test]
    fn selection_copy_cross_line_cjk_ascii_starts_at_selected_column() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(MultiLineTestCell(vec![
            "rollback 后 额外 schedule_frame()，",
            "只会在",
            "autoscroll",
        ]))]);
        let _ = render_test_view(&mut view, 30, 4);

        assert_eq!(
            select_columns(&mut view, 1, 0, 2, UnicodeWidthStr::width("autoscroll"),),
            Some("只会在\nautoscroll".to_string())
        );
    }

    #[test]
    fn selection_copy_does_not_wrap_width_agnostic_long_lines() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(MultiLineTestCell(vec![
            "alpha beta gamma delta epsilon",
        ]))]);
        let _ = render_test_view(&mut view, 10, 3);

        assert_eq!(
            select_columns(&mut view, 0, 0, 1, UnicodeWidthStr::width("alpha")),
            Some("alpha".to_string())
        );
    }

    #[test]
    fn selection_copy_preserves_blank_line_between_cells() {
        let mut view = TranscriptView::new_transcript(vec![
            Arc::new(TestCell("first")) as Arc<dyn HistoryCell>,
            Arc::new(TestCell("second")) as Arc<dyn HistoryCell>,
        ]);
        let _ = render_test_view(&mut view, 20, 4);

        assert_eq!(
            select_columns(&mut view, 0, 0, 2, UnicodeWidthStr::width("second")),
            Some("first\n\nsecond".to_string())
        );
    }

    #[test]
    fn drag_below_view_starts_autoscroll() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(FixedHeightCell(20))]);
        let _ = render_test_view(&mut view, 10, 5);
        view.apply_scroll(TranscriptScroll::Home);
        let _ = render_test_view(&mut view, 10, 5);
        let mut scroll = MouseScrollState::default();

        assert_eq!(
            view.handle_mouse_event(
                mouse(
                    MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    1,
                    1
                ),
                &mut scroll,
            ),
            TranscriptMouseOutcome::SelectionChanged
        );
        assert_eq!(
            view.handle_mouse_event(
                mouse(
                    MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                    1,
                    6
                ),
                &mut scroll,
            ),
            TranscriptMouseOutcome::SelectionChanged
        );

        assert!(view.drag_autoscroll_active());
    }

    #[test]
    fn advance_drag_autoscroll_scrolls_and_extends_selection() {
        let area = Rect::new(0, 0, 10, 5);
        let mut view = TranscriptView::new_transcript(vec![Arc::new(FixedHeightCell(20))]);
        view.render_inline(area, &mut Buffer::empty(area));
        view.apply_scroll(TranscriptScroll::Home);
        view.render_inline(area, &mut Buffer::empty(area));
        let mut scroll = MouseScrollState::default();

        let _ = view.handle_mouse_event(
            mouse(
                MouseEventKind::Down(crossterm::event::MouseButton::Left),
                1,
                1,
            ),
            &mut scroll,
        );
        let _ = view.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                1,
                6,
            ),
            &mut scroll,
        );
        let before_offset = view.scroll_offset();
        let before_head = view.selection.head;

        assert!(view.advance_drag_autoscroll(area));
        view.render_inline(area, &mut Buffer::empty(area));
        assert!(view.scroll_offset() > before_offset);
        assert!(view.selection.head.unwrap().line_index > before_head.unwrap().line_index);
    }

    #[test]
    fn mouse_up_stops_drag_autoscroll() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(FixedHeightCell(20))]);
        let _ = render_test_view(&mut view, 10, 5);
        view.apply_scroll(TranscriptScroll::Home);
        let _ = render_test_view(&mut view, 10, 5);
        let mut scroll = MouseScrollState::default();

        let _ = view.handle_mouse_event(
            mouse(
                MouseEventKind::Down(crossterm::event::MouseButton::Left),
                1,
                1,
            ),
            &mut scroll,
        );
        let _ = view.handle_mouse_event(
            mouse(
                MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                1,
                6,
            ),
            &mut scroll,
        );
        assert!(view.drag_autoscroll_active());

        let _ = view.handle_mouse_event(
            mouse(
                MouseEventKind::Up(crossterm::event::MouseButton::Left),
                1,
                6,
            ),
            &mut scroll,
        );

        assert!(!view.drag_autoscroll_active());
    }

    #[test]
    fn drag_horizontal_outside_clamps_column() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell("alpha beta gamma"))]);
        let _ = render_test_view(&mut view, 10, 3);
        let mut scroll = MouseScrollState::default();

        let _ = view.handle_mouse_event(
            mouse(
                MouseEventKind::Down(crossterm::event::MouseButton::Left),
                1,
                2,
            ),
            &mut scroll,
        );
        assert_eq!(
            view.handle_mouse_event(
                mouse(
                    MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                    99,
                    2
                ),
                &mut scroll,
            ),
            TranscriptMouseOutcome::SelectionChanged
        );
        assert_eq!(view.selection.head.map(|point| point.column), Some(9));
    }

    #[test]
    fn desired_height_saturates_at_u16_max_for_large_transcript() {
        let cells: Vec<Arc<dyn HistoryCell>> = (0..(usize::from(u16::MAX) + 10))
            .map(|_| Arc::new(TestCell("x")) as Arc<dyn HistoryCell>)
            .collect();
        let view = TranscriptView::new_transcript(cells);

        assert_eq!(view.desired_height(10), u16::MAX);
    }

    #[test]
    fn desired_height_returns_exact_sum_below_u16_max() {
        let view = TranscriptView::new_transcript(vec![Arc::new(FixedHeightCell(100))]);

        assert_eq!(view.desired_height(10), 100);
    }

    #[test]
    fn render_inline_bottom_aligns_in_nonzero_area_y() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell("tail"))]);
        let area = Rect::new(0, 4, 8, 3);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 8));

        view.render_inline(area, &mut buf);

        assert_eq!(
            area_lines(&buf, area),
            vec![
                "        ".to_string(),
                "        ".to_string(),
                "tail    ".to_string(),
            ]
        );
    }

    #[test]
    fn render_inline_scrolled_rows_remain_visible_in_nonzero_area_y() {
        let mut view =
            TranscriptView::new_transcript(vec![Arc::new(TestCell("one two three four"))]);
        let area = Rect::new(0, 4, 5, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 8));

        view.set_scroll_offset(1);
        view.render_inline(area, &mut buf);

        assert_eq!(
            area_lines(&buf, area),
            vec!["two  ".to_string(), "three".to_string()]
        );
    }

    fn ineffective_upward_scroll_keeps_follow_bottom(command: TranscriptScroll) {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell("short"))]);
        let area = Rect::new(0, 0, 12, 4);
        let mut buf = Buffer::empty(area);

        view.render_inline(area, &mut buf);
        assert!(view.is_scrolled_to_bottom());

        view.apply_scroll(command);
        assert!(view.is_scrolled_to_bottom());

        for i in 0..8 {
            view.insert_cell(Arc::new(OwnedTestCell(format!("tail-{i}"))));
        }
        let mut buf = Buffer::empty(area);
        view.render_inline(area, &mut buf);

        assert!(view.is_scrolled_to_bottom());
        assert!(
            area_lines(&buf, area)
                .iter()
                .any(|line| line.contains("tail-7")),
            "expected newest tail to remain visible after ineffective upward scroll"
        );
    }

    #[test]
    fn page_up_on_short_transcript_keeps_follow_bottom() {
        ineffective_upward_scroll_keeps_follow_bottom(TranscriptScroll::PageUp);
    }

    #[test]
    fn home_on_short_transcript_keeps_follow_bottom() {
        ineffective_upward_scroll_keeps_follow_bottom(TranscriptScroll::Home);
    }

    #[test]
    fn mouse_wheel_up_on_short_transcript_keeps_follow_bottom() {
        let mut view = TranscriptView::new_transcript(vec![Arc::new(TestCell("short"))]);
        let area = Rect::new(0, 0, 12, 4);
        let mut buf = Buffer::empty(area);

        view.render_inline(area, &mut buf);
        assert!(view.is_scrolled_to_bottom());

        let mut scroll = MouseScrollState::default();
        view.handle_mouse_event(mouse(MouseEventKind::ScrollUp, 1, 1), &mut scroll);
        assert!(view.is_scrolled_to_bottom());

        for i in 0..8 {
            view.insert_cell(Arc::new(OwnedTestCell(format!("tail-{i}"))));
        }
        let mut buf = Buffer::empty(area);
        view.render_inline(area, &mut buf);

        assert!(view.is_scrolled_to_bottom());
        assert!(
            area_lines(&buf, area)
                .iter()
                .any(|line| line.contains("tail-7")),
            "expected newest tail to remain visible after ineffective mouse wheel scroll"
        );
    }

    #[test]
    fn highlight_older_chunk_clears_bottom_stickiness() {
        let cells = (0..12)
            .map(|i| Arc::new(OwnedTestCell(format!("line-{i:02}"))) as Arc<dyn HistoryCell>)
            .collect();
        let mut view = TranscriptView::new_transcript(cells);
        let area = Rect::new(0, 0, 12, 4);
        let mut buf = Buffer::empty(area);

        view.render_inline(area, &mut buf);
        assert!(view.is_scrolled_to_bottom());

        view.set_highlight_cell(Some(0));
        let mut buf = Buffer::empty(area);
        view.render_inline(area, &mut buf);
        assert!(!view.is_scrolled_to_bottom());
        assert!(
            area_lines(&buf, area)
                .iter()
                .any(|line| line.contains("line-00")),
            "expected highlighted older chunk to be visible"
        );

        let mut buf = Buffer::empty(area);
        view.render_inline(area, &mut buf);
        assert!(!view.is_scrolled_to_bottom());
        assert!(
            area_lines(&buf, area)
                .iter()
                .any(|line| line.contains("line-00")),
            "expected highlighted older chunk to remain visible after another frame"
        );
    }
}
