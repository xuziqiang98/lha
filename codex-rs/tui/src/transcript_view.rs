use std::sync::Arc;

use crate::chatwidget::ActiveCellTranscriptKey;
use crate::history_cell::HistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::render::Insets;
use crate::render::renderable::InsetRenderable;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;
use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Text;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

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

#[derive(Default)]
pub(crate) struct TranscriptView {
    cells: Vec<Arc<dyn HistoryCell>>,
    renderables: Vec<Box<dyn Renderable>>,
    scroll_offset: usize,
    last_content_height: Option<usize>,
    last_rendered_height: Option<usize>,
    pending_scroll_chunk: Option<usize>,
    highlight_cell: Option<usize>,
    live_tail_key: Option<LiveTailKey>,
    last_width: Option<u16>,
}

impl TranscriptView {
    pub(crate) fn new(cells: Vec<Arc<dyn HistoryCell>>) -> Self {
        Self {
            renderables: Self::render_cells(&cells, None),
            cells,
            scroll_offset: usize::MAX,
            last_content_height: None,
            last_rendered_height: None,
            pending_scroll_chunk: None,
            highlight_cell: None,
            live_tail_key: None,
            last_width: None,
        }
    }

    pub(crate) fn insert_cell(&mut self, cell: Arc<dyn HistoryCell>) {
        let follow_bottom = self.is_scrolled_to_bottom();
        let had_prior_cells = !self.cells.is_empty();
        let tail_renderable = self.take_live_tail_renderable();
        self.cells.push(cell);
        self.renderables = Self::render_cells(&self.cells, self.highlight_cell);
        if let Some(tail) = tail_renderable {
            let tail = if !had_prior_cells
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
            self.scroll_offset = usize::MAX;
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

        if let Some(key) = next_key {
            let lines = compute_lines(width).unwrap_or_default();
            if !lines.is_empty() {
                self.renderables.push(Self::live_tail_renderable(
                    lines,
                    !self.cells.is_empty(),
                    key.is_stream_continuation,
                ));
            }
        }

        if follow_bottom {
            self.scroll_offset = usize::MAX;
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

    pub(crate) fn apply_scroll(&mut self, command: TranscriptScroll) -> bool {
        let old = self.scroll_offset;
        match command {
            TranscriptScroll::Up => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            TranscriptScroll::Down => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            TranscriptScroll::PageUp => {
                let page = self.page_height();
                self.scroll_offset = self.scroll_offset.saturating_sub(page);
            }
            TranscriptScroll::PageDown => {
                let page = self.page_height();
                self.scroll_offset = self.scroll_offset.saturating_add(page);
            }
            TranscriptScroll::HalfPageUp => {
                let half_page = self.page_height().saturating_add(1) / 2;
                self.scroll_offset = self.scroll_offset.saturating_sub(half_page);
            }
            TranscriptScroll::HalfPageDown => {
                let half_page = self.page_height().saturating_add(1) / 2;
                self.scroll_offset = self.scroll_offset.saturating_add(half_page);
            }
            TranscriptScroll::Home => {
                self.scroll_offset = 0;
            }
            TranscriptScroll::End => {
                self.scroll_offset = usize::MAX;
            }
        }
        self.scroll_offset != old
    }

    pub(crate) fn is_scrolled_to_bottom(&self) -> bool {
        if self.scroll_offset == usize::MAX {
            return true;
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
            self.scroll_offset = usize::MAX;
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
        if let Some(idx) = self.pending_scroll_chunk.take() {
            self.ensure_chunk_visible(idx, area, width);
        }
        self.scroll_offset = self
            .scroll_offset
            .min(content_height.saturating_sub(area.height as usize));

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
    }

    fn render_cells(
        cells: &[Arc<dyn HistoryCell>],
        highlight_cell: Option<usize>,
    ) -> Vec<Box<dyn Renderable>> {
        cells
            .iter()
            .enumerate()
            .map(|(idx, cell)| {
                let mut renderable: Box<dyn Renderable> = if cell.as_any().is::<UserHistoryCell>() {
                    Box::new(CachedRenderable::new(CellRenderable {
                        cell: cell.clone(),
                        style: if highlight_cell == Some(idx) {
                            user_message_style().reversed()
                        } else {
                            user_message_style()
                        },
                    }))
                } else {
                    Box::new(CachedRenderable::new(CellRenderable {
                        cell: cell.clone(),
                        style: Style::default(),
                    }))
                };
                if !cell.is_stream_continuation() && idx > 0 {
                    renderable =
                        Box::new(InsetRenderable::new(renderable, Insets::tlbr(1, 0, 0, 0)));
                }
                renderable
            })
            .collect()
    }

    fn rebuild_renderables(&mut self) {
        let tail_renderable = self.take_live_tail_renderable();
        self.renderables = Self::render_cells(&self.cells, self.highlight_cell);
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
    style: Style,
}

impl Renderable for CellRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(Text::from(self.cell.transcript_lines(area.width)))
            .style(self.style)
            .render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.cell.desired_transcript_height(width)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[derive(Debug)]
    struct TestCell(&'static str);

    #[derive(Debug)]
    struct FixedHeightCell(usize);

    impl HistoryCell for TestCell {
        fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
            textwrap::wrap(self.0, usize::from(width.max(1)))
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

    fn area_lines(buf: &Buffer, area: Rect) -> Vec<String> {
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn resize_keeps_anchor_when_scrolled_up() {
        let mut view = TranscriptView::new(vec![Arc::new(TestCell(
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
    fn desired_height_saturates_at_u16_max_for_large_transcript() {
        let cells: Vec<Arc<dyn HistoryCell>> = (0..(usize::from(u16::MAX) + 10))
            .map(|_| Arc::new(TestCell("x")) as Arc<dyn HistoryCell>)
            .collect();
        let view = TranscriptView::new(cells);

        assert_eq!(view.desired_height(10), u16::MAX);
    }

    #[test]
    fn desired_height_returns_exact_sum_below_u16_max() {
        let view = TranscriptView::new(vec![Arc::new(FixedHeightCell(100))]);

        assert_eq!(view.desired_height(10), 100);
    }

    #[test]
    fn render_inline_bottom_aligns_in_nonzero_area_y() {
        let mut view = TranscriptView::new(vec![Arc::new(TestCell("tail"))]);
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
        let mut view = TranscriptView::new(vec![Arc::new(TestCell("one two three four"))]);
        let area = Rect::new(0, 4, 5, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 8));

        view.set_scroll_offset(1);
        view.render_inline(area, &mut buf);

        assert_eq!(
            area_lines(&buf, area),
            vec!["two  ".to_string(), "three".to_string()]
        );
    }
}
