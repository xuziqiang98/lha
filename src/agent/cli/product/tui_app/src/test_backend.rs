use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::{self};
use std::io::Write;
use std::io::{self};

use ratatui::prelude::CrosstermBackend;

use ratatui::backend::Backend;
use ratatui::backend::ClearType;
use ratatui::backend::WindowSize;
use ratatui::buffer::Buffer;
use ratatui::buffer::Cell;
use ratatui::layout::Position;
use ratatui::layout::Size;
use unicode_width::UnicodeWidthChar;

/// This wraps a CrosstermBackend and a vt100::Parser to mock
/// a "real" terminal.
///
/// Importantly, this wrapper avoids calling any crossterm methods
/// which write to stdout regardless of the writer. This includes:
/// - getting the terminal size
/// - getting the cursor position
pub struct VT100Backend {
    crossterm_backend: CrosstermBackend<vt100::Parser>,
}

impl VT100Backend {
    /// Creates a new `TestBackend` with the specified width and height.
    pub fn new(width: u16, height: u16) -> Self {
        crossterm::style::force_color_output(true);
        Self {
            crossterm_backend: CrosstermBackend::new(vt100::Parser::new(height, width, 0)),
        }
    }

    pub fn vt100(&self) -> &vt100::Parser {
        self.crossterm_backend.writer()
    }
}

impl Write for VT100Backend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crossterm_backend.writer_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.crossterm_backend.writer_mut().flush()
    }
}

impl fmt::Display for VT100Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.crossterm_backend.writer().screen().contents())
    }
}

impl Backend for VT100Backend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.crossterm_backend.draw(content)?;
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.hide_cursor()?;
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.show_cursor()?;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.vt100().screen().cursor_position().into())
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.crossterm_backend.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.crossterm_backend.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.crossterm_backend.clear_region(clear_type)
    }

    fn append_lines(&mut self, line_count: u16) -> io::Result<()> {
        self.crossterm_backend.append_lines(line_count)
    }

    fn size(&self) -> io::Result<Size> {
        let (rows, cols) = self.vt100().screen().size();
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.vt100().screen().size().into(),
            // Arbitrary size, we don't rely on this in testing.
            pixels: Size {
                width: 640,
                height: 480,
            },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.crossterm_backend.writer_mut().flush()
    }

    fn scroll_region_up(&mut self, region: std::ops::Range<u16>, scroll_by: u16) -> io::Result<()> {
        self.crossterm_backend.scroll_region_up(region, scroll_by)
    }

    fn scroll_region_down(
        &mut self,
        region: std::ops::Range<u16>,
        scroll_by: u16,
    ) -> io::Result<()> {
        self.crossterm_backend.scroll_region_down(region, scroll_by)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AnsiFrameStats {
    pub(crate) raw_byte_count: usize,
    pub(crate) printed_columns: usize,
    pub(crate) cursor_move_count: usize,
    pub(crate) erase_line_rows: BTreeSet<u16>,
    pub(crate) erase_line_count_by_row: BTreeMap<u16, usize>,
    pub(crate) erase_display_count: usize,
    pub(crate) scroll_operation_count: usize,
    pub(crate) mutated_rows: BTreeSet<u16>,
    pub(crate) printed_columns_by_row: BTreeMap<u16, usize>,
    pub(crate) invalid_utf8: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AuditedAnsiFrame {
    pub(crate) stats: AnsiFrameStats,
    pub(crate) raw_bytes: Vec<u8>,
    pub(crate) draw_coordinates: Vec<(u16, u16)>,
    pub(crate) screen_before: Vec<String>,
    pub(crate) screen_after: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BufferFrameStats {
    pub(crate) changed_rows: BTreeSet<u16>,
    pub(crate) stable_rows_touched: BTreeSet<u16>,
    pub(crate) stable_cjk_rows_touched: BTreeSet<u16>,
    pub(crate) row_write_amplification: f64,
}

impl BufferFrameStats {
    pub(crate) fn diagnostic(&self, stage: &str, frame: &AuditedAnsiFrame) -> String {
        format!(
            "{}; changed_rows={:?}; stable_rows_touched={:?}; stable_cjk_rows_touched={:?}; \
             row_write_amplification={};",
            frame.diagnostic(stage),
            self.changed_rows,
            self.stable_rows_touched,
            self.stable_cjk_rows_touched,
            self.row_write_amplification,
        )
    }
}

pub(crate) fn analyze_buffer_frame(
    before: &Buffer,
    after: &Buffer,
    frame: &AuditedAnsiFrame,
) -> BufferFrameStats {
    assert_eq!(before.area, after.area, "buffer areas must match");
    let changed_rows = (before.area.y..before.area.bottom())
        .filter(|y| (before.area.x..before.area.right()).any(|x| before[(x, *y)] != after[(x, *y)]))
        .collect::<BTreeSet<_>>();
    let stable_rows_touched = frame
        .stats
        .mutated_rows
        .difference(&changed_rows)
        .copied()
        .collect::<BTreeSet<_>>();
    let stable_cjk_rows_touched = stable_rows_touched
        .iter()
        .copied()
        .filter(|y| {
            (before.area.x..before.area.right()).any(|x| {
                let symbol = before[(x, *y)].symbol();
                !symbol.is_ascii() || symbol.chars().any(|character| character.width() != Some(1))
            })
        })
        .collect::<BTreeSet<_>>();
    let row_write_amplification = if changed_rows.is_empty() {
        if frame.stats.mutated_rows.is_empty() {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        frame.stats.mutated_rows.len() as f64 / changed_rows.len() as f64
    };

    BufferFrameStats {
        changed_rows,
        stable_rows_touched,
        stable_cjk_rows_touched,
        row_write_amplification,
    }
}

pub(crate) fn assert_vt100_grid_matches_buffer(
    stage: &str,
    buffer: &Buffer,
    parser: &vt100::Parser,
    diagnostic: &str,
) {
    for y in buffer.area.y..buffer.area.bottom() {
        for x in buffer.area.x..buffer.area.right() {
            let expected = &buffer[(x, y)];
            let actual = parser.screen().cell(y, x).unwrap_or_else(|| {
                panic!("{stage}: missing VT100 cell at ({x}, {y}); {diagnostic}")
            });
            if expected.skip {
                assert!(
                    actual.contents().is_empty(),
                    "{stage}: wide continuation at ({x}, {y}) contains {:?}; {diagnostic}",
                    actual.contents(),
                );
            } else {
                let actual_contents = actual.contents();
                let actual_symbol = if actual_contents.is_empty() {
                    " "
                } else {
                    actual_contents.as_str()
                };
                assert_eq!(
                    actual_symbol,
                    expected.symbol(),
                    "{stage}: glyph mismatch at ({x}, {y}); {diagnostic}"
                );
            }
        }
    }
}

impl AuditedAnsiFrame {
    pub(crate) fn escaped_ansi(&self) -> String {
        self.raw_bytes
            .iter()
            .flat_map(|byte| std::ascii::escape_default(*byte))
            .map(char::from)
            .collect()
    }

    pub(crate) fn diagnostic(&self, stage: &str) -> String {
        format!(
            "stage={stage}; mutated_rows={:?}; printed_columns_by_row={:?}; \
             erase_line_rows={:?}; erase_line_count_by_row={:?}; erase_display_count={}; \
             scroll_operation_count={}; escaped_ansi={}; screen_before={:?}; screen_after={:?}",
            self.stats.mutated_rows,
            self.stats.printed_columns_by_row,
            self.stats.erase_line_rows,
            self.stats.erase_line_count_by_row,
            self.stats.erase_display_count,
            self.stats.scroll_operation_count,
            self.escaped_ansi(),
            self.screen_before,
            self.screen_after,
        )
    }
}

/// Crossterm backend with both VT100 emulation and frame-level ANSI auditing.
pub(crate) struct AuditedVT100Backend {
    crossterm_backend: CrosstermBackend<AuditedWriter>,
    fail_next_draw: bool,
}

impl AuditedVT100Backend {
    pub(crate) fn new(width: u16, height: u16) -> Self {
        crossterm::style::force_color_output(true);
        Self {
            crossterm_backend: CrosstermBackend::new(AuditedWriter::new(width, height)),
            fail_next_draw: false,
        }
    }

    pub(crate) fn vt100(&self) -> &vt100::Parser {
        &self.crossterm_backend.writer().vt100
    }

    pub(crate) fn frames(&self) -> &[AuditedAnsiFrame] {
        &self.crossterm_backend.writer().frames
    }

    pub(crate) fn last_frame(&self) -> Option<&AuditedAnsiFrame> {
        self.frames().last()
    }

    pub(crate) fn clear_frames(&mut self) {
        self.crossterm_backend.writer_mut().frames.clear();
    }

    pub(crate) fn set_size(&mut self, width: u16, height: u16) {
        let writer = self.crossterm_backend.writer_mut();
        writer.vt100.set_size(height, width);
        writer.analyzer.set_size(width, height);
    }

    pub(crate) fn fail_next_draw(&mut self) {
        self.fail_next_draw = true;
    }
}

impl Write for AuditedVT100Backend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crossterm_backend.writer_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.crossterm_backend.writer_mut().flush()
    }
}

impl fmt::Display for AuditedVT100Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.vt100().screen().contents())
    }
}

impl Backend for AuditedVT100Backend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        if std::mem::take(&mut self.fail_next_draw) {
            return Err(io::Error::other("injected draw failure"));
        }
        let content = content
            .map(|(x, y, cell)| (x, y, cell.clone()))
            .collect::<Vec<_>>();
        self.crossterm_backend
            .writer_mut()
            .draw_coordinates
            .extend(content.iter().map(|(x, y, _)| (*x, *y)));
        self.crossterm_backend
            .draw(content.iter().map(|(x, y, cell)| (*x, *y, cell)))
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.vt100().screen().cursor_position().into())
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.crossterm_backend.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.crossterm_backend.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.crossterm_backend.clear_region(clear_type)
    }

    fn append_lines(&mut self, line_count: u16) -> io::Result<()> {
        self.crossterm_backend.append_lines(line_count)
    }

    fn size(&self) -> io::Result<Size> {
        let (rows, cols) = self.vt100().screen().size();
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.vt100().screen().size().into(),
            pixels: Size {
                width: 640,
                height: 480,
            },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.crossterm_backend.writer_mut().flush()
    }

    fn scroll_region_up(&mut self, region: std::ops::Range<u16>, scroll_by: u16) -> io::Result<()> {
        self.crossterm_backend.scroll_region_up(region, scroll_by)
    }

    fn scroll_region_down(
        &mut self,
        region: std::ops::Range<u16>,
        scroll_by: u16,
    ) -> io::Result<()> {
        self.crossterm_backend.scroll_region_down(region, scroll_by)
    }
}

struct AuditedWriter {
    vt100: vt100::Parser,
    analyzer: AnsiAnalyzer,
    raw_bytes: Vec<u8>,
    draw_coordinates: Vec<(u16, u16)>,
    frame_screen_before: Option<Vec<String>>,
    frames: Vec<AuditedAnsiFrame>,
}

impl AuditedWriter {
    fn new(width: u16, height: u16) -> Self {
        Self {
            vt100: vt100::Parser::new(height, width, 0),
            analyzer: AnsiAnalyzer::new(width, height),
            raw_bytes: Vec::new(),
            draw_coordinates: Vec::new(),
            frame_screen_before: None,
            frames: Vec::new(),
        }
    }
}

fn visible_screen_rows(parser: &vt100::Parser) -> Vec<String> {
    let (height, width) = parser.screen().size();
    parser
        .screen()
        .rows(0, width)
        .take(usize::from(height))
        .collect()
}

impl Write for AuditedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.raw_bytes.is_empty() {
            self.frame_screen_before = Some(visible_screen_rows(&self.vt100));
        }
        self.raw_bytes.extend_from_slice(buf);
        self.analyzer.advance(buf);
        self.vt100.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.vt100.flush()?;
        let raw_bytes = std::mem::take(&mut self.raw_bytes);
        let draw_coordinates = std::mem::take(&mut self.draw_coordinates);
        let screen_before = self
            .frame_screen_before
            .take()
            .unwrap_or_else(|| visible_screen_rows(&self.vt100));
        let screen_after = visible_screen_rows(&self.vt100);
        let stats = self.analyzer.finish_frame(raw_bytes.len());
        self.frames.push(AuditedAnsiFrame {
            stats,
            raw_bytes,
            draw_coordinates,
            screen_before,
            screen_after,
        });
        Ok(())
    }
}

#[derive(Debug)]
struct AnsiAnalyzer {
    width: u16,
    height: u16,
    row: u16,
    column: u16,
    scroll_top: u16,
    scroll_bottom: u16,
    saved_cursor: Option<(u16, u16)>,
    state: AnsiState,
    utf8: Vec<u8>,
    stats: AnsiFrameStats,
}

#[derive(Debug, Default)]
enum AnsiState {
    #[default]
    Ground,
    Escape,
    Csi(Vec<u8>),
    Osc {
        escape_seen: bool,
    },
}

impl AnsiAnalyzer {
    fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            row: 0,
            column: 0,
            scroll_top: 0,
            scroll_bottom: height.saturating_sub(1),
            saved_cursor: None,
            state: AnsiState::Ground,
            utf8: Vec::new(),
            stats: AnsiFrameStats::default(),
        }
    }

    fn set_size(&mut self, width: u16, height: u16) {
        let bottom_anchored = self.scroll_bottom == self.height.saturating_sub(1);
        self.width = width;
        self.height = height;
        let screen_bottom = height.saturating_sub(1);
        self.row = self.row.min(screen_bottom);
        self.column = self.column.min(width.saturating_sub(1));
        self.scroll_top = self.scroll_top.min(screen_bottom);
        self.scroll_bottom = if bottom_anchored {
            screen_bottom
        } else {
            self.scroll_bottom.min(screen_bottom).max(self.scroll_top)
        };
    }

    fn advance(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.advance_byte(*byte);
        }
    }

    fn advance_byte(&mut self, byte: u8) {
        let state = std::mem::take(&mut self.state);
        self.state = match state {
            AnsiState::Ground => self.advance_ground(byte),
            AnsiState::Escape => match byte {
                b'[' => AnsiState::Csi(Vec::new()),
                b']' => AnsiState::Osc { escape_seen: false },
                b'7' => {
                    self.saved_cursor = Some((self.row, self.column));
                    AnsiState::Ground
                }
                b'8' => {
                    if let Some((row, column)) = self.saved_cursor {
                        self.row = row;
                        self.column = column;
                    }
                    AnsiState::Ground
                }
                b'D' => {
                    self.row = self
                        .row
                        .saturating_add(1)
                        .min(self.height.saturating_sub(1));
                    self.stats.cursor_move_count += 1;
                    AnsiState::Ground
                }
                b'E' => {
                    self.row = self
                        .row
                        .saturating_add(1)
                        .min(self.height.saturating_sub(1));
                    self.column = 0;
                    self.stats.cursor_move_count += 1;
                    AnsiState::Ground
                }
                b'M' => {
                    if self.row == self.scroll_top {
                        self.record_scroll_operation();
                    } else {
                        self.row = self.row.saturating_sub(1);
                        self.stats.cursor_move_count += 1;
                    }
                    AnsiState::Ground
                }
                _ => AnsiState::Ground,
            },
            AnsiState::Csi(mut sequence) => {
                if (0x40..=0x7e).contains(&byte) {
                    self.apply_csi(&sequence, byte);
                    AnsiState::Ground
                } else {
                    sequence.push(byte);
                    AnsiState::Csi(sequence)
                }
            }
            AnsiState::Osc { escape_seen } => {
                if byte == 0x07 || (escape_seen && byte == b'\\') {
                    AnsiState::Ground
                } else {
                    AnsiState::Osc {
                        escape_seen: byte == 0x1b,
                    }
                }
            }
        };
    }

    fn advance_ground(&mut self, byte: u8) -> AnsiState {
        if !self.utf8.is_empty() && byte >= 0x80 {
            self.utf8.push(byte);
            self.try_finish_utf8();
            return AnsiState::Ground;
        }

        match byte {
            0x1b => {
                self.finish_invalid_utf8();
                AnsiState::Escape
            }
            0x9b => {
                self.finish_invalid_utf8();
                AnsiState::Csi(Vec::new())
            }
            b'\n' => {
                self.finish_invalid_utf8();
                self.advance_row();
                AnsiState::Ground
            }
            b'\r' => {
                self.finish_invalid_utf8();
                self.column = 0;
                AnsiState::Ground
            }
            0x08 => {
                self.finish_invalid_utf8();
                self.column = self.column.saturating_sub(1);
                AnsiState::Ground
            }
            b'\t' => {
                self.finish_invalid_utf8();
                self.column = ((self.column / 8) + 1).saturating_mul(8).min(self.width);
                AnsiState::Ground
            }
            0x00..=0x1f | 0x7f => {
                self.finish_invalid_utf8();
                AnsiState::Ground
            }
            0x20..=0x7e => {
                self.finish_invalid_utf8();
                self.print_char(char::from(byte));
                AnsiState::Ground
            }
            _ => {
                self.utf8.push(byte);
                self.try_finish_utf8();
                AnsiState::Ground
            }
        }
    }

    fn try_finish_utf8(&mut self) {
        let Some(expected_len) = self
            .utf8
            .first()
            .and_then(|first| utf8_sequence_len(*first))
        else {
            self.stats.invalid_utf8 = true;
            self.utf8.clear();
            return;
        };
        if self.utf8.len() < expected_len {
            return;
        }
        match std::str::from_utf8(&self.utf8) {
            Ok(text) => {
                let chars = text.chars().collect::<Vec<_>>();
                self.utf8.clear();
                for character in chars {
                    self.print_char(character);
                }
            }
            Err(_) => {
                self.stats.invalid_utf8 = true;
                self.utf8.clear();
            }
        }
    }

    fn finish_invalid_utf8(&mut self) {
        if !self.utf8.is_empty() {
            self.stats.invalid_utf8 = true;
            self.utf8.clear();
        }
    }

    fn print_char(&mut self, character: char) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let width = character.width().unwrap_or(0);
        if width > 0
            && (self.column >= self.width
                || self
                    .column
                    .saturating_add(u16::try_from(width).unwrap_or(u16::MAX))
                    > self.width)
        {
            self.advance_row();
            self.column = 0;
        }
        self.stats.mutated_rows.insert(self.row);
        self.stats.printed_columns += width;
        *self
            .stats
            .printed_columns_by_row
            .entry(self.row)
            .or_default() += width;
        self.column = self
            .column
            .saturating_add(u16::try_from(width).unwrap_or(u16::MAX))
            .min(self.width);
    }

    fn advance_row(&mut self) {
        if self.height == 0 {
            return;
        }
        if self.row == self.scroll_bottom {
            self.record_scroll_operation();
        } else {
            self.row = self
                .row
                .saturating_add(1)
                .min(self.height.saturating_sub(1));
        }
    }

    fn apply_csi(&mut self, sequence: &[u8], final_byte: u8) {
        let parameters = parse_csi_parameters(sequence);
        let parameter = |index: usize, default: u16| {
            parameters
                .get(index)
                .copied()
                .flatten()
                .filter(|value| *value != 0)
                .unwrap_or(default)
        };

        match final_byte {
            b'H' | b'f' => {
                self.row = parameter(0, 1)
                    .saturating_sub(1)
                    .min(self.height.saturating_sub(1));
                self.column = parameter(1, 1)
                    .saturating_sub(1)
                    .min(self.width.saturating_sub(1));
                self.stats.cursor_move_count += 1;
            }
            b'A' => {
                self.row = self.row.saturating_sub(parameter(0, 1));
                self.stats.cursor_move_count += 1;
            }
            b'B' => {
                self.row = self
                    .row
                    .saturating_add(parameter(0, 1))
                    .min(self.height.saturating_sub(1));
                self.stats.cursor_move_count += 1;
            }
            b'C' => {
                self.column = self
                    .column
                    .saturating_add(parameter(0, 1))
                    .min(self.width.saturating_sub(1));
                self.stats.cursor_move_count += 1;
            }
            b'D' => {
                self.column = self.column.saturating_sub(parameter(0, 1));
                self.stats.cursor_move_count += 1;
            }
            b'E' => {
                self.row = self
                    .row
                    .saturating_add(parameter(0, 1))
                    .min(self.height.saturating_sub(1));
                self.column = 0;
                self.stats.cursor_move_count += 1;
            }
            b'F' => {
                self.row = self.row.saturating_sub(parameter(0, 1));
                self.column = 0;
                self.stats.cursor_move_count += 1;
            }
            b'G' | b'`' => {
                self.column = parameter(0, 1)
                    .saturating_sub(1)
                    .min(self.width.saturating_sub(1));
                self.stats.cursor_move_count += 1;
            }
            b'd' => {
                self.row = parameter(0, 1)
                    .saturating_sub(1)
                    .min(self.height.saturating_sub(1));
                self.stats.cursor_move_count += 1;
            }
            b'J' => {
                self.stats.erase_display_count += 1;
                self.stats.mutated_rows.extend(0..self.height);
            }
            b'K' => {
                self.stats.erase_line_rows.insert(self.row);
                *self
                    .stats
                    .erase_line_count_by_row
                    .entry(self.row)
                    .or_default() += 1;
                self.stats.mutated_rows.insert(self.row);
            }
            b'S' | b'T' | b'L' | b'M' => {
                self.record_scroll_operation();
            }
            b'r' => {
                self.scroll_top = parameter(0, 1)
                    .saturating_sub(1)
                    .min(self.height.saturating_sub(1));
                self.scroll_bottom = parameter(1, self.height)
                    .saturating_sub(1)
                    .min(self.height.saturating_sub(1))
                    .max(self.scroll_top);
                self.row = 0;
                self.column = 0;
                self.stats.cursor_move_count += 1;
            }
            b'@' | b'P' | b'X' => {
                self.stats.mutated_rows.insert(self.row);
            }
            b's' => self.saved_cursor = Some((self.row, self.column)),
            b'u' => {
                if let Some((row, column)) = self.saved_cursor {
                    self.row = row;
                    self.column = column;
                }
            }
            _ => {}
        }
    }

    fn record_scroll_operation(&mut self) {
        if self.height == 0 {
            return;
        }
        self.stats.scroll_operation_count += 1;
        self.stats
            .mutated_rows
            .extend(self.scroll_top..=self.scroll_bottom);
    }

    fn finish_frame(&mut self, raw_byte_count: usize) -> AnsiFrameStats {
        let mut stats = std::mem::take(&mut self.stats);
        stats.raw_byte_count = raw_byte_count;
        stats
    }
}

fn utf8_sequence_len(first: u8) -> Option<usize> {
    match first {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

fn parse_csi_parameters(sequence: &[u8]) -> Vec<Option<u16>> {
    let sequence = sequence
        .iter()
        .copied()
        .skip_while(|byte| matches!(byte, b'?' | b'>' | b'!' | b'='))
        .collect::<Vec<_>>();
    if sequence.is_empty() {
        return Vec::new();
    }
    sequence
        .split(|byte| *byte == b';' || *byte == b':')
        .map(|value| {
            if value.is_empty() {
                None
            } else {
                std::str::from_utf8(value).ok()?.parse().ok()
            }
        })
        .collect()
}

#[cfg(test)]
mod audited_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn ansi_analyzer_keeps_csi_osc_and_utf8_state_across_writes() {
        let mut writer = AuditedWriter::new(20, 4);
        writer.write_all(b"\x1b[").unwrap();
        writer.write_all(b"2;3H").unwrap();
        writer.write_all(&"丛".as_bytes()[..1]).unwrap();
        writer.write_all(&"丛".as_bytes()[1..]).unwrap();
        writer.write_all(b"\x1b]9;ignored").unwrap();
        writer.write_all(b"\x1b").unwrap();
        writer.write_all(b"\\X").unwrap();
        writer.flush().unwrap();

        let frame = writer.frames.last().unwrap();
        assert_eq!(frame.stats.cursor_move_count, 1);
        assert_eq!(frame.stats.printed_columns, 3);
        assert_eq!(frame.stats.mutated_rows, BTreeSet::from([1]));
        assert_eq!(frame.stats.printed_columns_by_row, BTreeMap::from([(1, 3)]));
        assert!(!frame.stats.invalid_utf8);
        assert_eq!(writer.vt100.screen().contents(), "\n  丛X");
    }

    #[test]
    fn ansi_analyzer_tracks_reverse_index_scroll_and_repeated_line_erases() {
        let mut writer = AuditedWriter::new(20, 4);
        writer
            .write_all(b"\x1b[2;4r\x1b[2;1H\x1bM\x1b[K\x1b[K")
            .unwrap();
        writer.flush().unwrap();

        let frame = writer.frames.last().unwrap();
        assert_eq!(frame.stats.scroll_operation_count, 1);
        assert_eq!(frame.stats.mutated_rows, BTreeSet::from([1, 2, 3]));
        assert_eq!(
            frame.stats.erase_line_count_by_row,
            BTreeMap::from([(1, 2)])
        );
    }

    #[test]
    fn ansi_analyzer_tracks_right_margin_wraps_per_row() {
        let mut writer = AuditedWriter::new(4, 2);
        writer.write_all(b"abcdE").unwrap();
        writer.flush().unwrap();

        let frame = writer.frames.last().unwrap();
        assert_eq!(frame.stats.mutated_rows, BTreeSet::from([0, 1]));
        assert_eq!(
            frame.stats.printed_columns_by_row,
            BTreeMap::from([(0, 4), (1, 1)])
        );
        assert_eq!(frame.screen_before, vec![String::new(), String::new()]);
        assert_eq!(
            frame.screen_after,
            vec!["abcd".to_string(), "E".to_string()]
        );
    }

    #[test]
    fn ansi_analyzer_expands_bottom_anchored_scroll_region_on_height_growth() {
        let mut backend = AuditedVT100Backend::new(4, 2);
        backend.write_all(b"\x1b[2;1H").unwrap();
        Write::flush(&mut backend).unwrap();
        backend.clear_frames();

        backend.set_size(4, 4);
        backend.write_all(b"\nX").unwrap();
        Write::flush(&mut backend).unwrap();

        let frame = backend.last_frame().unwrap();
        assert_eq!(frame.stats.scroll_operation_count, 0);
        assert_eq!(frame.stats.mutated_rows, BTreeSet::from([2]));
        assert_eq!(frame.stats.printed_columns_by_row, BTreeMap::from([(2, 1)]));
    }

    #[test]
    fn ansi_analyzer_preserves_non_anchored_scroll_region_on_height_growth() {
        let mut backend = AuditedVT100Backend::new(4, 4);
        backend.write_all(b"\x1b[2;3r\x1b[3;1H").unwrap();
        Write::flush(&mut backend).unwrap();
        backend.clear_frames();

        backend.set_size(4, 6);
        backend.write_all(b"\nX").unwrap();
        Write::flush(&mut backend).unwrap();

        let frame = backend.last_frame().unwrap();
        assert_eq!(frame.stats.scroll_operation_count, 1);
        assert_eq!(frame.stats.mutated_rows, BTreeSet::from([1, 2]));
        assert_eq!(frame.stats.printed_columns_by_row, BTreeMap::from([(2, 1)]));
    }
}
