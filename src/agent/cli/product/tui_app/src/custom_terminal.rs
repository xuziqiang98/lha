// This is derived from `ratatui::Terminal`, which is licensed under the following terms:
//
// The MIT License (MIT)
// Copyright (c) 2016-2022 Florian Dehau
// Copyright (c) 2023-2025 The Ratatui Developers
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
use std::collections::BTreeSet;
use std::io;
use std::io::Write;

use crossterm::cursor::Hide;
use crossterm::cursor::MoveTo;
use crossterm::cursor::Show;
use crossterm::queue;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use ratatui::backend::Backend;
use ratatui::backend::ClearType;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::widgets::WidgetRef;
use unicode_width::UnicodeWidthStr;

fn cursor_after_cell(x: u16, y: u16, symbol: &str) -> Position {
    let width = u16::try_from(symbol.width().max(1)).unwrap_or(u16::MAX);
    Position {
        x: x.saturating_add(width),
        y,
    }
}

fn symbol_contains_terminal_control(symbol: &str) -> bool {
    symbol.chars().any(char::is_control)
}

fn buffer_contains_terminal_control(buffer: &Buffer) -> bool {
    buffer
        .content
        .iter()
        .any(|cell| symbol_contains_terminal_control(cell.symbol()))
}

#[derive(Debug, Hash)]
pub struct Frame<'a> {
    pub(crate) cursor_position: Option<Position>,
    pub(crate) viewport_area: Rect,
    pub(crate) buffer: &'a mut Buffer,
}

impl Frame<'_> {
    pub const fn area(&self) -> Rect {
        self.viewport_area
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn render_widget_ref<W: WidgetRef>(&mut self, widget: W, area: Rect) {
        widget.render_ref(area, self.buffer);
    }

    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) {
        self.cursor_position = Some(position.into());
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct Terminal<B>
where
    B: Backend + Write,
{
    backend: B,
    buffers: [Buffer; 2],
    current: usize,
    pub hidden_cursor: bool,
    pub viewport_area: Rect,
    pub last_known_screen_size: Size,
    pub last_known_cursor_pos: Position,
    clear_tail_after_viewport: bool,
    pending_lifecycle_clear_rows: u16,
    frame_sequence: u64,
}

impl<B> Drop for Terminal<B>
where
    B: Backend + Write,
{
    fn drop(&mut self) {
        if self.hidden_cursor
            && let Err(err) = self.show_cursor()
        {
            tracing::warn!("failed to show terminal cursor during drop: {err}");
        }
    }
}

impl<B> Terminal<B>
where
    B: Backend + Write,
{
    pub fn with_options(mut backend: B) -> io::Result<Self> {
        let screen_size = backend.size()?;
        let cursor_pos = backend.get_cursor_position()?;
        Ok(Self {
            backend,
            buffers: [Buffer::empty(Rect::ZERO), Buffer::empty(Rect::ZERO)],
            current: 0,
            hidden_cursor: false,
            viewport_area: Rect::new(0, cursor_pos.y, 0, 0),
            last_known_screen_size: screen_size,
            last_known_cursor_pos: cursor_pos,
            clear_tail_after_viewport: false,
            pending_lifecycle_clear_rows: 0,
            frame_sequence: 0,
        })
    }

    pub fn get_frame(&mut self) -> Frame<'_> {
        let viewport_area = self.viewport_area;
        Frame {
            cursor_position: None,
            viewport_area,
            buffer: self.current_buffer_mut(),
        }
    }

    fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current]
    }

    fn previous_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[1 - self.current]
    }

    pub const fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    #[cfg(test)]
    pub(crate) fn last_frame_buffer(&self) -> &Buffer {
        &self.buffers[1 - self.current]
    }

    /// Queue only Ratatui's sparse cell diff. Physical erases are reserved for explicit
    /// viewport/lifecycle operations and the narrowed-viewport tail outside the buffer.
    pub fn flush(&mut self) -> io::Result<()> {
        let previous_index = 1 - self.current;
        if buffer_contains_terminal_control(&self.buffers[self.current]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Ratatui cells must not contain terminal control sequences",
            ));
        }
        let updates = self.buffers[previous_index].diff(&self.buffers[self.current]);

        let changed_cells = updates.len();
        let changed_rows = updates
            .iter()
            .map(|(_, y, _)| *y)
            .collect::<BTreeSet<_>>()
            .len();
        if let Some((x, y, cell)) = updates.last() {
            self.last_known_cursor_pos = cursor_after_cell(*x, *y, cell.symbol());
        }

        let tail_clear_rows = if self.clear_tail_after_viewport {
            let screen_size = self.size()?;
            let tail_x = self.viewport_area.right();
            if tail_x < screen_size.width {
                let bottom = self.viewport_area.bottom().min(screen_size.height);
                for y in self.viewport_area.y..bottom {
                    queue!(
                        self.backend,
                        MoveTo(tail_x, y),
                        SetAttribute(crossterm::style::Attribute::Reset),
                        SetForegroundColor(crossterm::style::Color::Reset),
                        SetBackgroundColor(crossterm::style::Color::Reset),
                        Clear(crossterm::terminal::ClearType::UntilNewLine)
                    )?;
                }
                bottom.saturating_sub(self.viewport_area.y)
            } else {
                0
            }
        } else {
            0
        };

        let result = self.backend.draw(updates.into_iter());
        tracing::debug!(
            target: "lha_tui::render",
            frame = self.frame_sequence,
            viewport_x = self.viewport_area.x,
            viewport_y = self.viewport_area.y,
            viewport_width = self.viewport_area.width,
            viewport_height = self.viewport_area.height,
            changed_cells,
            changed_rows,
            lifecycle_clear_rows = self.pending_lifecycle_clear_rows,
            tail_clear_rows,
            draw_ok = result.is_ok(),
            "queued TUI frame"
        );
        result
    }

    pub fn resize(&mut self, screen_size: Size) -> io::Result<()> {
        if screen_size != self.last_known_screen_size {
            self.clear()?;
        }
        self.last_known_screen_size = screen_size;
        Ok(())
    }

    pub fn set_viewport_area(&mut self, area: Rect) {
        if area.right() < self.viewport_area.right() {
            self.clear_tail_after_viewport = true;
        }
        self.current_buffer_mut().resize(area);
        self.previous_buffer_mut().resize(area);
        self.viewport_area = area;
    }

    pub fn autoresize(&mut self) -> io::Result<()> {
        let screen_size = self.size()?;
        if screen_size != self.last_known_screen_size {
            self.resize(screen_size)?;
        }
        Ok(())
    }

    pub fn draw<F>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.try_draw(|frame| {
            render_callback(frame);
            io::Result::Ok(())
        })
    }

    pub fn try_draw<F, E>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame) -> Result<(), E>,
        E: Into<io::Error>,
    {
        self.try_draw_unflushed(render_callback)?;
        let result = Backend::flush(&mut self.backend);
        self.complete_frame(result)
    }

    pub(crate) fn try_draw_unflushed<F, E>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame) -> Result<(), E>,
        E: Into<io::Error>,
    {
        self.autoresize()?;
        let mut frame = self.get_frame();
        render_callback(&mut frame).map_err(Into::into)?;
        let cursor_position = frame.cursor_position;
        self.queue_frame(cursor_position)
    }

    fn queue_frame(&mut self, cursor_position: Option<Position>) -> io::Result<()> {
        self.frame_sequence = self.frame_sequence.saturating_add(1);
        self.queue_hide_cursor()?;
        self.flush()?;

        if let Some(position) = cursor_position {
            self.queue_set_cursor_position(position)?;
            self.queue_show_cursor()?;
        }
        Ok(())
    }

    pub(crate) fn complete_frame(&mut self, result: io::Result<()>) -> io::Result<()> {
        tracing::debug!(
            target: "lha_tui::render",
            frame = self.frame_sequence,
            flush_ok = result.is_ok(),
            "flushed TUI frame"
        );
        result?;

        self.swap_buffers();
        self.clear_tail_after_viewport = false;
        self.pending_lifecycle_clear_rows = 0;
        Ok(())
    }

    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.hidden_cursor = true;
        Ok(())
    }

    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()?;
        self.hidden_cursor = false;
        Ok(())
    }

    fn queue_hide_cursor(&mut self) -> io::Result<()> {
        queue!(self.backend, Hide)?;
        self.hidden_cursor = true;
        Ok(())
    }

    fn queue_show_cursor(&mut self) -> io::Result<()> {
        queue!(self.backend, Show)?;
        self.hidden_cursor = false;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.backend.get_cursor_position()
    }

    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.backend.set_cursor_position(position)?;
        self.last_known_cursor_pos = position;
        Ok(())
    }

    fn queue_set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        queue!(self.backend, MoveTo(position.x, position.y))?;
        self.last_known_cursor_pos = position;
        Ok(())
    }

    pub fn clear(&mut self) -> io::Result<()> {
        let area = self.viewport_area;
        self.clear_area(area)
    }

    pub(crate) fn clear_area(&mut self, area: Rect) -> io::Result<()> {
        if area.is_empty() {
            return Ok(());
        }

        let size = self.size()?;
        if area.x >= size.width || area.y >= size.height {
            return Ok(());
        }

        let bottom = area.bottom().min(size.height);
        for y in area.y..bottom {
            queue!(
                self.backend,
                MoveTo(area.x, y),
                SetAttribute(crossterm::style::Attribute::Reset),
                SetForegroundColor(crossterm::style::Color::Reset),
                SetBackgroundColor(crossterm::style::Color::Reset),
                Clear(crossterm::terminal::ClearType::UntilNewLine)
            )?;
        }

        self.pending_lifecycle_clear_rows = self
            .pending_lifecycle_clear_rows
            .saturating_add(bottom.saturating_sub(area.y));
        self.previous_buffer_mut().reset();
        Ok(())
    }

    pub fn clear_scrollback(&mut self) -> io::Result<()> {
        if self.viewport_area.is_empty() {
            return Ok(());
        }
        queue!(
            self.backend,
            SetAttribute(crossterm::style::Attribute::Reset),
            SetForegroundColor(crossterm::style::Color::Reset),
            SetBackgroundColor(crossterm::style::Color::Reset),
            Clear(crossterm::terminal::ClearType::Purge)
        )?;
        self.clear()?;
        Write::flush(&mut self.backend)?;
        self.pending_lifecycle_clear_rows = 0;
        Ok(())
    }

    pub fn swap_buffers(&mut self) {
        self.previous_buffer_mut().reset();
        self.current = 1 - self.current;
    }

    pub fn size(&self) -> io::Result<Size> {
        self.backend.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;
    use ratatui::style::Style;

    #[derive(Debug)]
    struct RecordingBackend {
        output: Vec<u8>,
        size: Size,
        cursor_position: Position,
        fail_next_flush: bool,
        draw_coordinates: Vec<(u16, u16)>,
    }

    impl RecordingBackend {
        fn new(width: u16, height: u16) -> Self {
            Self {
                output: Vec::new(),
                size: Size::new(width, height),
                cursor_position: Position::ORIGIN,
                fail_next_flush: false,
                draw_coordinates: Vec::new(),
            }
        }

        fn clear_output(&mut self) {
            self.output.clear();
        }

        fn emitted_clear_to_end(&self) -> bool {
            self.output.windows(3).any(|bytes| bytes == b"\x1b[K")
        }
    }

    impl Write for RecordingBackend {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Backend for RecordingBackend {
        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
        {
            self.draw_coordinates
                .extend(content.map(|(x, y, _)| (x, y)));
            Ok(())
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            queue!(self, Hide)
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            queue!(self, Show)
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Ok(self.cursor_position)
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            self.cursor_position = position.into();
            Ok(())
        }

        fn clear(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn clear_region(&mut self, _clear_type: ClearType) -> io::Result<()> {
            Ok(())
        }

        fn size(&self) -> io::Result<Size> {
            Ok(self.size)
        }

        fn window_size(&mut self) -> io::Result<ratatui::backend::WindowSize> {
            Ok(ratatui::backend::WindowSize {
                columns_rows: self.size,
                pixels: Size::ZERO,
            })
        }

        fn flush(&mut self) -> io::Result<()> {
            if std::mem::take(&mut self.fail_next_flush) {
                Err(io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }

        fn scroll_region_up(
            &mut self,
            _region: std::ops::Range<u16>,
            _scroll_by: u16,
        ) -> io::Result<()> {
            Ok(())
        }

        fn scroll_region_down(
            &mut self,
            _region: std::ops::Range<u16>,
            _scroll_by: u16,
        ) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn ordinary_ascii_change_draws_only_changed_cell() {
        let backend = crate::product::tui_app::test_backend::AuditedVT100Backend::new(20, 2);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "abc", Style::default()))
            .unwrap();
        terminal.backend_mut().clear_frames();

        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "axc", Style::default()))
            .unwrap();

        let frame = terminal.backend().last_frame().expect("ASCII frame");
        assert_eq!(frame.draw_coordinates, vec![(1, 0)]);
        assert_eq!(frame.stats.printed_columns, 1);
        assert!(
            frame.stats.erase_line_rows.is_empty(),
            "{}",
            frame.escaped_ansi()
        );
        assert_eq!(frame.stats.erase_display_count, 0);
    }

    #[test]
    fn status_animation_does_not_draw_unchanged_cjk_row() {
        let backend = crate::product::tui_app::test_backend::AuditedVT100Backend::new(80, 4);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 80, 4));
        terminal
            .draw(|frame| {
                frame
                    .buffer
                    .set_string(0, 0, "稳定中文回答", Style::default());
                frame.buffer.set_string(0, 3, "Working", Style::default());
            })
            .unwrap();
        let before = terminal.last_frame_buffer().clone();
        terminal.backend_mut().clear_frames();

        terminal
            .draw(|frame| {
                frame
                    .buffer
                    .set_string(0, 0, "稳定中文回答", Style::default());
                frame
                    .buffer
                    .set_string(0, 3, "Working", Style::default().fg(Color::Blue));
            })
            .unwrap();
        let after = terminal.last_frame_buffer().clone();

        let frame = terminal.backend().last_frame().expect("status frame");
        let audit =
            crate::product::tui_app::test_backend::analyze_buffer_frame(&before, &after, frame);
        assert_eq!(audit.changed_rows, BTreeSet::from([3]));
        assert!(
            audit.stable_rows_touched.is_empty(),
            "{}",
            frame.escaped_ansi()
        );
        assert!(
            audit.stable_cjk_rows_touched.is_empty(),
            "{}",
            frame.escaped_ansi()
        );
        assert_eq!(frame.stats.erase_display_count, 0);
        assert!(
            frame.stats.erase_line_rows.is_empty(),
            "{}",
            frame.escaped_ansi()
        );
    }

    #[test]
    fn stock_diff_handles_wide_cell_transitions() {
        for (before, after) in [("中", "ab"), ("ab", "中"), ("中文", "中")] {
            let backend = crate::product::tui_app::test_backend::AuditedVT100Backend::new(20, 1);
            let mut terminal = Terminal::with_options(backend).unwrap();
            terminal.set_viewport_area(Rect::new(0, 0, 20, 1));
            terminal
                .draw(|frame| frame.buffer.set_string(0, 0, before, Style::default()))
                .unwrap();
            terminal
                .draw(|frame| frame.buffer.set_string(0, 0, after, Style::default()))
                .unwrap();
            let stage = format!("{before:?} -> {after:?}");
            let frame = terminal.backend().last_frame().expect("wide-cell frame");
            let diagnostic = frame.diagnostic(&stage);
            crate::product::tui_app::test_backend::assert_vt100_grid_matches_buffer(
                &stage,
                terminal.last_frame_buffer(),
                terminal.backend().vt100(),
                &diagnostic,
            );
        }
    }

    #[test]
    fn no_op_frame_emits_no_print_or_erase() {
        let backend = crate::product::tui_app::test_backend::AuditedVT100Backend::new(20, 2);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "unchanged", Style::default()))
            .unwrap();
        terminal.backend_mut().clear_frames();
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "unchanged", Style::default()))
            .unwrap();
        let frame = terminal.backend().last_frame().expect("no-op frame");
        assert!(
            frame.draw_coordinates.is_empty(),
            "{}",
            frame.escaped_ansi()
        );
        assert_eq!(frame.stats.printed_columns, 0);
        assert!(frame.stats.erase_line_rows.is_empty());
        assert_eq!(frame.stats.erase_display_count, 0);
    }

    #[test]
    fn explicit_background_blank_cells_round_trip() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(6, 1);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 6, 1));
        terminal
            .draw(|frame| {
                for x in 0..6 {
                    frame.buffer[(x, 0)].set_bg(Color::Blue);
                }
            })
            .unwrap();
        assert!((0..6).all(|x| {
            terminal
                .backend()
                .vt100()
                .screen()
                .cell(0, x)
                .unwrap()
                .bgcolor()
                == vt100::Color::Idx(4)
        }));
        terminal.draw(|_| {}).unwrap();
        let screen = terminal.backend().vt100().screen();
        assert!((0..6).all(|x| screen.cell(0, x).unwrap().bgcolor() == vt100::Color::Default));
    }

    #[test]
    fn narrowing_viewport_clears_tail_once() {
        let backend = RecordingBackend::new(20, 2);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        terminal.draw(|_| {}).unwrap();
        terminal.backend_mut().clear_output();
        terminal.set_viewport_area(Rect::new(0, 0, 10, 2));

        terminal.draw(|_| {}).unwrap();
        let first = terminal.backend().output.clone();
        terminal.backend_mut().clear_output();
        terminal.draw(|_| {}).unwrap();

        assert!(
            first.windows(3).any(|bytes| bytes == b"\x1b[K"),
            "{:?}",
            String::from_utf8_lossy(&first)
        );
        assert!(!terminal.backend().emitted_clear_to_end());
    }

    #[test]
    fn pending_tail_clear_survives_flush_failure() {
        let backend = RecordingBackend::new(20, 2);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        terminal.draw(|_| {}).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 10, 2));
        terminal.backend_mut().fail_next_flush = true;
        assert!(terminal.draw(|_| {}).is_err());

        terminal.backend_mut().clear_output();
        terminal.draw(|_| {}).unwrap();
        assert!(
            terminal.backend().emitted_clear_to_end(),
            "{:?}",
            String::from_utf8_lossy(&terminal.backend().output)
        );
    }

    #[test]
    fn failed_flush_does_not_commit_buffer_state() {
        let backend = RecordingBackend::new(20, 1);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 20, 1));
        terminal.backend_mut().fail_next_flush = true;

        assert!(
            terminal
                .draw(|frame| frame.buffer.set_string(0, 0, "x", Style::default()))
                .is_err()
        );
        assert_eq!(terminal.backend().draw_coordinates, vec![(0, 0)]);

        terminal.backend_mut().draw_coordinates.clear();
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "x", Style::default()))
            .unwrap();
        assert_eq!(terminal.backend().draw_coordinates, vec![(0, 0)]);

        terminal.backend_mut().draw_coordinates.clear();
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "x", Style::default()))
            .unwrap();
        assert!(terminal.backend().draw_coordinates.is_empty());
    }

    #[test]
    fn rendered_buffers_contain_no_terminal_control_sequences() {
        for (symbol, skip) in [
            ("\x1b]8;;https://example.test\x07x", false),
            ("\u{009b}2J", false),
            ("\u{009d}9;notification\u{009c}", false),
            ("\n", false),
            ("\r", false),
            ("\t", false),
            ("\x1b[2J", true),
        ] {
            let backend = RecordingBackend::new(20, 1);
            let mut terminal = Terminal::with_options(backend).unwrap();
            terminal.set_viewport_area(Rect::new(0, 0, 20, 1));
            let error = terminal
                .draw(|frame| {
                    let cell = &mut frame.buffer[(0, 0)];
                    cell.set_symbol(symbol);
                    cell.skip = skip;
                })
                .expect_err("control sequence should be rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{symbol:?}");
        }
    }

    #[test]
    fn resize_clear_then_returns_to_sparse_diff() {
        let backend = crate::product::tui_app::test_backend::AuditedVT100Backend::new(20, 2);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "stable", Style::default()))
            .unwrap();

        terminal.backend_mut().set_size(16, 2);
        terminal.resize(Size::new(16, 2)).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 16, 2));
        terminal.backend_mut().clear_frames();
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "stable", Style::default()))
            .unwrap();
        let restored = terminal.backend().last_frame().expect("restored frame");
        assert_eq!(restored.stats.erase_line_rows, BTreeSet::from([0, 1]));
        assert!(restored.stats.printed_columns >= "stable".len());

        terminal.backend_mut().clear_frames();
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "stable", Style::default()))
            .unwrap();
        let no_op = terminal
            .backend()
            .last_frame()
            .expect("no-op after restore");
        assert_eq!(no_op.stats.printed_columns, 0);
        assert!(no_op.stats.erase_line_rows.is_empty());
        assert_eq!(no_op.stats.erase_display_count, 0);

        terminal.backend_mut().clear_frames();
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "staple", Style::default()))
            .unwrap();
        let sparse = terminal.backend().last_frame().expect("sparse frame");
        assert_eq!(sparse.draw_coordinates, vec![(3, 0)]);
        assert_eq!(sparse.stats.printed_columns, 1);
        assert!(sparse.stats.erase_line_rows.is_empty());
        assert_eq!(sparse.stats.erase_display_count, 0);
    }

    #[test]
    fn clear_scrollback_also_clears_and_restores_visible_viewport() {
        let backend = crate::product::tui_app::test_backend::AuditedVT100Backend::new(20, 2);
        let mut terminal = Terminal::with_options(backend).unwrap();
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "visible", Style::default()))
            .unwrap();

        terminal.backend_mut().clear_frames();
        terminal.clear_scrollback().unwrap();
        let clear = terminal
            .backend()
            .last_frame()
            .expect("scrollback clear frame");
        assert_eq!(clear.stats.erase_display_count, 1);
        assert_eq!(clear.stats.erase_line_rows, BTreeSet::from([0, 1]));
        assert!(
            terminal
                .backend()
                .vt100()
                .screen()
                .contents()
                .trim()
                .is_empty()
        );

        terminal.backend_mut().clear_frames();
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "visible", Style::default()))
            .unwrap();
        assert!(
            terminal
                .backend()
                .vt100()
                .screen()
                .contents()
                .starts_with("visible")
        );

        terminal.backend_mut().clear_frames();
        terminal
            .draw(|frame| frame.buffer.set_string(0, 0, "visible", Style::default()))
            .unwrap();
        let no_op = terminal.backend().last_frame().expect("post-clear no-op");
        assert_eq!(no_op.stats.printed_columns, 0);
        assert!(no_op.stats.erase_line_rows.is_empty());
        assert_eq!(no_op.stats.erase_display_count, 0);
    }
}
