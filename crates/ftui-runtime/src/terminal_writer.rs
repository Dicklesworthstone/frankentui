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

use ftui_core::inline_mode::InlineStrategy;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::link_registry::LinkRegistry;
use tracing::{debug_span, info_span, trace};

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
    /// Inline mode rendering strategy (selected from capabilities).
    inline_strategy: InlineStrategy,
    /// Whether a scroll region is currently active.
    scroll_region_active: bool,
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
        let inline_strategy = InlineStrategy::select(&capabilities);
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
            inline_strategy,
            scroll_region_active: false,
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
        // Reset scroll region on resize; it will be re-established on next present
        if self.scroll_region_active {
            let _ = self.deactivate_scroll_region();
        }
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

    /// Get the inline mode rendering strategy.
    pub fn inline_strategy(&self) -> InlineStrategy {
        self.inline_strategy
    }

    /// Check if a scroll region is currently active.
    pub fn scroll_region_active(&self) -> bool {
        self.scroll_region_active
    }

    /// Activate the scroll region for inline mode.
    ///
    /// Sets DECSTBM to constrain scrolling to the log region (above UI).
    /// Only called when the strategy permits scroll-region usage.
    fn activate_scroll_region(&mut self, ui_height: u16) -> io::Result<()> {
        if self.scroll_region_active {
            return Ok(());
        }

        let log_bottom = self.term_height.saturating_sub(ui_height);
        if log_bottom > 0 {
            // DECSTBM: set scroll region to rows 1..log_bottom (1-indexed)
            write!(self.writer(), "\x1b[1;{}r", log_bottom)?;
            self.scroll_region_active = true;
        }
        Ok(())
    }

    /// Deactivate the scroll region, resetting to full screen.
    fn deactivate_scroll_region(&mut self) -> io::Result<()> {
        if self.scroll_region_active {
            self.writer().write_all(b"\x1b[r")?;
            self.scroll_region_active = false;
        }
        Ok(())
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
        let mode_str = match self.screen_mode {
            ScreenMode::Inline { .. } => "inline",
            ScreenMode::AltScreen => "altscreen",
        };
        let _span = info_span!(
            "present_ui",
            mode = mode_str,
            width = buffer.width(),
            height = buffer.height(),
        )
        .entered();

        match self.screen_mode {
            ScreenMode::Inline { ui_height } => self.present_inline(buffer, ui_height),
            ScreenMode::AltScreen => self.present_altscreen(buffer),
        }
    }

    /// Present UI in inline mode with cursor save/restore.
    ///
    /// When the scroll-region strategy is active, DECSTBM is set to constrain
    /// log scrolling to the region above the UI. This prevents log output from
    /// overwriting the UI, reducing redraw work.
    fn present_inline(&mut self, buffer: &Buffer, ui_height: u16) -> io::Result<()> {
        // Activate scroll region if strategy calls for it
        {
            let _span = debug_span!("scroll_region").entered();
            match self.inline_strategy {
                InlineStrategy::ScrollRegion => {
                    self.activate_scroll_region(ui_height)?;
                }
                InlineStrategy::Hybrid => {
                    self.activate_scroll_region(ui_height)?;
                }
                InlineStrategy::OverlayRedraw => {}
            }
        }

        // Begin sync output if available
        if self.capabilities.sync_output && !self.in_sync_block {
            self.writer().write_all(SYNC_BEGIN)?;
            self.in_sync_block = true;
        }

        // Save cursor (DEC save)
        self.writer().write_all(CURSOR_SAVE)?;
        self.cursor_saved = true;

        // Move to UI anchor and clear UI region
        {
            let _span = debug_span!("clear_ui", rows = ui_height).entered();
            let ui_y = self.ui_start_row();
            write!(self.writer(), "\x1b[{};1H", ui_y + 1)?;

            for i in 0..ui_height {
                write!(self.writer(), "\x1b[{};1H", ui_y + i + 1)?;
                self.writer().write_all(ERASE_LINE)?;
            }

            write!(self.writer(), "\x1b[{};1H", ui_y + 1)?;
        }

        // Compute diff
        let diff = {
            let _span = debug_span!("diff_compute").entered();
            if let Some(ref prev) = self.prev_buffer {
                if prev.width() == buffer.width() && prev.height() == buffer.height() {
                    BufferDiff::compute(prev, buffer)
                } else {
                    self.create_full_diff(buffer)
                }
            } else {
                self.create_full_diff(buffer)
            }
        };

        // Emit diff
        {
            let _span = debug_span!("emit").entered();
            self.emit_diff(buffer, &diff)?;
        }

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
        let diff = {
            let _span = debug_span!("diff_compute").entered();
            if let Some(ref prev) = self.prev_buffer {
                if prev.width() == buffer.width() && prev.height() == buffer.height() {
                    BufferDiff::compute(prev, buffer)
                } else {
                    self.create_full_diff(buffer)
                }
            } else {
                self.create_full_diff(buffer)
            }
        };

        // Begin sync if available
        if self.capabilities.sync_output {
            self.writer().write_all(SYNC_BEGIN)?;
        }

        {
            let _span = debug_span!("emit").entered();
            self.emit_diff(buffer, &diff)?;
        }

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
        use ftui_render::cell::{CellAttrs, StyleFlags};

        let runs = diff.runs();
        let _span = debug_span!("emit_diff", run_count = runs.len()).entered();

        let mut current_style: Option<(
            ftui_render::cell::PackedRgba,
            ftui_render::cell::PackedRgba,
            StyleFlags,
        )> = None;
        let mut current_link: Option<u32> = None;

        let ui_y_start = self.ui_start_row();
        // Borrow writer once
        let writer = self.writer.as_mut().expect("writer has been consumed");

        for run in runs {
            // Move cursor to run start
            write!(writer, "\x1b[{};{}H", ui_y_start + run.y + 1, run.x0 + 1)?;

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
                    writer.write_all(b"\x1b[0m")?;

                    // Apply attributes
                    if !cell_style.2.is_empty() {
                        Self::emit_style_flags(writer, cell_style.2)?;
                    }

                    // Apply colors
                    if cell_style.0.a() > 0 {
                        write!(
                            writer,
                            "\x1b[38;2;{};{};{}m",
                            cell_style.0.r(),
                            cell_style.0.g(),
                            cell_style.0.b()
                        )?;
                    }
                    if cell_style.1.a() > 0 {
                        write!(
                            writer,
                            "\x1b[48;2;{};{};{}m",
                            cell_style.1.r(),
                            cell_style.1.g(),
                            cell_style.1.b()
                        )?;
                    }

                    current_style = Some(cell_style);
                }

                // Check if link changed
                let raw_link_id = cell.attrs.link_id();
                let new_link = if raw_link_id == CellAttrs::LINK_ID_NONE {
                    None
                } else {
                    Some(raw_link_id)
                };

                if current_link != new_link {
                    // Close current link
                    if current_link.is_some() {
                        writer.write_all(b"\x1b]8;;\x1b\\")?;
                    }
                    // Open new link if present
                    if let Some(link_id) = new_link
                        && let Some(url) = self.links.get(link_id)
                    {
                        write!(writer, "\x1b]8;;{}\x1b\\", url)?;
                    }
                    current_link = new_link;
                }

                // Emit content
                if let Some(ch) = cell.content.as_char() {
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    writer.write_all(encoded.as_bytes())?;
                } else if let Some(gid) = cell.content.grapheme_id() {
                    // Use pool directly with writer (no clone needed)
                    if let Some(text) = self.pool.get(gid) {
                        writer.write_all(text.as_bytes())?;
                    } else {
                        writer.write_all(b" ")?;
                    }
                } else {
                    writer.write_all(b" ")?;
                }
            }
        }

        // Reset style
        writer.write_all(b"\x1b[0m")?;
        
        // Close any open link
        if current_link.is_some() {
            writer.write_all(b"\x1b]8;;\x1b\\")?;
        }

        trace!("emit_diff complete");
        Ok(())
    }

    /// Emit SGR flags.
    fn emit_style_flags(writer: &mut impl Write, flags: ftui_render::cell::StyleFlags) -> io::Result<()> {
        use ftui_render::cell::StyleFlags;

        let mut codes = Vec::with_capacity(8);

        if flags.contains(StyleFlags::BOLD) {
            codes.push("1");
        }
        if flags.contains(StyleFlags::DIM) {
            codes.push("2");
        }
        if flags.contains(StyleFlags::ITALIC) {
            codes.push("3");
        }
        if flags.contains(StyleFlags::UNDERLINE) {
            codes.push("4");
        }
        if flags.contains(StyleFlags::BLINK) {
            codes.push("5");
        }
        if flags.contains(StyleFlags::REVERSE) {
            codes.push("7");
        }
        if flags.contains(StyleFlags::HIDDEN) {
            codes.push("8");
        }
        if flags.contains(StyleFlags::STRIKETHROUGH) {
            codes.push("9");
        }

        if !codes.is_empty() {
            write!(writer, "\x1b[{}m", codes.join(";"))?;
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

        // Reset scroll region if active
        if self.scroll_region_active {
            let _ = writer.write_all(b"\x1b[r");
            self.scroll_region_active = false;
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
        assert!(
            output
                .windows(CURSOR_RESTORE.len())
                .any(|w| w == CURSOR_RESTORE)
        );
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
        assert!(
            output
                .windows(CURSOR_RESTORE.len())
                .any(|w| w == CURSOR_RESTORE)
        );
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
        let save_count = output
            .windows(CURSOR_SAVE.len())
            .filter(|w| *w == CURSOR_SAVE)
            .count();
        assert_eq!(save_count, 2, "Should have saved cursor twice");

        // Should have cursor restore after each UI present
        let restore_count = output
            .windows(CURSOR_RESTORE.len())
            .filter(|w| *w == CURSOR_RESTORE)
            .count();
        // At least 2 from presents, plus 1 from drop cleanup = 3
        assert!(
            restore_count >= 2,
            "Should have restored cursor at least twice"
        );
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

    // --- Scroll-region optimization tests ---

    /// Capabilities that enable scroll-region strategy (no mux, scroll_region + sync_output).
    fn scroll_region_caps() -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::basic();
        caps.scroll_region = true;
        caps.sync_output = true;
        caps
    }

    /// Capabilities for hybrid strategy (scroll_region but no sync_output).
    fn hybrid_caps() -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::basic();
        caps.scroll_region = true;
        caps
    }

    /// Capabilities that force overlay (in tmux even with scroll_region).
    fn mux_caps() -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::basic();
        caps.scroll_region = true;
        caps.sync_output = true;
        caps.in_tmux = true;
        caps
    }

    #[test]
    fn strategy_selected_from_capabilities() {
        // No capabilities → OverlayRedraw
        let w = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        assert_eq!(w.inline_strategy(), InlineStrategy::OverlayRedraw);

        // scroll_region + sync_output → ScrollRegion
        let w = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            scroll_region_caps(),
        );
        assert_eq!(w.inline_strategy(), InlineStrategy::ScrollRegion);

        // scroll_region only → Hybrid
        let w = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            hybrid_caps(),
        );
        assert_eq!(w.inline_strategy(), InlineStrategy::Hybrid);

        // In mux → OverlayRedraw even with all caps
        let w = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            mux_caps(),
        );
        assert_eq!(w.inline_strategy(), InlineStrategy::OverlayRedraw);
    }

    #[test]
    fn scroll_region_activated_on_present() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                scroll_region_caps(),
            );
            writer.set_size(80, 24);
            assert!(!writer.scroll_region_active());

            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer).unwrap();
            assert!(writer.scroll_region_active());
        }

        // Should contain DECSTBM: ESC [ 1 ; 19 r (rows 1-19 are log region)
        let expected = b"\x1b[1;19r";
        assert!(
            output.windows(expected.len()).any(|w| w == expected),
            "Should set scroll region to rows 1-19"
        );
    }

    #[test]
    fn scroll_region_not_activated_for_overlay() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer).unwrap();
            assert!(!writer.scroll_region_active());
        }

        // Should NOT contain any scroll region setup
        let decstbm = b"\x1b[1;19r";
        assert!(
            !output.windows(decstbm.len()).any(|w| w == decstbm),
            "OverlayRedraw should not set scroll region"
        );
    }

    #[test]
    fn scroll_region_not_activated_in_mux() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                mux_caps(),
            );
            writer.set_size(80, 24);

            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer).unwrap();
            assert!(!writer.scroll_region_active());
        }

        // Should NOT contain scroll region setup despite having the capability
        let decstbm = b"\x1b[1;19r";
        assert!(
            !output.windows(decstbm.len()).any(|w| w == decstbm),
            "Mux environment should not use scroll region"
        );
    }

    #[test]
    fn scroll_region_reset_on_cleanup() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                scroll_region_caps(),
            );
            writer.set_size(80, 24);

            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer).unwrap();
            // Dropped here - cleanup should reset scroll region
        }

        // Should contain scroll region reset: ESC [ r
        let reset = b"\x1b[r";
        assert!(
            output.windows(reset.len()).any(|w| w == reset),
            "Cleanup should reset scroll region"
        );
    }

    #[test]
    fn scroll_region_reset_on_resize() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            scroll_region_caps(),
        );
        writer.set_size(80, 24);

        // Manually activate scroll region
        writer.activate_scroll_region(5).unwrap();
        assert!(writer.scroll_region_active());

        // Resize should deactivate it
        writer.set_size(80, 40);
        assert!(!writer.scroll_region_active());
    }

    #[test]
    fn scroll_region_reactivated_after_resize() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                scroll_region_caps(),
            );
            writer.set_size(80, 24);

            // First present activates scroll region
            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer).unwrap();
            assert!(writer.scroll_region_active());

            // Resize deactivates
            writer.set_size(80, 40);
            assert!(!writer.scroll_region_active());

            // Next present re-activates with new dimensions
            let buffer2 = Buffer::new(80, 5);
            writer.present_ui(&buffer2).unwrap();
            assert!(writer.scroll_region_active());
        }

        // Should contain the new scroll region: ESC [ 1 ; 35 r (40 - 5 = 35)
        let new_region = b"\x1b[1;35r";
        assert!(
            output.windows(new_region.len()).any(|w| w == new_region),
            "Should set scroll region to new dimensions after resize"
        );
    }

    #[test]
    fn hybrid_strategy_activates_scroll_region() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                hybrid_caps(),
            );
            writer.set_size(80, 24);

            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer).unwrap();
            assert!(writer.scroll_region_active());
        }

        // Hybrid uses scroll region as internal optimization
        let expected = b"\x1b[1;19r";
        assert!(
            output.windows(expected.len()).any(|w| w == expected),
            "Hybrid should activate scroll region as optimization"
        );
    }

    #[test]
    fn altscreen_does_not_activate_scroll_region() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            scroll_region_caps(),
        );
        writer.set_size(80, 24);

        let buffer = Buffer::new(80, 24);
        writer.present_ui(&buffer).unwrap();
        assert!(!writer.scroll_region_active());
    }

    #[test]
    fn scroll_region_still_saves_restores_cursor() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                scroll_region_caps(),
            );
            writer.set_size(80, 24);

            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer).unwrap();
        }

        // Even with scroll region, cursor save/restore is used for UI presents
        assert!(
            output.windows(CURSOR_SAVE.len()).any(|w| w == CURSOR_SAVE),
            "Scroll region mode should still save cursor"
        );
        assert!(
            output
                .windows(CURSOR_RESTORE.len())
                .any(|w| w == CURSOR_RESTORE),
            "Scroll region mode should still restore cursor"
        );
    }
}
