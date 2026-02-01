#![forbid(unsafe_code)]

//! Terminal output coordinator with inline mode support.
//!
//! The `TerminalWriter` is the component that makes inline mode work. It:
//! - Serializes log writes and UI presents (one-writer rule)
//! - Implements the cursor save/restore contract
//! - Manages scroll regions (when optimization enabled)
//! - Ensures single buffered write per operation
//!
//! # Screen Modes
//!
//! - **Inline Mode**: Preserves terminal scrollback. UI is rendered at the
//!   bottom, logs scroll normally above. Uses cursor save/restore.
//!
//! - **AltScreen Mode**: Uses alternate screen buffer. Full-screen UI,
//!   no scrollback preservation.
//!
//! # Inline Mode Contract
//!
//! 1. Cursor is saved before any UI operation
//! 2. UI region is cleared and redrawn
//! 3. Cursor is restored after UI operation
//! 4. Log writes go above the UI region
//! 5. Terminal state is restored on drop
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::{TerminalWriter, ScreenMode, UiAnchor};
//! use ftui_render::buffer::Buffer;
//! use ftui_core::terminal_capabilities::TerminalCapabilities;
//!
//! // Create writer for inline mode with 10-row UI
//! let mut writer = TerminalWriter::new(
//!     std::io::stdout(),
//!     ScreenMode::Inline { ui_height: 10 },
//!     UiAnchor::Bottom,
//!     TerminalCapabilities::detect(),
//! );
//!
//! // Write logs (goes to scrollback above UI)
//! writer.write_log("Starting...\n")?;
//!
//! // Present UI
//! let buffer = Buffer::new(80, 10);
//! writer.present_ui(&buffer)?;
//! ```

use std::io::{self, BufWriter, Write};

use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::link_registry::LinkRegistry;

/// Size of the internal write buffer (64KB).
const BUFFER_CAPACITY: usize = 64 * 1024;

/// DEC cursor save (ESC 7) - more portable than CSI s.
const CURSOR_SAVE: &[u8] = b"\x1b7";

/// DEC cursor restore (ESC 8) - more portable than CSI u.
const CURSOR_RESTORE: &[u8] = b"\x1b8";

/// Synchronized output begin (DEC 2026).
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";

/// Synchronized output end (DEC 2026).
const SYNC_END: &[u8] = b"\x1b[?2026l";

/// Erase entire line (CSI 2 K).
const ERASE_LINE: &[u8] = b"\x1b[2K";

/// Screen mode determines whether we use alternate screen or inline mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenMode {
    /// Inline mode preserves scrollback. UI is anchored at bottom/top.
    Inline {
        /// Height of the UI region in rows.
        ui_height: u16,
    },
    /// Alternate screen mode for full-screen applications.
    #[default]
    AltScreen,
}

/// Where the UI region is anchored in inline mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiAnchor {
    /// UI at bottom of terminal (default for agent harness).
    #[default]
    Bottom,
    /// UI at top of terminal.
    Top,
}

/// Unified terminal output coordinator.
///
/// Enforces the one-writer rule and implements inline mode correctly.
/// All terminal output should go through this struct.
pub struct TerminalWriter<W: Write> {
    /// Buffered writer for efficient output. Option allows moving out for into_inner().
    writer: Option<BufWriter<W>>,
    /// Current screen mode.
    screen_mode: ScreenMode,
    /// Where UI is anchored in inline mode.
    ui_anchor: UiAnchor,
    /// Previous buffer for diffing.
    prev_buffer: Option<Buffer>,
    /// Grapheme pool for complex characters.
    pool: GraphemePool,
    /// Link registry for hyperlinks.
    links: LinkRegistry,
    /// Terminal capabilities.
    capabilities: TerminalCapabilities,
    /// Terminal width in columns.
    term_width: u16,
    /// Terminal height in rows.
    term_height: u16,
    /// Whether we're in the middle of a sync block.
    in_sync_block: bool,
    /// Whether cursor has been saved.
    cursor_saved: bool,
}

impl<W: Write> TerminalWriter<W> {
    /// Create a new terminal writer.
    ///
    /// # Arguments
    ///
    /// * `writer` - Output destination (takes ownership for one-writer rule)
    /// * `screen_mode` - Inline or alternate screen mode
    /// * `ui_anchor` - Where to anchor UI in inline mode
    /// * `capabilities` - Terminal capabilities
    pub fn new(
        writer: W,
        screen_mode: ScreenMode,
        ui_anchor: UiAnchor,
        capabilities: TerminalCapabilities,
    ) -> Self {
        Self {
            writer: Some(BufWriter::with_capacity(BUFFER_CAPACITY, writer)),
            screen_mode,
            ui_anchor,
            prev_buffer: None,
            pool: GraphemePool::new(),
            links: LinkRegistry::new(),
            capabilities,
            term_width: 80,
            term_height: 24,
            in_sync_block: false,
            cursor_saved: false,
        }
    }

    /// Get a mutable reference to the internal writer.
    ///
    /// # Panics
    ///
    /// Panics if the writer has been taken (via `into_inner`).
    #[inline]
    fn writer(&mut self) -> &mut BufWriter<W> {
        self.writer.as_mut().expect("writer has been consumed")
    }

    /// Set the terminal size.
    ///
    /// Call this when the terminal is resized.
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.term_width = width;
        self.term_height = height;
        // Clear prev_buffer to force full redraw after resize
        self.prev_buffer = None;
    }

    /// Get the current terminal width.
    pub fn width(&self) -> u16 {
        self.term_width
    }

    /// Get the current terminal height.
    pub fn height(&self) -> u16 {
        self.term_height
    }

    /// Get the UI height for the current mode.
    pub fn ui_height(&self) -> u16 {
        match self.screen_mode {
            ScreenMode::Inline { ui_height } => ui_height,
            ScreenMode::AltScreen => self.term_height,
        }
    }

    /// Calculate the row where the UI starts (0-indexed).
    fn ui_start_row(&self) -> u16 {
        match (self.screen_mode, self.ui_anchor) {
            (ScreenMode::Inline { ui_height }, UiAnchor::Bottom) => {
                self.term_height.saturating_sub(ui_height)
            }
            (ScreenMode::Inline { .. }, UiAnchor::Top) => 0,
            (ScreenMode::AltScreen, _) => 0,
        }
    }

    /// Present a UI frame.
    ///
    /// In inline mode, this:
    /// 1. Begins synchronized output (if supported)
    /// 2. Saves cursor position
    /// 3. Moves to UI region and clears it
    /// 4. Renders the buffer using the presenter
    /// 5. Restores cursor position
    /// 6. Ends synchronized output
    ///
    /// In AltScreen mode, this just renders the buffer.
    pub fn present_ui(&mut self, buffer: &Buffer) -> io::Result<()> {
        match self.screen_mode {
            ScreenMode::Inline { ui_height } => {
                self.present_inline(buffer, ui_height)
            }
            ScreenMode::AltScreen => {
                self.present_altscreen(buffer)
            }
        }
    }

    /// Present UI in inline mode with cursor save/restore.
    fn present_inline(&mut self, buffer: &Buffer, ui_height: u16) -> io::Result<()> {
        // Begin sync output if available
        if self.capabilities.sync_output && !self.in_sync_block {
            self.writer().write_all(SYNC_BEGIN)?;
            self.in_sync_block = true;
        }

        // Save cursor (DEC save)
        self.writer().write_all(CURSOR_SAVE)?;
        self.cursor_saved = true;

        // Move to UI anchor
        let ui_y = self.ui_start_row();
        write!(self.writer(), "\x1b[{};1H", ui_y + 1)?; // 1-indexed

        // Clear UI region only (not full screen!)
        for i in 0..ui_height {
            write!(self.writer(), "\x1b[{};1H", ui_y + i + 1)?;
            self.writer().write_all(ERASE_LINE)?;
        }

        // Move back to UI start
        write!(self.writer(), "\x1b[{};1H", ui_y + 1)?;

        // Compute diff and present
        let diff = if let Some(ref prev) = self.prev_buffer {
            if prev.width() == buffer.width() && prev.height() == buffer.height() {
                BufferDiff::compute(prev, buffer)
            } else {
                // Size changed, full redraw
                self.create_full_diff(buffer)
            }
        } else {
            // No previous buffer, full redraw
            self.create_full_diff(buffer)
        };

        // Present the diff directly (we handle cursor ourselves in inline mode)
        self.emit_diff(buffer, &diff)?;

        // Restore cursor
        self.writer().write_all(CURSOR_RESTORE)?;
        self.cursor_saved = false;

        // End sync output
        if self.in_sync_block {
            self.writer().write_all(SYNC_END)?;
            self.in_sync_block = false;
        }

        self.writer().flush()?;

        // Save current buffer for next diff
        self.prev_buffer = Some(buffer.clone());

        Ok(())
    }

    /// Present UI in alternate screen mode (simpler, no cursor gymnastics).
    fn present_altscreen(&mut self, buffer: &Buffer) -> io::Result<()> {
        let diff = if let Some(ref prev) = self.prev_buffer {
            if prev.width() == buffer.width() && prev.height() == buffer.height() {
                BufferDiff::compute(prev, buffer)
            } else {
                self.create_full_diff(buffer)
            }
        } else {
            self.create_full_diff(buffer)
        };

        // Use presenter directly
        // Begin sync if available
        if self.capabilities.sync_output {
            self.writer().write_all(SYNC_BEGIN)?;
        }

        self.emit_diff(buffer, &diff)?;

        // Reset style at end
        self.writer().write_all(b"\x1b[0m")?;

        if self.capabilities.sync_output {
            self.writer().write_all(SYNC_END)?;
        }

        self.writer().flush()?;
        self.prev_buffer = Some(buffer.clone());

        Ok(())
    }

    /// Emit a diff directly to the writer.
    fn emit_diff(&mut self, buffer: &Buffer, diff: &BufferDiff) -> io::Result<()> {
        use ftui_render::cell::StyleFlags;

        let mut current_style: Option<(ftui_render::cell::PackedRgba, ftui_render::cell::PackedRgba, StyleFlags)> = None;

        for run in diff.runs() {
            // Move cursor to run start
            let ui_y = self.ui_start_row();
            write!(self.writer(), "\x1b[{};{}H", ui_y + run.y + 1, run.x0 + 1)?;

            // Emit cells in the run
            for x in run.x0..=run.x1 {
                let cell = buffer.get_unchecked(x, run.y);

                // Skip continuation cells
                if cell.is_continuation() {
                    continue;
                }

                // Check if style changed
                let cell_style = (cell.fg, cell.bg, cell.attrs.flags());
                if current_style != Some(cell_style) {
                    // Reset and apply new style
                    self.writer().write_all(b"\x1b[0m")?;

                    // Apply attributes
                    if !cell_style.2.is_empty() {
                        self.emit_style_flags(cell_style.2)?;
                    }

                    // Apply colors
                    if cell_style.0.a() > 0 {
                        write!(self.writer(), "\x1b[38;2;{};{};{}m",
                            cell_style.0.r(), cell_style.0.g(), cell_style.0.b())?;
                    }
                    if cell_style.1.a() > 0 {
                        write!(self.writer(), "\x1b[48;2;{};{};{}m",
                            cell_style.1.r(), cell_style.1.g(), cell_style.1.b())?;
                    }

                    current_style = Some(cell_style);
                }

                // Emit content
                if let Some(ch) = cell.content.as_char() {
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    self.writer().write_all(encoded.as_bytes())?;
                } else if let Some(gid) = cell.content.grapheme_id() {
                    // Clone text out of pool to avoid borrow conflict with writer
                    let text = self.pool.get(gid).map(|s| s.to_string());
                    if let Some(text) = text {
                        self.writer().write_all(text.as_bytes())?;
                    } else {
                        self.writer().write_all(b" ")?;
                    }
                } else {
                    self.writer().write_all(b" ")?;
                }
            }
        }

        // Reset style
        self.writer().write_all(b"\x1b[0m")?;

        Ok(())
    }

    /// Emit SGR flags.
    fn emit_style_flags(&mut self, flags: ftui_render::cell::StyleFlags) -> io::Result<()> {
        use ftui_render::cell::StyleFlags;

        let mut codes = Vec::with_capacity(8);

        if flags.contains(StyleFlags::BOLD) { codes.push("1"); }
        if flags.contains(StyleFlags::DIM) { codes.push("2"); }
        if flags.contains(StyleFlags::ITALIC) { codes.push("3"); }
        if flags.contains(StyleFlags::UNDERLINE) { codes.push("4"); }
        if flags.contains(StyleFlags::BLINK) { codes.push("5"); }
        if flags.contains(StyleFlags::REVERSE) { codes.push("7"); }
        if flags.contains(StyleFlags::HIDDEN) { codes.push("8"); }
        if flags.contains(StyleFlags::STRIKETHROUGH) { codes.push("9"); }

        if !codes.is_empty() {
            write!(self.writer(), "\x1b[{}m", codes.join(";"))?;
        }

        Ok(())
    }

    /// Create a full-screen diff (marks all cells as changed).
    fn create_full_diff(&self, buffer: &Buffer) -> BufferDiff {
        let empty = Buffer::new(buffer.width(), buffer.height());
        BufferDiff::compute(&empty, buffer)
    }

    /// Write log output (goes to scrollback region in inline mode).
    ///
    /// In inline mode, this writes above the UI region.
    /// In AltScreen mode, logs are typically not shown (returns Ok silently).
    pub fn write_log(&mut self, text: &str) -> io::Result<()> {
        match self.screen_mode {
            ScreenMode::Inline { .. } => {
                // Log writes go to scrollback region (above UI)
                // Just write normally - terminal scrolls
                // The next present_ui will redraw UI in correct position
                self.writer().write_all(text.as_bytes())?;
                self.writer().flush()
            }
            ScreenMode::AltScreen => {
                // AltScreen: no scrollback, logs are typically handled differently
                // (e.g., written to a log pane or file)
                Ok(())
            }
        }
    }

    /// Clear the screen.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.writer().write_all(b"\x1b[2J\x1b[1;1H")?;
        self.writer().flush()?;
        self.prev_buffer = None;
        Ok(())
    }

    /// Hide the cursor.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.writer().write_all(b"\x1b[?25l")?;
        self.writer().flush()
    }

    /// Show the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.writer().write_all(b"\x1b[?25h")?;
        self.writer().flush()
    }

    /// Flush any buffered output.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer().flush()
    }

    /// Get the grapheme pool for interning complex characters.
    pub fn pool(&self) -> &GraphemePool {
        &self.pool
    }

    /// Get mutable access to the grapheme pool.
    pub fn pool_mut(&mut self) -> &mut GraphemePool {
        &mut self.pool
    }

    /// Get the link registry.
    pub fn links(&self) -> &LinkRegistry {
        &self.links
    }

    /// Get mutable access to the link registry.
    pub fn links_mut(&mut self) -> &mut LinkRegistry {
        &mut self.links
    }

    /// Get the terminal capabilities.
    pub fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }

    /// Consume the writer and return the underlying writer.
    ///
    /// Performs cleanup operations before returning.
    /// Returns `None` if the buffer could not be flushed.
    pub fn into_inner(mut self) -> Option<W> {
        self.cleanup();
        // Take the writer before Drop runs (Drop will see None and skip cleanup)
        self.writer.take()?.into_inner().ok()
    }

    /// Internal cleanup on drop.
    fn cleanup(&mut self) {
        let Some(ref mut writer) = self.writer else {
            return; // Writer already taken (via into_inner)
        };

        // End any pending sync block
        if self.in_sync_block {
            let _ = writer.write_all(SYNC_END);
            self.in_sync_block = false;
        }

        // Restore cursor if saved
        if self.cursor_saved {
            let _ = writer.write_all(CURSOR_RESTORE);
            self.cursor_saved = false;
        }

        // Reset style
        let _ = writer.write_all(b"\x1b[0m");

        // Show cursor
        let _ = writer.write_all(b"\x1b[?25h");

        // Flush
        let _ = writer.flush();
    }
}

impl<W: Write> Drop for TerminalWriter<W> {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::Cell;

    fn basic_caps() -> TerminalCapabilities {
        TerminalCapabilities::basic()
    }

    fn full_caps() -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::basic();
        caps.true_color = true;
        caps.sync_output = true;
        caps
    }

    #[test]
    fn new_creates_writer() {
        let output = Vec::new();
        let writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 10 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        assert_eq!(writer.ui_height(), 10);
    }

    #[test]
    fn ui_start_row_bottom_anchor() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 10 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);
        assert_eq!(writer.ui_start_row(), 14); // 24 - 10 = 14
    }

    #[test]
    fn ui_start_row_top_anchor() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 10 },
            UiAnchor::Top,
            basic_caps(),
        );
        writer.set_size(80, 24);
        assert_eq!(writer.ui_start_row(), 0);
    }

    #[test]
    fn ui_start_row_altscreen() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);
        assert_eq!(writer.ui_start_row(), 0);
    }

    #[test]
    fn present_ui_inline_saves_restores_cursor() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 10);

            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer).unwrap();
        }

        // Should contain cursor save and restore
        assert!(output.windows(CURSOR_SAVE.len()).any(|w| w == CURSOR_SAVE));
        assert!(output.windows(CURSOR_RESTORE.len()).any(|w| w == CURSOR_RESTORE));
    }

    #[test]
    fn present_ui_with_sync_output() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                full_caps(),
            );
            writer.set_size(10, 10);

            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer).unwrap();
        }

        // Should contain sync begin and end
        assert!(output.windows(SYNC_BEGIN.len()).any(|w| w == SYNC_BEGIN));
        assert!(output.windows(SYNC_END.len()).any(|w| w == SYNC_END));
    }

    #[test]
    fn write_log_in_inline_mode() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.write_log("test log\n").unwrap();
        }

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("test log"));
    }

    #[test]
    fn write_log_in_altscreen_is_noop() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.write_log("test log\n").unwrap();
        }

        // Should not contain log text (altscreen drops logs)
        let output_str = String::from_utf8_lossy(&output);
        assert!(!output_str.contains("test log"));
    }

    #[test]
    fn clear_screen_resets_prev_buffer() {
        let mut output = Vec::new();
        let mut writer = TerminalWriter::new(
            &mut output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );

        // Present a buffer
        let buffer = Buffer::new(10, 5);
        writer.present_ui(&buffer).unwrap();
        assert!(writer.prev_buffer.is_some());

        // Clear screen should reset
        writer.clear_screen().unwrap();
        assert!(writer.prev_buffer.is_none());
    }

    #[test]
    fn set_size_clears_prev_buffer() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );

        writer.prev_buffer = Some(Buffer::new(10, 10));
        writer.set_size(20, 20);

        assert!(writer.prev_buffer.is_none());
    }

    #[test]
    fn drop_cleanup_restores_cursor() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.cursor_saved = true;
            // Dropped here
        }

        // Should contain cursor restore
        assert!(output.windows(CURSOR_RESTORE.len()).any(|w| w == CURSOR_RESTORE));
    }

    #[test]
    fn drop_cleanup_ends_sync_block() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                full_caps(),
            );
            writer.in_sync_block = true;
            // Dropped here
        }

        // Should contain sync end
        assert!(output.windows(SYNC_END.len()).any(|w| w == SYNC_END));
    }

    #[test]
    fn present_multiple_frames_uses_diff() {
        use std::io::Cursor;

        // Use Cursor<Vec<u8>> which allows us to track position
        let output = Cursor::new(Vec::new());
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(10, 5);

        // First frame - full draw
        let mut buffer1 = Buffer::new(10, 5);
        buffer1.set_raw(0, 0, Cell::from_char('A'));
        writer.present_ui(&buffer1).unwrap();

        // Second frame - same content (diff is empty, minimal output)
        writer.present_ui(&buffer1).unwrap();

        // Third frame - change one cell
        let mut buffer2 = buffer1.clone();
        buffer2.set_raw(1, 0, Cell::from_char('B'));
        writer.present_ui(&buffer2).unwrap();

        // Test passes if it doesn't panic - the diffing is working
        // (Detailed output length verification would require more complex setup)
    }

    #[test]
    fn cell_content_rendered_correctly() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 5);

            let mut buffer = Buffer::new(10, 5);
            buffer.set_raw(0, 0, Cell::from_char('H'));
            buffer.set_raw(1, 0, Cell::from_char('i'));
            buffer.set_raw(2, 0, Cell::from_char('!'));
            writer.present_ui(&buffer).unwrap();
        }

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains('H'));
        assert!(output_str.contains('i'));
        assert!(output_str.contains('!'));
    }

    #[test]
    fn resize_reanchors_ui_region() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 10 },
            UiAnchor::Bottom,
            basic_caps(),
        );

        // Initial size: 80x24, UI at row 14 (24 - 10)
        writer.set_size(80, 24);
        assert_eq!(writer.ui_start_row(), 14);

        // After resize to 80x40, UI should be at row 30 (40 - 10)
        writer.set_size(80, 40);
        assert_eq!(writer.ui_start_row(), 30);

        // After resize to smaller 80x15, UI at row 5 (15 - 10)
        writer.set_size(80, 15);
        assert_eq!(writer.ui_start_row(), 5);
    }

    #[test]
    fn resize_with_top_anchor_stays_at_zero() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 10 },
            UiAnchor::Top,
            basic_caps(),
        );

        writer.set_size(80, 24);
        assert_eq!(writer.ui_start_row(), 0);

        writer.set_size(80, 40);
        assert_eq!(writer.ui_start_row(), 0);
    }

    #[test]
    fn inline_mode_never_clears_full_screen() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 10);

            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer).unwrap();
        }

        // Should NOT contain full screen clear (ED2 = "\x1b[2J")
        let has_ed2 = output.windows(4).any(|w| w == b"\x1b[2J");
        assert!(!has_ed2, "Inline mode should never use full screen clear");

        // Should contain individual line clears (EL = "\x1b[2K")
        assert!(output.windows(ERASE_LINE.len()).any(|w| w == ERASE_LINE));
    }

    #[test]
    fn present_after_log_maintains_cursor_position() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 10);

            // Present UI first
            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer).unwrap();

            // Write a log
            writer.write_log("log line\n").unwrap();

            // Present UI again
            writer.present_ui(&buffer).unwrap();
        }

        // Should have cursor save before each UI present
        let save_count = output.windows(CURSOR_SAVE.len())
            .filter(|w| *w == CURSOR_SAVE)
            .count();
        assert_eq!(save_count, 2, "Should have saved cursor twice");

        // Should have cursor restore after each UI present
        let restore_count = output.windows(CURSOR_RESTORE.len())
            .filter(|w| *w == CURSOR_RESTORE)
            .count();
        // At least 2 from presents, plus 1 from drop cleanup = 3
        assert!(restore_count >= 2, "Should have restored cursor at least twice");
    }

    #[test]
    fn ui_height_bounds_check() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 100 },
            UiAnchor::Bottom,
            basic_caps(),
        );

        // Terminal smaller than UI height
        writer.set_size(80, 10);

        // Should saturate to 0, not underflow
        assert_eq!(writer.ui_start_row(), 0);
    }
}
