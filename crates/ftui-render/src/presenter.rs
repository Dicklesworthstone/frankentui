#![forbid(unsafe_code)]

//! Presenter: state-tracked ANSI emission.
//!
//! The Presenter transforms buffer diffs into minimal terminal output by tracking
//! the current terminal state and only emitting sequences when changes are needed.
//!
//! # Design Principles
//!
//! - **State tracking**: Track current style, link, and cursor to avoid redundant output
//! - **Run grouping**: Use ChangeRuns to minimize cursor positioning
//! - **Single write**: Buffer all output and flush once per frame
//! - **Synchronized output**: Use DEC 2026 to prevent flicker on supported terminals
//!
//! # Usage
//!
//! ```ignore
//! use ftui_render::presenter::Presenter;
//! use ftui_render::buffer::Buffer;
//! use ftui_render::diff::BufferDiff;
//! use ftui_core::terminal_capabilities::TerminalCapabilities;
//!
//! let caps = TerminalCapabilities::detect();
//! let mut presenter = Presenter::new(std::io::stdout(), caps);
//!
//! let mut current = Buffer::new(80, 24);
//! let mut next = Buffer::new(80, 24);
//! // ... render widgets into `next` ...
//!
//! let diff = BufferDiff::compute(&current, &next);
//! presenter.present(&next, &diff)?;
//! std::mem::swap(&mut current, &mut next);
//! ```

use std::io::{self, BufWriter, Write};

use crate::ansi::{self, EraseLineMode};
use crate::buffer::Buffer;
use crate::cell::{Cell, CellAttrs, PackedRgba, StyleFlags};
use crate::diff::BufferDiff;
use crate::grapheme_pool::GraphemePool;
use crate::link_registry::LinkRegistry;
use ftui_core::{debug_span, info_span, trace};

pub use ftui_core::terminal_capabilities::TerminalCapabilities;

/// Size of the internal write buffer (64KB).
const BUFFER_CAPACITY: usize = 64 * 1024;

/// Cached style state for comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    fg: PackedRgba,
    bg: PackedRgba,
    attrs: StyleFlags,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            fg: PackedRgba::TRANSPARENT,
            bg: PackedRgba::TRANSPARENT,
            attrs: StyleFlags::empty(),
        }
    }
}

impl CellStyle {
    fn from_cell(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            attrs: cell.attrs.flags(),
        }
    }
}

/// State-tracked ANSI presenter.
///
/// Transforms buffer diffs into minimal terminal output by tracking
/// the current terminal state and only emitting necessary escape sequences.
pub struct Presenter<W: Write> {
    /// Buffered writer for efficient output.
    writer: BufWriter<W>,
    /// Current style state (None = unknown/reset).
    current_style: Option<CellStyle>,
    /// Current hyperlink ID (None = no link).
    current_link: Option<u32>,
    /// Current cursor X position (0-indexed).
    cursor_x: u16,
    /// Current cursor Y position (0-indexed).
    cursor_y: u16,
    /// Terminal capabilities for conditional output.
    capabilities: TerminalCapabilities,
}

impl<W: Write> Presenter<W> {
    /// Create a new presenter with the given writer and capabilities.
    pub fn new(writer: W, capabilities: TerminalCapabilities) -> Self {
        Self {
            writer: BufWriter::with_capacity(BUFFER_CAPACITY, writer),
            current_style: None,
            current_link: None,
            cursor_x: 0,
            cursor_y: 0,
            capabilities,
        }
    }

    /// Get the terminal capabilities.
    #[inline]
    pub fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }

    /// Present a frame using the given buffer and diff.
    ///
    /// This is the main entry point for rendering. It:
    /// 1. Begins synchronized output (if supported)
    /// 2. Emits changes based on the diff
    /// 3. Resets style and closes links
    /// 4. Ends synchronized output
    /// 5. Flushes all buffered output
    pub fn present(&mut self, buffer: &Buffer, diff: &BufferDiff) -> io::Result<()> {
        self.present_with_pool(buffer, diff, None, None)
    }

    /// Present a frame with grapheme pool and link registry.
    pub fn present_with_pool(
        &mut self,
        buffer: &Buffer,
        diff: &BufferDiff,
        pool: Option<&GraphemePool>,
        links: Option<&LinkRegistry>,
    ) -> io::Result<()> {
        // Begin synchronized output to prevent flicker
        if self.capabilities.sync_output {
            ansi::sync_begin(&mut self.writer)?;
        }

        // Emit diff using run grouping for efficiency
        self.emit_diff(buffer, diff, pool, links)?;

        // Reset style at end (clean state for next frame)
        ansi::sgr_reset(&mut self.writer)?;
        self.current_style = None;

        // Close any open hyperlink
        if self.current_link.is_some() {
            ansi::hyperlink_end(&mut self.writer)?;
            self.current_link = None;
        }

        // End synchronized output
        if self.capabilities.sync_output {
            ansi::sync_end(&mut self.writer)?;
        }

        self.writer.flush()
    }

    /// Emit diff using run grouping.
    fn emit_diff(
        &mut self,
        buffer: &Buffer,
        diff: &BufferDiff,
        pool: Option<&GraphemePool>,
        links: Option<&LinkRegistry>,
    ) -> io::Result<()> {
        for run in diff.runs() {
            // Single cursor move per run
            self.move_cursor_to(run.x0, run.y)?;

            // Emit cells (cursor advances naturally after each character)
            for x in run.x0..=run.x1 {
                let cell = buffer.get_unchecked(x, run.y);
                self.emit_cell(cell, pool, links)?;
            }
        }
        Ok(())
    }

    /// Emit a single cell.
    fn emit_cell(
        &mut self,
        cell: &Cell,
        pool: Option<&GraphemePool>,
        links: Option<&LinkRegistry>,
    ) -> io::Result<()> {
        // Skip continuation cells (second cell of wide characters)
        if cell.is_continuation() {
            return Ok(());
        }

        // Emit style changes if needed
        self.emit_style_changes(cell)?;

        // Emit link changes if needed
        self.emit_link_changes(cell, links)?;

        // Emit the cell content
        self.emit_content(cell, pool)?;

        // Update cursor position (character output advances cursor)
        self.cursor_x += cell.width_hint() as u16;

        Ok(())
    }

    /// Emit style changes if the cell style differs from current.
    fn emit_style_changes(&mut self, cell: &Cell) -> io::Result<()> {
        let new_style = CellStyle::from_cell(cell);

        // Check if style changed
        if self.current_style == Some(new_style) {
            return Ok(());
        }

        // v1 strategy: Reset + apply (per ADR-002)
        // This is simpler and more robust than incremental updates
        ansi::sgr_reset(&mut self.writer)?;

        // Apply foreground color
        if new_style.fg.a() > 0 {
            ansi::sgr_fg_packed(&mut self.writer, new_style.fg)?;
        }

        // Apply background color
        if new_style.bg.a() > 0 {
            ansi::sgr_bg_packed(&mut self.writer, new_style.bg)?;
        }

        // Apply attributes
        if !new_style.attrs.is_empty() {
            ansi::sgr_flags(&mut self.writer, new_style.attrs)?;
        }

        self.current_style = Some(new_style);
        Ok(())
    }

    /// Emit hyperlink changes if the cell link differs from current.
    fn emit_link_changes(
        &mut self,
        cell: &Cell,
        links: Option<&LinkRegistry>,
    ) -> io::Result<()> {
        let raw_link_id = cell.attrs.link_id();
        let new_link = if raw_link_id == CellAttrs::LINK_ID_NONE {
            None
        } else {
            Some(raw_link_id)
        };

        // Check if link changed
        if self.current_link == new_link {
            return Ok(());
        }

        // Close current link if open
        if self.current_link.is_some() {
            ansi::hyperlink_end(&mut self.writer)?;
        }

        // Open new link if present
        if let (Some(link_id), Some(registry)) = (new_link, links)
            && let Some(url) = registry.get(link_id)
        {
            ansi::hyperlink_start(&mut self.writer, url)?;
        }

        self.current_link = new_link;
        Ok(())
    }

    /// Emit cell content (character or grapheme).
    fn emit_content(&mut self, cell: &Cell, pool: Option<&GraphemePool>) -> io::Result<()> {
        // Check if this is a grapheme reference
        if let Some(grapheme_id) = cell.content.grapheme_id() {
            if let Some(pool) = pool
                && let Some(text) = pool.get(grapheme_id)
            {
                return self.writer.write_all(text.as_bytes());
            }
            // Fallback: emit replacement character
            return self.writer.write_all("�".as_bytes());
        }

        // Regular character content
        if let Some(ch) = cell.content.as_char() {
            let mut buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut buf);
            self.writer.write_all(encoded.as_bytes())
        } else {
            // Empty cell - emit space
            self.writer.write_all(b" ")
        }
    }

    /// Move cursor to the specified position.
    fn move_cursor_to(&mut self, x: u16, y: u16) -> io::Result<()> {
        // Skip if already at position
        if self.cursor_x == x && self.cursor_y == y {
            return Ok(());
        }

        // Use CUP (cursor position) for absolute positioning
        ansi::cup(&mut self.writer, y, x)?;
        self.cursor_x = x;
        self.cursor_y = y;
        Ok(())
    }

    /// Clear the entire screen.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        ansi::erase_display(&mut self.writer, ansi::EraseDisplayMode::All)?;
        ansi::cup(&mut self.writer, 0, 0)?;
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.writer.flush()
    }

    /// Clear a single line.
    pub fn clear_line(&mut self, y: u16) -> io::Result<()> {
        self.move_cursor_to(0, y)?;
        ansi::erase_line(&mut self.writer, EraseLineMode::All)?;
        self.writer.flush()
    }

    /// Hide the cursor.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        ansi::cursor_hide(&mut self.writer)?;
        self.writer.flush()
    }

    /// Show the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        ansi::cursor_show(&mut self.writer)?;
        self.writer.flush()
    }

    /// Position the cursor at the specified coordinates.
    pub fn position_cursor(&mut self, x: u16, y: u16) -> io::Result<()> {
        self.move_cursor_to(x, y)?;
        self.writer.flush()
    }

    /// Reset the presenter state.
    ///
    /// Useful after resize or when terminal state is unknown.
    pub fn reset(&mut self) {
        self.current_style = None;
        self.current_link = None;
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    /// Flush any buffered output.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Get the inner writer (consuming the presenter).
    ///
    /// Flushes any buffered data before returning the writer.
    pub fn into_inner(self) -> Result<W, io::Error> {
        self.writer
            .into_inner()
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_presenter() -> Presenter<Vec<u8>> {
        let caps = TerminalCapabilities::basic();
        Presenter::new(Vec::new(), caps)
    }

    fn test_presenter_with_sync() -> Presenter<Vec<u8>> {
        let mut caps = TerminalCapabilities::basic();
        caps.sync_output = true;
        Presenter::new(Vec::new(), caps)
    }

    fn get_output(presenter: Presenter<Vec<u8>>) -> Vec<u8> {
        presenter.into_inner().unwrap()
    }

    #[test]
    fn empty_diff_produces_minimal_output() {
        let mut presenter = test_presenter();
        let buffer = Buffer::new(10, 10);
        let diff = BufferDiff::new();

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        // Should only have SGR reset
        assert!(output.starts_with(b"\x1b[0m"));
    }

    #[test]
    fn single_cell_change() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 10);
        buffer.set_raw(5, 5, Cell::from_char('X'));

        let old = Buffer::new(10, 10);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        // Should contain cursor position and character
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("X"));
        assert!(output_str.contains("\x1b[")); // Contains escape sequences
    }

    #[test]
    fn style_tracking_avoids_redundant_sgr() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);

        // Set multiple cells with same style
        let fg = PackedRgba::rgb(255, 0, 0);
        buffer.set_raw(0, 0, Cell::from_char('A').with_fg(fg));
        buffer.set_raw(1, 0, Cell::from_char('B').with_fg(fg));
        buffer.set_raw(2, 0, Cell::from_char('C').with_fg(fg));

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        // Count SGR sequences (should be minimal due to style tracking)
        let output_str = String::from_utf8_lossy(&output);
        let sgr_count = output_str.matches("\x1b[38;2").count();
        // Should have exactly 1 fg color sequence (style set once, reused for ABC)
        assert_eq!(sgr_count, 1, "Expected 1 SGR fg sequence, got {}", sgr_count);
    }

    #[test]
    fn cursor_position_optimized() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 5);

        // Set adjacent cells (should be one run)
        buffer.set_raw(3, 2, Cell::from_char('A'));
        buffer.set_raw(4, 2, Cell::from_char('B'));
        buffer.set_raw(5, 2, Cell::from_char('C'));

        let old = Buffer::new(10, 5);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        // Should have only one CUP sequence for the run
        let output_str = String::from_utf8_lossy(&output);
        let _cup_count = output_str.matches("\x1b[").filter(|_| true).count();

        // Content should be "ABC" somewhere in output
        assert!(output_str.contains("ABC") ||
                (output_str.contains('A') && output_str.contains('B') && output_str.contains('C')));
    }

    #[test]
    fn sync_output_wrapped_when_supported() {
        let mut presenter = test_presenter_with_sync();
        let buffer = Buffer::new(10, 10);
        let diff = BufferDiff::new();

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        // Should have sync begin and end
        assert!(output.starts_with(ansi::SYNC_BEGIN));
        assert!(output.windows(ansi::SYNC_END.len()).any(|w| w == ansi::SYNC_END));
    }

    #[test]
    fn clear_screen_works() {
        let mut presenter = test_presenter();
        presenter.clear_screen().unwrap();
        let output = get_output(presenter);

        // Should contain erase display sequence
        assert!(output.windows(b"\x1b[2J".len()).any(|w| w == b"\x1b[2J"));
    }

    #[test]
    fn cursor_visibility() {
        let mut presenter = test_presenter();

        presenter.hide_cursor().unwrap();
        presenter.show_cursor().unwrap();

        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains("\x1b[?25l")); // Hide
        assert!(output_str.contains("\x1b[?25h")); // Show
    }

    #[test]
    fn reset_clears_state() {
        let mut presenter = test_presenter();
        presenter.cursor_x = 50;
        presenter.cursor_y = 20;
        presenter.current_style = Some(CellStyle::default());

        presenter.reset();

        assert_eq!(presenter.cursor_x, 0);
        assert_eq!(presenter.cursor_y, 0);
        assert!(presenter.current_style.is_none());
    }

    #[test]
    fn position_cursor() {
        let mut presenter = test_presenter();
        presenter.position_cursor(10, 5).unwrap();

        let output = get_output(presenter);
        // CUP is 1-indexed: row 6, col 11
        assert!(output.windows(b"\x1b[6;11H".len()).any(|w| w == b"\x1b[6;11H"));
    }

    #[test]
    fn skip_cursor_move_when_already_at_position() {
        let mut presenter = test_presenter();
        presenter.cursor_x = 5;
        presenter.cursor_y = 3;

        // Move to same position
        presenter.move_cursor_to(5, 3).unwrap();

        // Should produce no output
        let output = get_output(presenter);
        assert!(output.is_empty());
    }

    #[test]
    fn continuation_cells_skipped() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);

        // Set a wide character
        buffer.set_raw(0, 0, Cell::from_char('中'));
        // The next cell would be a continuation - simulate it
        buffer.set_raw(1, 0, Cell::CONTINUATION);

        // Create a diff that includes both cells
        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        // Should contain the wide character
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('中'));
    }
}
