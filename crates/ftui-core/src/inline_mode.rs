#![forbid(unsafe_code)]

//! Inline Mode Spike: Validates correctness-first inline mode strategies.
//!
//! This module implements the Phase -1 spike (bd-10i.1.1) to validate inline mode
//! strategies for FrankenTUI. Inline mode preserves terminal scrollback while
//! rendering a stable UI region + streaming logs.
//!
//! # Strategies Implemented
//!
//! - **Strategy A (Scroll-Region)**: Uses DECSTBM to constrain scrolling to a region.
//! - **Strategy B (Overlay-Redraw)**: Save cursor, clear UI, write logs, redraw UI, restore.
//! - **Strategy C (Hybrid)**: Overlay-redraw baseline with scroll-region optimization where safe.
//!
//! # Key Invariants
//!
//! 1. Cursor is restored after each frame present.
//! 2. Terminal modes are restored on normal exit AND panic.
//! 3. No full-screen clears in inline mode (preserves scrollback).
//! 4. One writer owns terminal output (enforced by ownership).

use std::io::{self, Write};

use unicode_width::UnicodeWidthChar;

use crate::terminal_capabilities::TerminalCapabilities;

// ============================================================================
// ANSI Escape Sequences
// ============================================================================

/// DEC cursor save (ESC 7) - more portable than CSI s.
const CURSOR_SAVE: &[u8] = b"\x1b7";

/// DEC cursor restore (ESC 8) - more portable than CSI u.
const CURSOR_RESTORE: &[u8] = b"\x1b8";

/// CSI sequence to move cursor to position (1-indexed).
fn cursor_position(row: u16, col: u16) -> Vec<u8> {
    format!("\x1b[{};{}H", row, col).into_bytes()
}

/// Set scroll region (DECSTBM): CSI top ; bottom r (1-indexed).
fn set_scroll_region(top: u16, bottom: u16) -> Vec<u8> {
    format!("\x1b[{};{}r", top, bottom).into_bytes()
}

/// Reset scroll region to full screen: CSI r.
const RESET_SCROLL_REGION: &[u8] = b"\x1b[r";

/// Erase line from cursor to end: CSI 0 K.
#[allow(dead_code)] // Kept for future use in inline mode optimization
const ERASE_TO_EOL: &[u8] = b"\x1b[0K";

/// Erase entire line: CSI 2 K.
const ERASE_LINE: &[u8] = b"\x1b[2K";

/// Synchronized output begin (DEC 2026): CSI ? 2026 h.
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";

/// Synchronized output end (DEC 2026): CSI ? 2026 l.
const SYNC_END: &[u8] = b"\x1b[?2026l";

// ============================================================================
// Inline Mode Strategy
// ============================================================================

/// Inline mode rendering strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InlineStrategy {
    /// Use scroll regions (DECSTBM) to anchor UI while logs scroll.
    /// More efficient but less portable (muxes may misbehave).
    ScrollRegion,

    /// Overlay redraw: save cursor, write logs, redraw UI, restore cursor.
    /// More portable but more redraw work.
    OverlayRedraw,

    /// Hybrid: overlay-redraw baseline with scroll-region optimization
    /// where safe (detected modern terminals without mux).
    #[default]
    Hybrid,
}

impl InlineStrategy {
    /// Select strategy based on terminal capabilities.
    ///
    /// Hybrid mode uses scroll-region only when:
    /// - Not in a terminal multiplexer (tmux/screen/zellij)
    /// - Scroll region capability is detected
    /// - Synchronized output is available (reduces flicker)
    #[must_use]
    pub fn select(caps: &TerminalCapabilities) -> Self {
        if caps.in_any_mux() {
            // Muxes may not handle scroll regions correctly
            InlineStrategy::OverlayRedraw
        } else if caps.scroll_region && caps.sync_output {
            // Modern terminal with full support
            InlineStrategy::ScrollRegion
        } else if caps.scroll_region {
            // Scroll region available but no sync output - use hybrid
            InlineStrategy::Hybrid
        } else {
            // Fallback to most portable option
            InlineStrategy::OverlayRedraw
        }
    }
}

// ============================================================================
// Inline Mode Session
// ============================================================================

/// Configuration for inline mode rendering.
#[derive(Debug, Clone, Copy)]
pub struct InlineConfig {
    /// Height of the UI region (bottom N rows).
    pub ui_height: u16,

    /// Total terminal height.
    pub term_height: u16,

    /// Total terminal width.
    pub term_width: u16,

    /// Rendering strategy to use.
    pub strategy: InlineStrategy,

    /// Use synchronized output (DEC 2026) if available.
    pub use_sync_output: bool,
}

impl InlineConfig {
    /// Create config for a UI region of given height.
    #[must_use]
    pub fn new(ui_height: u16, term_height: u16, term_width: u16) -> Self {
        Self {
            ui_height,
            term_height,
            term_width,
            strategy: InlineStrategy::default(),
            use_sync_output: false,
        }
    }

    /// Set the rendering strategy.
    #[must_use]
    pub const fn with_strategy(mut self, strategy: InlineStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Enable synchronized output.
    #[must_use]
    pub const fn with_sync_output(mut self, enabled: bool) -> Self {
        self.use_sync_output = enabled;
        self
    }

    /// Row where the UI region starts (1-indexed for ANSI).
    ///
    /// Returns at least 1 (valid ANSI row).
    #[must_use]
    pub const fn ui_top_row(&self) -> u16 {
        let row = self
            .term_height
            .saturating_sub(self.ui_height)
            .saturating_add(1);
        // Ensure we return at least row 1 (valid ANSI row)
        if row == 0 { 1 } else { row }
    }

    /// Row where the log region ends (1-indexed for ANSI).
    ///
    /// Returns 0 if there's no room for logs (UI takes full height).
    /// Callers should check for 0 before using this value.
    #[must_use]
    pub const fn log_bottom_row(&self) -> u16 {
        self.ui_top_row().saturating_sub(1)
    }

    /// Check if the configuration is valid for inline mode.
    ///
    /// Returns `true` if there's room for both logs and UI.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.ui_height > 0 && self.ui_height < self.term_height && self.term_height > 1
    }
}

// ============================================================================
// Inline Mode Renderer
// ============================================================================

/// Inline mode renderer implementing the one-writer rule.
///
/// This struct owns terminal output and enforces that all writes go through it.
/// Cleanup is guaranteed via `Drop`.
pub struct InlineRenderer<W: Write> {
    writer: W,
    config: InlineConfig,
    scroll_region_set: bool,
    in_sync_block: bool,
    cursor_saved: bool,
}

impl<W: Write> InlineRenderer<W> {
    /// Create a new inline renderer.
    ///
    /// # Arguments
    /// * `writer` - The terminal output (takes ownership to enforce one-writer rule).
    /// * `config` - Inline mode configuration.
    pub fn new(writer: W, config: InlineConfig) -> Self {
        Self {
            writer,
            config,
            scroll_region_set: false,
            in_sync_block: false,
            cursor_saved: false,
        }
    }

    /// Initialize inline mode on the terminal.
    ///
    /// For scroll-region strategy, this sets up DECSTBM.
    /// For overlay/hybrid strategy, this just prepares state.
    pub fn enter(&mut self) -> io::Result<()> {
        match self.config.strategy {
            InlineStrategy::ScrollRegion => {
                // Set scroll region to log area (top of screen to just above UI)
                let log_bottom = self.config.log_bottom_row();
                if log_bottom > 0 {
                    self.writer.write_all(&set_scroll_region(1, log_bottom))?;
                    self.scroll_region_set = true;
                }
            }
            InlineStrategy::OverlayRedraw | InlineStrategy::Hybrid => {
                // No setup needed for overlay-based modes.
                // Hybrid uses overlay as baseline; scroll-region would be an
                // internal optimization applied per-operation, not upfront.
            }
        }
        self.writer.flush()
    }

    /// Exit inline mode, restoring terminal state.
    pub fn exit(&mut self) -> io::Result<()> {
        self.cleanup_internal()
    }

    /// Write log output (goes to scrollback region).
    ///
    /// In scroll-region mode: writes to current cursor position in scroll region.
    /// In overlay mode: saves cursor, writes, then restores cursor.
    ///
    /// Returns `Ok(())` even if there's no log region (logs are silently dropped
    /// when UI takes the full terminal height).
    pub fn write_log(&mut self, text: &str) -> io::Result<()> {
        let log_row = self.config.log_bottom_row();

        // If there's no room for logs, silently drop
        if log_row == 0 {
            return Ok(());
        }

        match self.config.strategy {
            InlineStrategy::ScrollRegion => {
                // Cursor should be in scroll region; just write
                self.writer.write_all(text.as_bytes())?;
            }
            InlineStrategy::OverlayRedraw | InlineStrategy::Hybrid => {
                // Save cursor, move to log area, write, restore
                self.writer.write_all(CURSOR_SAVE)?;
                self.cursor_saved = true;

                // Move to bottom of log region
                self.writer.write_all(&cursor_position(log_row, 1))?;
                self.writer.write_all(ERASE_LINE)?;

                // Keep overlay logging single-line so wraps/newlines never scribble
                // into the UI region below.
                let safe_line =
                    Self::sanitize_overlay_log_line(text, usize::from(self.config.term_width));
                if !safe_line.is_empty() {
                    self.writer.write_all(safe_line.as_bytes())?;
                }

                // Restore cursor
                self.writer.write_all(CURSOR_RESTORE)?;
                self.cursor_saved = false;
            }
        }
        self.writer.flush()
    }

    /// Present a UI frame.
    ///
    /// # Invariants
    /// - Cursor position is saved before and restored after.
    /// - UI region is redrawn without affecting scrollback.
    /// - Synchronized output wraps the operation if enabled.
    pub fn present_ui<F>(&mut self, render_fn: F) -> io::Result<()>
    where
        F: FnOnce(&mut W, &InlineConfig) -> io::Result<()>,
    {
        if !self.config.is_valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid inline mode configuration",
            ));
        }

        // Begin sync output to prevent flicker
        if self.config.use_sync_output && !self.in_sync_block {
            self.writer.write_all(SYNC_BEGIN)?;
            self.in_sync_block = true;
        }

        // Save cursor position
        self.writer.write_all(CURSOR_SAVE)?;
        self.cursor_saved = true;

        let operation_result = (|| -> io::Result<()> {
            // Move to UI region
            let ui_row = self.config.ui_top_row();
            self.writer.write_all(&cursor_position(ui_row, 1))?;

            // Clear and render each UI line
            for row in 0..self.config.ui_height {
                self.writer
                    .write_all(&cursor_position(ui_row.saturating_add(row), 1))?;
                self.writer.write_all(ERASE_LINE)?;
            }

            // Move back to start of UI and let caller render
            self.writer.write_all(&cursor_position(ui_row, 1))?;
            render_fn(&mut self.writer, &self.config)?;
            Ok(())
        })();

        // Always attempt to restore terminal state even if rendering failed.
        let restore_result = self.writer.write_all(CURSOR_RESTORE);
        if restore_result.is_ok() {
            self.cursor_saved = false;
        }

        let sync_end_result = if self.in_sync_block {
            let res = self.writer.write_all(SYNC_END);
            if res.is_ok() {
                self.in_sync_block = false;
            }
            Some(res)
        } else {
            None
        };

        let flush_result = self.writer.flush();

        operation_result?;
        restore_result?;
        if let Some(res) = sync_end_result {
            res?;
        }
        flush_result
    }

    fn sanitize_overlay_log_line(text: &str, max_cols: usize) -> String {
        if max_cols == 0 {
            return String::new();
        }

        let mut out = String::new();
        let mut used_cols = 0usize;

        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                break;
            }

            // Skip ASCII control characters so logs cannot inject cursor motion.
            if ch.is_control() {
                continue;
            }

            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if ch_width == 0 {
                // Keep combining marks only when they can attach to prior text.
                if !out.is_empty() {
                    out.push(ch);
                }
                continue;
            }

            if used_cols.saturating_add(ch_width) > max_cols {
                break;
            }

            out.push(ch);
            used_cols += ch_width;
            if used_cols == max_cols {
                break;
            }
        }

        out
    }

    /// Internal cleanup - guaranteed to run on drop.
    fn cleanup_internal(&mut self) -> io::Result<()> {
        // End any pending sync block
        if self.in_sync_block {
            let _ = self.writer.write_all(SYNC_END);
            self.in_sync_block = false;
        }

        // Reset scroll region if we set one
        if self.scroll_region_set {
            let _ = self.writer.write_all(RESET_SCROLL_REGION);
            self.scroll_region_set = false;
        }

        // Restore cursor only if we saved it (avoid restoring to stale position)
        if self.cursor_saved {
            let _ = self.writer.write_all(CURSOR_RESTORE);
            self.cursor_saved = false;
        }

        self.writer.flush()
    }
}

impl<W: Write> Drop for InlineRenderer<W> {
    fn drop(&mut self) {
        // Best-effort cleanup on drop (including panic)
        let _ = self.cleanup_internal();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    type TestWriter = Cursor<Vec<u8>>;

    fn test_writer() -> TestWriter {
        Cursor::new(Vec::new())
    }

    fn writer_contains_sequence(writer: &TestWriter, seq: &[u8]) -> bool {
        writer
            .get_ref()
            .windows(seq.len())
            .any(|window| window == seq)
    }

    fn writer_clear(writer: &mut TestWriter) {
        writer.get_mut().clear();
    }

    #[test]
    fn config_calculates_regions_correctly() {
        // 24 row terminal, 6 row UI
        let config = InlineConfig::new(6, 24, 80);
        assert_eq!(config.ui_top_row(), 19); // rows 19-24 are UI
        assert_eq!(config.log_bottom_row(), 18); // rows 1-18 are logs
    }

    #[test]
    fn strategy_selection_prefers_overlay_in_mux() {
        let mut caps = TerminalCapabilities::basic();
        caps.in_tmux = true;
        caps.scroll_region = true;
        caps.sync_output = true;

        assert_eq!(InlineStrategy::select(&caps), InlineStrategy::OverlayRedraw);
    }

    #[test]
    fn strategy_selection_uses_scroll_region_in_modern_terminal() {
        let mut caps = TerminalCapabilities::basic();
        caps.scroll_region = true;
        caps.sync_output = true;

        assert_eq!(InlineStrategy::select(&caps), InlineStrategy::ScrollRegion);
    }

    #[test]
    fn strategy_selection_uses_hybrid_without_sync() {
        let mut caps = TerminalCapabilities::basic();
        caps.scroll_region = true;
        caps.sync_output = false;

        assert_eq!(InlineStrategy::select(&caps), InlineStrategy::Hybrid);
    }

    #[test]
    fn enter_sets_scroll_region_for_scroll_strategy() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80).with_strategy(InlineStrategy::ScrollRegion);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.enter().unwrap();

        // Should set scroll region: ESC [ 1 ; 18 r
        assert!(writer_contains_sequence(&renderer.writer, b"\x1b[1;18r"));
    }

    #[test]
    fn exit_resets_scroll_region() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80).with_strategy(InlineStrategy::ScrollRegion);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.enter().unwrap();
        renderer.exit().unwrap();

        // Should reset scroll region: ESC [ r
        assert!(writer_contains_sequence(
            &renderer.writer,
            RESET_SCROLL_REGION
        ));
    }

    #[test]
    fn present_ui_saves_and_restores_cursor() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80).with_strategy(InlineStrategy::OverlayRedraw);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer
            .present_ui(|w, _| {
                w.write_all(b"UI Content")?;
                Ok(())
            })
            .unwrap();

        // Should save cursor (ESC 7)
        assert!(writer_contains_sequence(&renderer.writer, CURSOR_SAVE));
        // Should restore cursor (ESC 8)
        assert!(writer_contains_sequence(&renderer.writer, CURSOR_RESTORE));
    }

    #[test]
    fn present_ui_uses_sync_output_when_enabled() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80)
            .with_strategy(InlineStrategy::OverlayRedraw)
            .with_sync_output(true);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.present_ui(|_, _| Ok(())).unwrap();

        // Should have sync begin and end
        assert!(writer_contains_sequence(&renderer.writer, SYNC_BEGIN));
        assert!(writer_contains_sequence(&renderer.writer, SYNC_END));
    }

    #[test]
    fn drop_cleans_up_scroll_region() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80).with_strategy(InlineStrategy::ScrollRegion);

        {
            let mut renderer = InlineRenderer::new(writer, config);
            renderer.enter().unwrap();
            // Renderer dropped here
        }

        // Can't easily test drop output, but this verifies no panic
    }

    #[test]
    fn write_log_preserves_cursor_in_overlay_mode() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80).with_strategy(InlineStrategy::OverlayRedraw);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.write_log("test log\n").unwrap();

        // Should save and restore cursor
        assert!(writer_contains_sequence(&renderer.writer, CURSOR_SAVE));
        assert!(writer_contains_sequence(&renderer.writer, CURSOR_RESTORE));
    }

    #[test]
    fn write_log_overlay_truncates_to_single_safe_line() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 5).with_strategy(InlineStrategy::OverlayRedraw);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.write_log("ABCDE\nSECOND").unwrap();

        let output = String::from_utf8_lossy(renderer.writer.get_ref());
        assert!(output.contains("ABCDE"));
        assert!(!output.contains("SECOND"));
        assert!(!output.contains('\n'));
    }

    #[test]
    fn write_log_overlay_truncates_wide_chars_by_display_width() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 3).with_strategy(InlineStrategy::OverlayRedraw);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.write_log("ab界Z").unwrap();

        let output = String::from_utf8_lossy(renderer.writer.get_ref());
        assert!(output.contains("ab"));
        assert!(!output.contains('界'));
        assert!(!output.contains('Z'));
    }

    #[test]
    fn write_log_overlay_allows_wide_char_when_it_exactly_fits_width() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 4).with_strategy(InlineStrategy::OverlayRedraw);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.write_log("ab界Z").unwrap();

        let output = String::from_utf8_lossy(renderer.writer.get_ref());
        assert!(output.contains("ab界"));
        assert!(!output.contains('Z'));
    }

    #[test]
    fn hybrid_does_not_set_scroll_region_in_enter() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80).with_strategy(InlineStrategy::Hybrid);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.enter().unwrap();

        // Hybrid should NOT set scroll region (uses overlay baseline)
        assert!(!writer_contains_sequence(&renderer.writer, b"\x1b[1;18r"));
        assert!(!renderer.scroll_region_set);
    }

    #[test]
    fn config_is_valid_checks_boundaries() {
        // Valid config
        let valid = InlineConfig::new(6, 24, 80);
        assert!(valid.is_valid());

        // UI takes all rows (no room for logs)
        let full_ui = InlineConfig::new(24, 24, 80);
        assert!(!full_ui.is_valid());

        // Zero UI height
        let no_ui = InlineConfig::new(0, 24, 80);
        assert!(!no_ui.is_valid());

        // Single row terminal
        let tiny = InlineConfig::new(1, 1, 80);
        assert!(!tiny.is_valid());
    }

    #[test]
    fn log_bottom_row_zero_when_no_room() {
        // UI takes full height
        let config = InlineConfig::new(24, 24, 80);
        assert_eq!(config.log_bottom_row(), 0);
    }

    #[test]
    fn write_log_silently_drops_when_no_log_region() {
        let writer = test_writer();
        // UI takes full height - no room for logs
        let config = InlineConfig::new(24, 24, 80).with_strategy(InlineStrategy::OverlayRedraw);
        let mut renderer = InlineRenderer::new(writer, config);

        // Should succeed but not write anything meaningful
        renderer.write_log("test log\n").unwrap();

        // Should not have written cursor save/restore since we bailed early
        assert!(!writer_contains_sequence(&renderer.writer, CURSOR_SAVE));
    }

    #[test]
    fn cleanup_does_not_restore_unsaved_cursor() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80).with_strategy(InlineStrategy::ScrollRegion);
        let mut renderer = InlineRenderer::new(writer, config);

        // Just enter and exit, never save cursor explicitly
        renderer.enter().unwrap();
        writer_clear(&mut renderer.writer); // Clear output to check cleanup behavior
        renderer.exit().unwrap();

        // Should NOT restore cursor since we never saved it
        assert!(!writer_contains_sequence(&renderer.writer, CURSOR_RESTORE));
    }

    #[test]
    fn inline_strategy_default_is_hybrid() {
        assert_eq!(InlineStrategy::default(), InlineStrategy::Hybrid);
    }

    #[test]
    fn config_ui_top_row_clamps_to_1() {
        // ui_height >= term_height means saturating_sub yields 0, +1 = 1
        let config = InlineConfig::new(30, 24, 80);
        assert!(config.ui_top_row() >= 1);
    }

    #[test]
    fn strategy_select_fallback_no_scroll_no_sync() {
        let mut caps = TerminalCapabilities::basic();
        caps.scroll_region = false;
        caps.sync_output = false;
        assert_eq!(InlineStrategy::select(&caps), InlineStrategy::OverlayRedraw);
    }

    #[test]
    fn write_log_in_scroll_region_mode() {
        let writer = test_writer();
        let config = InlineConfig::new(6, 24, 80).with_strategy(InlineStrategy::ScrollRegion);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.enter().unwrap();
        renderer.write_log("hello\n").unwrap();

        // In scroll-region mode, log is written directly without cursor save/restore
        let output = renderer.writer.get_ref();
        assert!(output.windows(b"hello\n".len()).any(|w| w == b"hello\n"));
    }

    #[test]
    fn present_ui_clears_ui_lines() {
        let writer = test_writer();
        let config = InlineConfig::new(2, 10, 80).with_strategy(InlineStrategy::OverlayRedraw);
        let mut renderer = InlineRenderer::new(writer, config);

        renderer.present_ui(|_, _| Ok(())).unwrap();

        // Should contain ERASE_LINE sequences for the 2 UI rows
        let count = renderer
            .writer
            .get_ref()
            .windows(ERASE_LINE.len())
            .filter(|w| *w == ERASE_LINE)
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn present_ui_render_error_still_restores_state() {
        let writer = test_writer();
        let config = InlineConfig::new(2, 10, 80)
            .with_strategy(InlineStrategy::OverlayRedraw)
            .with_sync_output(true);
        let mut renderer = InlineRenderer::new(writer, config);

        let err = renderer
            .present_ui(|_, _| Err(io::Error::other("boom")))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);

        assert!(writer_contains_sequence(&renderer.writer, CURSOR_RESTORE));
        assert!(writer_contains_sequence(&renderer.writer, SYNC_END));
        assert!(!renderer.cursor_saved);
        assert!(!renderer.in_sync_block);
    }

    #[test]
    fn present_ui_rejects_invalid_config() {
        let writer = test_writer();
        let config = InlineConfig::new(0, 24, 80).with_strategy(InlineStrategy::OverlayRedraw);
        let mut renderer = InlineRenderer::new(writer, config);

        let err = renderer.present_ui(|_, _| Ok(())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!writer_contains_sequence(&renderer.writer, CURSOR_SAVE));
    }

    #[test]
    fn config_new_defaults() {
        let config = InlineConfig::new(5, 20, 100);
        assert_eq!(config.ui_height, 5);
        assert_eq!(config.term_height, 20);
        assert_eq!(config.term_width, 100);
        assert_eq!(config.strategy, InlineStrategy::Hybrid);
        assert!(!config.use_sync_output);
    }
}
