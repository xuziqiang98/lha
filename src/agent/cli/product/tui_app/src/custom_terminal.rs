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
use std::io;
use std::io::Write;

use crossterm::cursor::Hide;
use crossterm::cursor::MoveTo;
use crossterm::cursor::Show;
use crossterm::queue;
use crossterm::style::Colors;
use crossterm::style::Print;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetColors;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use derive_more::IsVariant;
use ratatui::backend::Backend;
use ratatui::backend::ClearType;
use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::widgets::WidgetRef;
use unicode_width::UnicodeWidthStr;

fn display_width(symbol: &str) -> usize {
    if !symbol.contains('\x1b') {
        return symbol.width();
    }

    // OSC escape sequences are terminal controls and do not consume columns.
    let mut visible = String::with_capacity(symbol.len());
    let mut chars = symbol.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&']') {
            chars.next();
            while let Some(ch) = chars.next() {
                if ch == '\x07' {
                    break;
                }
                if ch == '\x1b' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
        } else {
            visible.push(ch);
        }
    }
    visible.width()
}

fn symbol_has_physical_repaint_risk(symbol: &str) -> bool {
    let width = display_width(symbol);
    symbol.contains('\x1b')
        || !symbol.is_ascii()
        || width == 0
        || width > 1
        || width != symbol.chars().count()
}

fn cursor_after_put(x: u16, y: u16, symbol: &str) -> Position {
    let width = u16::try_from(display_width(symbol).max(1)).unwrap_or(u16::MAX);
    Position {
        x: x.saturating_add(width),
        y,
    }
}

#[derive(Debug, Hash)]
pub struct Frame<'a> {
    /// Where should the cursor be after drawing this frame?
    ///
    /// If `None`, the cursor is hidden and its position is controlled by the backend. If `Some((x,
    /// y))`, the cursor is shown and placed at `(x, y)` after the call to `Terminal::draw()`.
    pub(crate) cursor_position: Option<Position>,

    /// The area of the viewport
    pub(crate) viewport_area: Rect,

    /// The buffer that is used to draw the current frame
    pub(crate) buffer: &'a mut Buffer,
}

impl Frame<'_> {
    /// The area of the current frame
    ///
    /// This is guaranteed not to change during rendering, so may be called multiple times.
    ///
    /// If your app listens for a resize event from the backend, it should ignore the values from
    /// the event for any calculations that are used to render the current frame and use this value
    /// instead as this is the area of the buffer that is used to render the current frame.
    pub const fn area(&self) -> Rect {
        self.viewport_area
    }

    /// Render a [`WidgetRef`] to the current buffer using [`WidgetRef::render_ref`].
    ///
    /// Usually the area argument is the size of the current frame or a sub-area of the current
    /// frame (which can be obtained using [`Layout`] to split the total area).
    #[allow(clippy::needless_pass_by_value)]
    pub fn render_widget_ref<W: WidgetRef>(&mut self, widget: W, area: Rect) {
        widget.render_ref(area, self.buffer);
    }

    /// After drawing this frame, make the cursor visible and put it at the specified (x, y)
    /// coordinates. If this method is not called, the cursor will be hidden.
    ///
    /// Note that this will interfere with calls to [`Terminal::hide_cursor`],
    /// [`Terminal::show_cursor`], and [`Terminal::set_cursor_position`]. Pick one of the APIs and
    /// stick with it.
    ///
    /// [`Terminal::hide_cursor`]: crate::product::tui_app::Terminal::hide_cursor
    /// [`Terminal::show_cursor`]: crate::product::tui_app::Terminal::show_cursor
    /// [`Terminal::set_cursor_position`]: crate::product::tui_app::Terminal::set_cursor_position
    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) {
        self.cursor_position = Some(position.into());
    }

    /// Gets the buffer that this `Frame` draws into as a mutable reference.
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct Terminal<B>
where
    B: Backend + Write,
{
    /// The backend used to interface with the terminal
    backend: B,
    /// Holds the results of the current and previous draw calls. The two are compared at the end
    /// of each draw pass to output the necessary updates to the terminal
    buffers: [Buffer; 2],
    /// Index of the current buffer in the previous array
    current: usize,
    /// Whether the cursor is currently hidden
    pub hidden_cursor: bool,
    /// Area of the viewport
    pub viewport_area: Rect,
    /// Last known size of the terminal. Used to detect if the internal buffers have to be resized.
    pub last_known_screen_size: Size,
    /// Last known position of the cursor. Used to find the new area when the viewport is inlined
    /// and the terminal resized.
    pub last_known_cursor_pos: Position,
    /// Whether the next flush should clear cells to the right of a narrowed viewport.
    clear_tail_after_viewport: bool,
    /// Whether the next flush should treat every viewport row as dirty.
    force_full_viewport_repaint: bool,
}

impl<B> Drop for Terminal<B>
where
    B: Backend,
    B: Write,
{
    #[allow(clippy::print_stderr)]
    fn drop(&mut self) {
        // Attempt to restore the cursor state
        if self.hidden_cursor
            && let Err(err) = self.show_cursor()
        {
            eprintln!("Failed to show the cursor: {err}");
        }
    }
}

impl<B> Terminal<B>
where
    B: Backend,
    B: Write,
{
    /// Creates a new [`Terminal`] with the given [`Backend`] and [`TerminalOptions`].
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
            force_full_viewport_repaint: false,
        })
    }

    /// Get a Frame object which provides a consistent view into the terminal state for rendering.
    pub fn get_frame(&mut self) -> Frame<'_> {
        Frame {
            cursor_position: None,
            viewport_area: self.viewport_area,
            buffer: self.current_buffer_mut(),
        }
    }

    /// Gets the current buffer as a reference.
    fn current_buffer(&self) -> &Buffer {
        &self.buffers[self.current]
    }

    /// Gets the current buffer as a mutable reference.
    fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current]
    }

    /// Gets the previous buffer as a reference.
    fn previous_buffer(&self) -> &Buffer {
        &self.buffers[1 - self.current]
    }

    /// Gets the previous buffer as a mutable reference.
    fn previous_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[1 - self.current]
    }

    /// Gets the backend
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// Gets the backend as a mutable reference
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Obtains a difference between the previous and the current buffer and passes it to the
    /// current backend for drawing.
    pub fn flush(&mut self) -> io::Result<()> {
        let clear_tail_after_viewport = self.clear_tail_after_viewport;
        let screen_size = if clear_tail_after_viewport {
            Some(self.size()?)
        } else {
            None
        };
        let force_full_repaint = self.force_full_viewport_repaint;
        let mut updates = diff_buffers_with_full_repaint(
            self.previous_buffer(),
            self.current_buffer(),
            force_full_repaint,
        );
        if let Some(screen_size) = screen_size {
            let tail_x = self.viewport_area.right();
            if tail_x < screen_size.width {
                let bottom = self.viewport_area.bottom().min(screen_size.height);
                for y in self.viewport_area.y..bottom {
                    updates.push(DrawCommand::ClearToEnd {
                        x: tail_x,
                        y,
                        bg: Color::Reset,
                    });
                }
            }
            self.clear_tail_after_viewport = false;
        }
        if let Some(DrawCommand::Put { x, y, cell }) =
            updates.iter().rfind(|command| command.is_put())
        {
            self.last_known_cursor_pos = cursor_after_put(*x, *y, cell.symbol());
        }
        let result = draw(&mut self.backend, updates.into_iter());
        if result.is_ok() {
            self.force_full_viewport_repaint = false;
        }
        result
    }

    /// Updates the Terminal so that internal buffers match the requested area.
    ///
    /// Requested area will be saved to remain consistent when rendering. This leads to a full clear
    /// of the screen.
    pub fn resize(&mut self, screen_size: Size) -> io::Result<()> {
        if screen_size != self.last_known_screen_size {
            self.clear()?;
            self.invalidate_viewport();
        }
        self.last_known_screen_size = screen_size;
        Ok(())
    }

    /// Sets the viewport area.
    pub fn set_viewport_area(&mut self, area: Rect) {
        if area.right() < self.viewport_area.right() {
            self.clear_tail_after_viewport = true;
        }
        self.current_buffer_mut().resize(area);
        self.previous_buffer_mut().resize(area);
        self.viewport_area = area;
    }

    /// Queries the backend for size and resizes if it doesn't match the previous size.
    pub fn autoresize(&mut self) -> io::Result<()> {
        let screen_size = self.size()?;
        if screen_size != self.last_known_screen_size {
            self.resize(screen_size)?;
        }
        Ok(())
    }

    /// Draws a single frame to the terminal.
    ///
    /// Returns a [`CompletedFrame`] if successful, otherwise a [`std::io::Error`].
    ///
    /// If the render callback passed to this method can fail, use [`try_draw`] instead.
    ///
    /// Applications should call `draw` or [`try_draw`] in a loop to continuously render the
    /// terminal. These methods are the main entry points for drawing to the terminal.
    ///
    /// [`try_draw`]: Terminal::try_draw
    ///
    /// This method will:
    ///
    /// - autoresize the terminal if necessary
    /// - call the render callback, passing it a [`Frame`] reference to render to
    /// - flush the current internal state by copying the current buffer to the backend
    /// - move the cursor to the last known position if it was set during the rendering closure
    ///
    /// The render callback should fully render the entire frame when called, including areas that
    /// are unchanged from the previous frame. This is because each frame is compared to the
    /// previous frame to determine what has changed, and only the changes are written to the
    /// terminal. If the render callback does not fully render the frame, the terminal will not be
    /// in a consistent state.
    pub fn draw<F>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.try_draw(|frame| {
            render_callback(frame);
            io::Result::Ok(())
        })
    }

    /// Tries to draw a single frame to the terminal.
    ///
    /// Returns [`Result::Ok`] containing a [`CompletedFrame`] if successful, otherwise
    /// [`Result::Err`] containing the [`std::io::Error`] that caused the failure.
    ///
    /// This is the equivalent of [`Terminal::draw`] but the render callback is a function or
    /// closure that returns a `Result` instead of nothing.
    ///
    /// Applications should call `try_draw` or [`draw`] in a loop to continuously render the
    /// terminal. These methods are the main entry points for drawing to the terminal.
    ///
    /// [`draw`]: Terminal::draw
    ///
    /// This method will:
    ///
    /// - autoresize the terminal if necessary
    /// - call the render callback, passing it a [`Frame`] reference to render to
    /// - flush the current internal state by copying the current buffer to the backend
    /// - move the cursor to the last known position if it was set during the rendering closure
    /// - return a [`CompletedFrame`] with the current buffer and the area of the terminal
    ///
    /// The render callback passed to `try_draw` can return any [`Result`] with an error type that
    /// can be converted into an [`std::io::Error`] using the [`Into`] trait. This makes it possible
    /// to use the `?` operator to propagate errors that occur during rendering. If the render
    /// callback returns an error, the error will be returned from `try_draw` as an
    /// [`std::io::Error`] and the terminal will not be updated.
    ///
    /// The [`CompletedFrame`] returned by this method can be useful for debugging or testing
    /// purposes, but it is often not used in regular applicationss.
    ///
    /// The render callback should fully render the entire frame when called, including areas that
    /// are unchanged from the previous frame. This is because each frame is compared to the
    /// previous frame to determine what has changed, and only the changes are written to the
    /// terminal. If the render function does not fully render the frame, the terminal will not be
    /// in a consistent state.
    pub fn try_draw<F, E>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame) -> Result<(), E>,
        E: Into<io::Error>,
    {
        // Autoresize - otherwise we get glitches if shrinking or potential desync between widgets
        // and the terminal (if growing), which may OOB.
        self.autoresize()?;

        let mut frame = self.get_frame();

        render_callback(&mut frame).map_err(Into::into)?;

        // We can't change the cursor position right away because we have to flush the frame to
        // stdout first. But we also can't keep the frame around, since it holds a &mut to
        // Buffer. Thus, we're taking the important data out of the Frame and dropping it.
        let cursor_position = frame.cursor_position;

        self.queue_hide_cursor()?;

        // Draw to stdout
        self.flush()?;

        match cursor_position {
            None => {}
            Some(position) => {
                self.queue_set_cursor_position(position)?;
                self.queue_show_cursor()?;
            }
        }

        self.swap_buffers();

        Backend::flush(&mut self.backend)?;

        Ok(())
    }

    /// Hides the cursor.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.hidden_cursor = true;
        Ok(())
    }

    /// Shows the cursor.
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

    /// Gets the current cursor position.
    ///
    /// This is the position of the cursor after the last draw call.
    #[allow(dead_code)]
    pub fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.backend.get_cursor_position()
    }

    /// Sets the cursor position.
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

    /// Clear the terminal and force a full redraw on the next draw call.
    pub fn clear(&mut self) -> io::Result<()> {
        if self.viewport_area.is_empty() {
            return Ok(());
        }
        self.backend
            .set_cursor_position(self.viewport_area.as_position())?;
        self.backend.clear_region(ClearType::AfterCursor)?;
        // Reset the back buffer to make sure the next update will redraw everything.
        self.previous_buffer_mut().reset();
        Ok(())
    }

    /// Clear a visible terminal area and force a full redraw on the next draw call.
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

        self.previous_buffer_mut().reset();
        Ok(())
    }

    /// Force the next draw pass to repaint the whole viewport.
    pub fn invalidate_viewport(&mut self) {
        self.previous_buffer_mut().reset();
        self.force_full_viewport_repaint = true;
    }

    /// Clear terminal scrollback (if supported) and force a full redraw.
    pub fn clear_scrollback(&mut self) -> io::Result<()> {
        if self.viewport_area.is_empty() {
            return Ok(());
        }
        self.backend
            .set_cursor_position(self.viewport_area.as_position())?;
        queue!(self.backend, Clear(crossterm::terminal::ClearType::Purge))?;
        std::io::Write::flush(&mut self.backend)?;
        self.previous_buffer_mut().reset();
        Ok(())
    }

    /// Clears the inactive buffer and swaps it with the current buffer
    pub fn swap_buffers(&mut self) {
        self.previous_buffer_mut().reset();
        self.current = 1 - self.current;
    }

    /// Queries the real size of the backend.
    pub fn size(&self) -> io::Result<Size> {
        self.backend.size()
    }
}

#[derive(Debug, IsVariant)]
enum DrawCommand {
    Put { x: u16, y: u16, cell: Cell },
    ClearToEnd { x: u16, y: u16, bg: Color },
}

fn diff_buffers(a: &Buffer, b: &Buffer) -> Vec<DrawCommand> {
    diff_buffers_with_full_repaint(a, b, false)
}

fn diff_buffers_with_full_repaint(
    a: &Buffer,
    b: &Buffer,
    force_full_repaint: bool,
) -> Vec<DrawCommand> {
    let previous_buffer = &a.content;
    let next_buffer = &b.content;

    let mut updates = vec![];
    for y in 0..a.area.height {
        let row_start = y as usize * a.area.width as usize;
        let row_end = row_start + a.area.width as usize;
        let row = &next_buffer[row_start..row_end];
        let previous_row = &previous_buffer[row_start..row_end];
        if row.is_empty() {
            continue;
        }
        let bg = row.last().map(|cell| cell.bg).unwrap_or(Color::Reset);

        // Scan the row to find the rightmost column that still matters: any non-space glyph,
        // any cell whose bg differs from the row's trailing bg, or any cell with modifiers.
        // Multi-width glyphs extend that region through their full displayed width.
        // Rows are dirty only when the logical buffer changed or an explicit viewport
        // invalidation requests a repaint. Wide/CJK/OSC cells affect how we repaint a dirty row,
        // not whether an unchanged row should be repainted every animation frame; physical screen
        // divergence should be repaired with an explicit viewport invalidation.
        // Default-background dirty rows are cleared from column zero before repainting so stale
        // wide-cell fragments cannot survive inside the text span. Rows with explicit background
        // colors are repainted cell-by-cell because some terminals and multiplexers handle colored
        // erase inconsistently for blank rows.
        let paints_explicit_background = bg != Color::Reset;
        let mut last_nonblank_column = if paints_explicit_background {
            row.len().saturating_sub(1)
        } else {
            0
        };
        let mut column = 0usize;
        while column < row.len() {
            let cell = &row[column];
            let width = display_width(cell.symbol());
            if cell.symbol() != " " || cell.bg != bg || cell.modifier != Modifier::empty() {
                last_nonblank_column = last_nonblank_column.max(column + (width.saturating_sub(1)));
            }
            column += width.max(1); // treat zero-width symbols as width 1
        }

        let row_changed = row
            .iter()
            .zip(previous_row.iter())
            .any(|(current, previous)| current != previous);
        let row_is_dirty = force_full_repaint || row_changed;
        if !row_is_dirty {
            continue;
        }

        if !paints_explicit_background {
            let (x, y) = a.pos_of(row_start);
            updates.push(DrawCommand::ClearToEnd { x, y, bg });
        }

        // Repaint dirty rows left-to-right instead of sending only sparse changed cells. This
        // repairs rare cases where the real terminal screen has stale cells even though the
        // previous in-memory buffer still matches most of the row.
        let repaint_end = if paints_explicit_background {
            row.len().saturating_sub(1)
        } else {
            last_nonblank_column
        };
        let mut to_skip = 0usize;
        for column in 0..=repaint_end {
            let i = row_start + column;
            if !next_buffer[i].skip && to_skip == 0 {
                let (x, y) = a.pos_of(i);
                updates.push(DrawCommand::Put {
                    x,
                    y,
                    cell: next_buffer[i].clone(),
                });
            }
            to_skip = display_width(next_buffer[i].symbol()).saturating_sub(1);
        }
    }
    updates
}

fn draw<I>(writer: &mut impl Write, commands: I) -> io::Result<()>
where
    I: Iterator<Item = DrawCommand>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut modifier = Modifier::empty();
    let mut next_cursor_pos: Option<Position> = None;
    for command in commands {
        let (x, y) = match command {
            DrawCommand::Put { x, y, .. } => (x, y),
            DrawCommand::ClearToEnd { x, y, .. } => (x, y),
        };
        let command_pos = Position { x, y };
        if next_cursor_pos != Some(command_pos) {
            queue!(writer, MoveTo(x, y))?;
        }
        match command {
            DrawCommand::Put { cell, .. } => {
                if cell.modifier != modifier {
                    let diff = ModifierDiff {
                        from: modifier,
                        to: cell.modifier,
                    };
                    diff.queue(writer)?;
                    modifier = cell.modifier;
                }
                if cell.fg != fg || cell.bg != bg {
                    queue!(
                        writer,
                        SetColors(Colors::new(cell.fg.into(), cell.bg.into()))
                    )?;
                    fg = cell.fg;
                    bg = cell.bg;
                }

                let symbol = cell.symbol();
                let symbol_has_risk = symbol_has_physical_repaint_risk(symbol);
                queue!(writer, Print(symbol))?;
                next_cursor_pos = if symbol_has_risk {
                    None
                } else {
                    Some(cursor_after_put(x, y, symbol))
                };
            }
            DrawCommand::ClearToEnd { bg: clear_bg, .. } => {
                queue!(writer, SetAttribute(crossterm::style::Attribute::Reset))?;
                fg = Color::Reset;
                modifier = Modifier::empty();
                queue!(writer, SetBackgroundColor(clear_bg.into()))?;
                bg = clear_bg;
                queue!(writer, Clear(crossterm::terminal::ClearType::UntilNewLine))?;
                next_cursor_pos = Some(command_pos);
            }
        }
    }

    queue!(
        writer,
        SetForegroundColor(crossterm::style::Color::Reset),
        SetBackgroundColor(crossterm::style::Color::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )?;

    Ok(())
}

/// The `ModifierDiff` struct is used to calculate the difference between two `Modifier`
/// values. This is useful when updating the terminal display, as it allows for more
/// efficient updates by only sending the necessary changes.
struct ModifierDiff {
    pub from: Modifier,
    pub to: Modifier,
}

impl ModifierDiff {
    fn queue<W: io::Write>(self, w: &mut W) -> io::Result<()> {
        use crossterm::style::Attribute as CAttribute;
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(CAttribute::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(CAttribute::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::RapidBlink))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::style::Style;

    fn compact_screen(contents: &str) -> String {
        contents.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[derive(Debug)]
    struct RecordingBackend {
        output: Vec<u8>,
        size: Size,
        cursor_position: Position,
    }

    impl RecordingBackend {
        fn new(width: u16, height: u16) -> Self {
            Self {
                output: Vec::new(),
                size: Size::new(width, height),
                cursor_position: Position::ORIGIN,
            }
        }

        fn output_string(&self) -> String {
            String::from_utf8(self.output.clone()).expect("terminal output should be utf8")
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
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            for (x, y, cell) in content {
                queue!(self, MoveTo(x, y), Print(cell.symbol()))?;
                self.cursor_position = cursor_after_put(x, y, cell.symbol());
            }
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
            let position = position.into();
            queue!(self, MoveTo(position.x, position.y))?;
            self.cursor_position = position;
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
            Ok(())
        }

        fn scroll_region_up(
            &mut self,
            _region: std::ops::Range<u16>,
            _line_count: u16,
        ) -> io::Result<()> {
            Ok(())
        }

        fn scroll_region_down(
            &mut self,
            _region: std::ops::Range<u16>,
            _line_count: u16,
        ) -> io::Result<()> {
            Ok(())
        }
    }

    fn queued_command(command: impl crossterm::Command) -> String {
        let mut bytes = Vec::new();
        queue!(&mut bytes, command).expect("queue test command");
        String::from_utf8(bytes).expect("queued command should be utf8")
    }

    fn index_of(haystack: &str, needle: &str, context: &str) -> usize {
        haystack
            .find(needle)
            .unwrap_or_else(|| panic!("missing {context}: {haystack:?}"))
    }

    fn cell_with_symbol(symbol: &str) -> Cell {
        let mut cell = Cell::default();
        cell.set_symbol(symbol);
        cell
    }

    fn corrupt_backend_row(
        terminal: &mut Terminal<crate::product::tui_app::test_backend::VT100Backend>,
        y: u16,
        text: &str,
    ) {
        draw(
            terminal.backend_mut(),
            vec![
                DrawCommand::Put {
                    x: 0,
                    y,
                    cell: cell_with_symbol(text),
                },
                DrawCommand::ClearToEnd {
                    x: u16::try_from(display_width(text)).expect("test text should fit"),
                    y,
                    bg: Color::Reset,
                },
            ]
            .into_iter(),
        )
        .expect("corrupt backend row");
        std::io::Write::flush(terminal.backend_mut()).expect("flush corrupted row");
    }

    #[test]
    fn display_width_ignores_osc_sequences() {
        assert_eq!(
            display_width("\x1b]8;;https://example.test\x07依\x1b]8;;\x07"),
            2
        );
    }

    #[test]
    fn draw_hides_cursor_before_repaint_and_shows_after_final_move() {
        let backend = RecordingBackend::new(80, 4);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 80, 4));
        assert!(!terminal.hidden_cursor);

        let final_cursor = Position { x: 3, y: 2 };
        terminal
            .draw(|frame| {
                frame
                    .buffer
                    .set_string(0, 0, "目标程序 + fuzz harness", Style::default());
                frame.set_cursor_position(final_cursor);
            })
            .expect("draw mixed-width frame");

        let output = terminal.backend().output_string();
        let hide = queued_command(Hide);
        let show = queued_command(Show);
        let repaint_move = queued_command(MoveTo(0, 0));
        let final_move = queued_command(MoveTo(final_cursor.x, final_cursor.y));

        let hide_index = index_of(&output, &hide, "hide cursor command");
        let repaint_move_index = index_of(&output, &repaint_move, "initial repaint move");
        let text_index = index_of(&output, "目", "mixed-width repaint text");
        let final_move_index = output
            .rfind(&final_move)
            .unwrap_or_else(|| panic!("missing final cursor move: {output:?}"));
        let show_index = index_of(&output, &show, "show cursor command");

        assert!(
            hide_index < repaint_move_index,
            "cursor should hide before first repaint move: {output:?}"
        );
        assert!(
            hide_index < text_index,
            "cursor should hide before mixed-width text is printed: {output:?}"
        );
        assert!(
            final_move_index < show_index,
            "final cursor move should happen before show cursor: {output:?}"
        );
        assert!(
            show_index > text_index,
            "cursor should show only after repaint output: {output:?}"
        );

        let after_show = &output[show_index + show.len()..];
        assert!(
            !after_show.contains(&repaint_move),
            "repaint move should not happen after show cursor: {output:?}"
        );
        assert!(!terminal.hidden_cursor);
        assert_eq!(terminal.last_known_cursor_pos, final_cursor);
    }

    #[test]
    fn draw_keeps_cursor_hidden_when_frame_has_no_cursor_position() {
        let backend = RecordingBackend::new(80, 4);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 80, 4));

        terminal
            .draw(|frame| {
                frame
                    .buffer
                    .set_string(0, 0, "目标程序 + fuzz harness", Style::default());
            })
            .expect("draw mixed-width frame without cursor");

        let output = terminal.backend().output_string();
        let hide = queued_command(Hide);
        let show = queued_command(Show);

        assert!(
            output.contains(&hide),
            "cursor should be hidden before repaint: {output:?}"
        );
        assert!(
            !output.contains(&show),
            "cursor should not be shown when frame has no cursor position: {output:?}"
        );
        assert!(terminal.hidden_cursor);
    }

    #[test]
    fn diff_buffers_clears_dirty_default_background_row_before_repaint() {
        let area = Rect::new(0, 0, 3, 2);
        let previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);

        next.cell_mut((2, 0))
            .expect("cell should exist")
            .set_symbol("X");

        let commands = diff_buffers(&previous, &next);

        assert!(
            matches!(
                commands.first(),
                Some(DrawCommand::ClearToEnd { x: 0, y: 0, .. })
            ),
            "expected diff_buffers to clear the default-background row before repainting; commands: {commands:?}",
        );
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Put { x: 2, y: 0, .. })),
            "expected diff_buffers to update the final cell; commands: {commands:?}",
        );
    }

    #[test]
    fn draw_reapplies_foreground_after_clear_to_end_between_same_colored_rows() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(4, 2);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 4, 2));

        terminal
            .draw(|frame| {
                let style = Style::default().fg(Color::Red);
                frame.buffer.set_string(0, 0, "AAAA", style);
                frame.buffer.set_string(0, 1, "BBBB", style);
            })
            .expect("draw colored rows");

        let screen = terminal.backend().vt100().screen();
        let expected_fg = vt100::Color::Idx(1);
        assert_eq!(
            screen.cell(0, 0).expect("first row cell").fgcolor(),
            expected_fg
        );
        assert_eq!(
            screen.cell(1, 0).expect("second row cell").fgcolor(),
            expected_fg
        );
    }

    #[test]
    fn custom_terminal_preserves_cjk_ascii_order_across_repainted_frames() {
        for width in [80, 100, 120] {
            let backend = crate::product::tui_app::test_backend::VT100Backend::new(width, 4);
            let mut terminal = Terminal::with_options(backend).expect("terminal");
            terminal.set_viewport_area(Rect::new(0, 0, width, 4));

            terminal
                .draw(|frame| {
                    frame.buffer.set_string(
                        0,
                        0,
                        "不是说本地开发或合 main 绝对不能有 Git 依；赖我说的是：",
                        Style::default(),
                    );
                    frame.buffer.set_string(
                        0,
                        1,
                        "- 如果这个分支合到 main 后要被认为 crates是.io 发布就绪",
                        Style::default(),
                    );
                })
                .expect("draw stale frame");

            terminal
                .draw(|frame| {
                    frame.buffer.set_string(
                        0,
                        0,
                        "不是说本地开发或合 main 绝对不能有 Git 依赖；我说的是：",
                        Style::default(),
                    );
                    frame.buffer.set_string(
                        0,
                        1,
                        "- 如果这个分支合到 main 后要被认为是 crates.io 发布就绪，那么现在还不满足发布条件。",
                        Style::default(),
                    );
                })
                .expect("draw corrected frame");

            let contents = terminal.backend().vt100().screen().contents();
            let compact = compact_screen(&contents);
            assert!(
                contents.contains("Git 依赖；我说的是"),
                "width {width} reordered Git dependency text: {contents:?}"
            );
            assert!(
                contents.contains("被认为是 crates.io 发布就绪"),
                "width {width} reordered crates.io readiness text: {contents:?}"
            );
            assert!(
                !compact.contains("依；赖"),
                "width {width} kept stale CJK punctuation corruption: {contents:?}"
            );
            assert!(
                !compact.contains("crates是.io"),
                "width {width} kept stale crates.io corruption: {contents:?}"
            );
            assert!(
                !compact.contains("crates.io是"),
                "width {width} moved the Chinese predicate after crates.io: {contents:?}"
            );
        }
    }

    #[test]
    fn custom_terminal_repairs_ascii_row_after_screen_buffer_divergence() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(120, 4);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 120, 4));

        terminal
            .draw(|frame| {
                frame.buffer.set_string(
                    0,
                    0,
                    "fix(cybergym): harden scoped threat-model graph seeding",
                    Style::default(),
                );
                frame
                    .buffer
                    .set_string(0, 1, "- git diff --check", Style::default());
            })
            .expect("draw correct baseline");

        corrupt_backend_row(
            &mut terminal,
            0,
            "fix(cybergym):en hard scoped threat-model graph seeding",
        );
        corrupt_backend_row(&mut terminal, 1, "- git diffcheck");
        let corrupted = terminal.backend().vt100().screen().contents();
        assert!(
            corrupted.contains("fix(cybergym):en hard"),
            "test setup should corrupt subject row: {corrupted:?}"
        );
        assert!(
            corrupted.contains("- git diffcheck"),
            "test setup should corrupt validation row: {corrupted:?}"
        );

        terminal
            .draw(|frame| {
                frame.buffer.set_string(
                    0,
                    0,
                    "fix(cybergym): harden scoped threat-model graph seeding.",
                    Style::default(),
                );
                frame
                    .buffer
                    .set_string(0, 1, "- git diff --check.", Style::default());
            })
            .expect("draw corrected dirty rows");

        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            contents.contains("fix(cybergym): harden scoped threat-model graph seeding."),
            "terminal should repaint stale ASCII subject cells: {contents:?}"
        );
        assert!(
            contents.contains("- git diff --check."),
            "terminal should repaint stale ASCII validation cells: {contents:?}"
        );
        assert!(
            !contents.contains("fix(cybergym):en hard"),
            "terminal kept stale subject corruption: {contents:?}"
        );
        assert!(
            !contents.contains("en hard scoped"),
            "terminal kept stale harden/scoped corruption: {contents:?}"
        );
        assert!(
            !contents.contains("git diffcheck"),
            "terminal kept stale git diff corruption: {contents:?}"
        );
    }

    #[test]
    fn invalidate_viewport_repairs_cjk_row_after_screen_buffer_divergence() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(120, 4);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 120, 4));

        let correct = "看到的“大工具输出”细节";
        let corrupted = "看到的工具输出”细“大节";

        terminal
            .draw(|frame| {
                frame.buffer.set_string(0, 0, correct, Style::default());
            })
            .expect("draw correct frame");

        corrupt_backend_row(&mut terminal, 0, corrupted);
        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            contents.contains(corrupted),
            "test setup should corrupt CJK row: {contents:?}"
        );

        terminal.invalidate_viewport();
        terminal
            .draw(|frame| {
                frame.buffer.set_string(0, 0, correct, Style::default());
            })
            .expect("draw unchanged frame");

        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            contents.contains(correct),
            "terminal should repaint stale CJK cells: {contents:?}"
        );
        assert!(
            !contents.contains("工具输出”细“大节"),
            "terminal kept stale CJK corruption: {contents:?}"
        );
        assert!(
            !contents.contains("细“大节"),
            "terminal kept stale CJK quote/order corruption: {contents:?}"
        );
    }

    #[test]
    fn unchanged_ascii_row_keeps_incremental_diff_behavior_after_screen_buffer_divergence() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(120, 4);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 120, 4));

        let correct = "fix(cybergym): harden scoped threat-model graph seeding";
        let corrupted = "fix(cybergym):en hard scoped threat-model graph seeding";

        terminal
            .draw(|frame| {
                frame.buffer.set_string(0, 0, correct, Style::default());
            })
            .expect("draw correct ASCII frame");

        corrupt_backend_row(&mut terminal, 0, corrupted);
        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            contents.contains(corrupted),
            "test setup should corrupt ASCII row: {contents:?}"
        );

        terminal
            .draw(|frame| {
                frame.buffer.set_string(0, 0, correct, Style::default());
            })
            .expect("draw unchanged ASCII frame");

        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            contents.contains(corrupted),
            "unchanged pure ASCII row should keep incremental diff behavior: {contents:?}"
        );
        assert!(
            !contents.contains(correct),
            "unchanged pure ASCII row should not be unconditionally repainted: {contents:?}"
        );
    }

    #[test]
    fn invalidate_viewport_repairs_mixed_cjk_ascii_row_after_screen_buffer_divergence() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(180, 4);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 180, 4));

        let correct = "所以 `cybergym-server-data/.../out/<fuzzer>` 本身就是“目标程序 + fuzz harness”的组合体。";
        let corrupted = "所以 `cybergym-server-data/.../out/<fuzzer>` 本身就是“目标程序 + fuzz”的 harness组合体。";

        terminal
            .draw(|frame| {
                frame.buffer.set_string(0, 0, correct, Style::default());
            })
            .expect("draw correct mixed-width frame");

        corrupt_backend_row(&mut terminal, 0, corrupted);
        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            contents.contains("fuzz”的 harness组合体"),
            "test setup should corrupt mixed CJK/ASCII row: {contents:?}"
        );

        terminal.invalidate_viewport();
        terminal
            .draw(|frame| {
                frame.buffer.set_string(0, 0, correct, Style::default());
            })
            .expect("draw unchanged mixed-width frame");

        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            contents.contains(correct),
            "terminal should repaint stale mixed CJK/ASCII row: {contents:?}"
        );
        assert!(
            !contents.contains("fuzz”的 harness组合体"),
            "terminal kept stale screenshot corruption: {contents:?}"
        );
    }

    #[test]
    fn invalidate_viewport_clears_physical_stale_rows_that_render_blank() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(80, 4);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 80, 4));

        terminal
            .draw(|frame| {
                frame
                    .buffer
                    .set_string(0, 0, "STALE live/status row", Style::default());
            })
            .expect("draw stale live/status row");
        assert!(
            terminal
                .backend()
                .vt100()
                .screen()
                .contents()
                .contains("STALE live/status row"),
            "test setup should draw stale row"
        );

        terminal.invalidate_viewport();
        terminal.draw(|_| {}).expect("draw blank invalidated frame");

        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            !contents.contains("STALE live/status row"),
            "invalidated blank frame should clear physical stale row: {contents:?}"
        );
    }

    #[test]
    fn draw_preserves_cjk_ascii_order_after_wide_cells() {
        let mut backend = crate::product::tui_app::test_backend::VT100Backend::new(24, 1);
        let commands = vec![
            DrawCommand::Put {
                x: 0,
                y: 0,
                cell: cell_with_symbol("依"),
            },
            DrawCommand::Put {
                x: 2,
                y: 0,
                cell: cell_with_symbol("赖"),
            },
            DrawCommand::Put {
                x: 4,
                y: 0,
                cell: cell_with_symbol("；"),
            },
            DrawCommand::Put {
                x: 6,
                y: 0,
                cell: cell_with_symbol("我"),
            },
            DrawCommand::Put {
                x: 8,
                y: 0,
                cell: cell_with_symbol("说"),
            },
            DrawCommand::Put {
                x: 10,
                y: 0,
                cell: cell_with_symbol("的"),
            },
            DrawCommand::Put {
                x: 12,
                y: 0,
                cell: cell_with_symbol("是"),
            },
            DrawCommand::Put {
                x: 14,
                y: 0,
                cell: cell_with_symbol("c"),
            },
        ];

        draw(&mut backend, commands.into_iter()).expect("draw cjk commands");

        let contents = backend.vt100().screen().contents();
        assert!(
            contents.contains("依赖；我说的是c"),
            "draw should preserve CJK/ASCII order: {contents:?}"
        );
        assert!(!compact_screen(&contents).contains("依；赖"));
    }

    #[test]
    fn diff_buffers_clear_to_end_starts_at_dirty_row_start_for_wide_char() {
        let area = Rect::new(0, 0, 10, 1);
        let mut previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);

        previous.set_string(0, 0, "中文", Style::default());
        next.set_string(0, 0, "中", Style::default());

        let commands = diff_buffers(&previous, &next);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, DrawCommand::ClearToEnd { x: 0, y: 0, .. })),
            "expected clear-to-end to start at the dirty row origin; commands: {commands:?}"
        );
    }

    #[test]
    fn diff_buffers_clears_cjk_row_with_ascii_marker_before_left_to_right_repaint() {
        let area = Rect::new(0, 0, 80, 1);
        let previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);

        next.set_string(0, 0, "这些都适合长远演进。- 最小使用体验", Style::default());

        let commands = diff_buffers(&previous, &next);

        assert!(
            matches!(
                commands.first(),
                Some(DrawCommand::ClearToEnd { x: 0, y: 0, .. })
            ),
            "expected the dirty CJK row to be cleared before repainting; commands: {commands:?}",
        );
        assert!(
            commands.iter().any(|command| {
                matches!(
                    command,
                    DrawCommand::Put {
                        x: 0,
                        y: 0,
                        cell
                    } if cell.symbol() == "这"
                )
            }),
            "expected repaint to start at the first CJK glyph; commands: {commands:?}",
        );
        assert!(
            commands.iter().any(|command| {
                matches!(
                    command,
                    DrawCommand::Put {
                        y: 0,
                        cell,
                        ..
                    } if cell.symbol() == "-"
                )
            }),
            "expected repaint to include the ASCII list marker in order; commands: {commands:?}",
        );
        assert!(
            commands
                .iter()
                .all(|command| !matches!(command, DrawCommand::Put { x: 1, y: 0, .. })),
            "expected repaint to skip the second cell of the first wide glyph; commands: {commands:?}",
        );
    }

    #[test]
    fn diff_buffers_skips_unchanged_cjk_rows_when_status_animates() {
        let area = Rect::new(0, 0, 80, 4);
        let mut previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);

        let static_cjk = "这是一个静态中文回答，状态动画不应该让这一行每帧重绘。";
        previous.set_string(0, 0, static_cjk, Style::default());
        next.set_string(0, 0, static_cjk, Style::default());

        previous.set_string(0, 3, "• Working /", Style::default());
        next.set_string(0, 3, "• Working -", Style::default());

        let commands = diff_buffers(&previous, &next);

        assert!(
            commands.iter().all(|command| !matches!(
                command,
                DrawCommand::ClearToEnd { y: 0, .. } | DrawCommand::Put { y: 0, .. }
            )),
            "unchanged CJK row should not repaint during unrelated animation; commands: {commands:?}",
        );
        assert!(
            commands.iter().any(|command| matches!(
                command,
                DrawCommand::ClearToEnd { y: 3, .. } | DrawCommand::Put { y: 3, .. }
            )),
            "changed status row should still repaint; commands: {commands:?}",
        );
    }

    #[test]
    fn diff_buffers_repaints_non_reset_background_blank_row_with_puts() {
        let area = Rect::new(0, 0, 6, 1);
        let previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);
        let row_bg = Color::Blue;

        for x in 0..area.width {
            next.cell_mut((x, 0))
                .expect("cell should exist")
                .set_style(Style::default().bg(row_bg));
        }

        let commands = diff_buffers(&previous, &next);

        assert!(
            commands
                .iter()
                .all(|command| !matches!(command, DrawCommand::ClearToEnd { y: 0, .. })),
            "expected explicit background row to avoid ClearToEnd; commands: {commands:?}"
        );
        assert!(
            commands.iter().any(|command| {
                matches!(
                    command,
                    DrawCommand::Put {
                        x: 5,
                        y: 0,
                        cell
                    } if cell.bg == row_bg
                )
            }),
            "expected the final background cell to be repainted; commands: {commands:?}"
        );
    }

    #[test]
    fn diff_buffers_repaints_non_reset_background_text_row_to_end() {
        let area = Rect::new(0, 0, 8, 1);
        let previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);
        let row_bg = Color::Blue;

        next.set_string(0, 0, "Plan", Style::default().bg(row_bg));
        for x in 0..area.width {
            next.cell_mut((x, 0))
                .expect("cell should exist")
                .set_style(Style::default().bg(row_bg));
        }

        let commands = diff_buffers(&previous, &next);

        assert!(
            commands
                .iter()
                .all(|command| !matches!(command, DrawCommand::ClearToEnd { y: 0, .. })),
            "expected explicit background row to avoid ClearToEnd; commands: {commands:?}"
        );
        assert!(
            commands.iter().any(|command| {
                matches!(
                    command,
                    DrawCommand::Put {
                        x: 7,
                        y: 0,
                        cell
                    } if cell.bg == row_bg
                )
            }),
            "expected trailing background cells to be repainted; commands: {commands:?}"
        );
    }

    #[test]
    fn resize_invalidates_previous_buffer() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(120, 8);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 120, 8));
        terminal
            .previous_buffer_mut()
            .set_string(100, 0, "hat", Style::default());

        terminal.resize(Size::new(99, 8)).expect("resize");

        let previous = terminal.previous_buffer();
        assert!(
            previous
                .content
                .iter()
                .all(|cell| cell.symbol().trim().is_empty()),
            "resize should force the next draw to repaint without stale buddy cells"
        );
    }

    #[test]
    fn clear_area_removes_old_viewport_live_rows() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(120, 8);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        let old_area = Rect::new(0, 0, 120, 8);
        terminal.set_viewport_area(old_area);

        terminal
            .draw(|frame| {
                frame.buffer.set_string(108, 2, ".-----.", Style::default());
                frame.buffer.set_string(
                    0,
                    4,
                    "• Working (35s • esc to interrupt)",
                    Style::default(),
                );
            })
            .expect("draw old live viewport");
        assert!(
            terminal
                .backend()
                .vt100()
                .screen()
                .contents()
                .contains(".-----.")
        );
        assert!(
            terminal
                .backend()
                .vt100()
                .screen()
                .contents()
                .contains("Working")
        );

        let new_area = Rect::new(0, 4, 99, 4);
        terminal.clear_area(old_area).expect("clear old viewport");
        terminal.set_viewport_area(new_area);
        terminal.clear_area(new_area).expect("clear new viewport");
        terminal.draw(|_| {}).expect("draw new viewport");

        let contents = terminal.backend().vt100().screen().contents();
        assert!(
            !contents.contains(".-----."),
            "old buddy row should be cleared after viewport moves: {contents:?}"
        );
        assert!(
            !contents.contains("Working"),
            "old status row should be cleared after viewport moves: {contents:?}"
        );
    }

    #[test]
    fn narrowing_viewport_clears_trailing_cells() {
        let backend = crate::product::tui_app::test_backend::VT100Backend::new(120, 8);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 120, 8));

        terminal
            .draw(|frame| {
                frame.buffer.set_string(112, 4, "buddy", Style::default());
            })
            .expect("draw wide viewport");
        assert!(
            terminal
                .backend()
                .vt100()
                .screen()
                .contents()
                .contains("buddy")
        );

        terminal.set_viewport_area(Rect::new(0, 0, 99, 8));
        terminal.draw(|_| {}).expect("draw narrowed viewport");

        assert!(
            !terminal
                .backend()
                .vt100()
                .screen()
                .contents()
                .contains("buddy"),
            "trailing buddy cells should not survive a narrower viewport"
        );
    }
}
