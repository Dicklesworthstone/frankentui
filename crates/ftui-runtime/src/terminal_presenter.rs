#![forbid(unsafe_code)]

//! Terminal presenter implementation wrapping [`TerminalWriter`].
//!
//! Provides the [`BackendPresenter`] implementation for terminal output (ANSI escape
//! sequences over stdout or arbitrary `Write` sinks).

use std::io::{self, Stdout, Write};
use std::sync::Arc;
use std::time::Duration;

use ftui_backend::{
    BackendPresenter, DiffStrategy, PresentTimings, SanitizeMode, ScreenMode, UiAnchor,
};
use ftui_core::osc52::ClipboardSelection;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::link_registry::LinkRegistry;

use crate::terminal_writer::{RuntimeDiffConfig, TerminalWriter};

/// Presenter implementation wrapping [`TerminalWriter`] for native/terminal output.
pub struct TerminalPresenter<W: Write + Send = Stdout> {
    /// Inner terminal writer handling low-level terminal I/O.
    pub writer: TerminalWriter<W>,
}

impl<W: Write + Send> TerminalPresenter<W> {
    /// Create a presenter wrapping an existing [`TerminalWriter`].
    pub fn new(writer: TerminalWriter<W>) -> Self {
        Self { writer }
    }

    /// Create a presenter writing to an arbitrary `Write` sink with default diff config.
    pub fn with_writer(
        writer: W,
        mode: ScreenMode,
        anchor: UiAnchor,
        capabilities: TerminalCapabilities,
    ) -> Self {
        Self {
            writer: TerminalWriter::new(writer, mode, anchor, capabilities),
        }
    }

    /// Create a presenter writing to an arbitrary `Write` sink with custom diff config.
    pub fn with_diff_config(
        writer: W,
        mode: ScreenMode,
        anchor: UiAnchor,
        capabilities: TerminalCapabilities,
        diff_config: RuntimeDiffConfig,
    ) -> Self {
        Self {
            writer: TerminalWriter::with_diff_config(
                writer,
                mode,
                anchor,
                capabilities,
                diff_config,
            ),
        }
    }

    /// Reference the inner terminal writer.
    #[must_use]
    pub const fn writer(&self) -> &TerminalWriter<W> {
        &self.writer
    }

    /// Mutably access the inner terminal writer.
    pub fn writer_mut(&mut self) -> &mut TerminalWriter<W> {
        &mut self.writer
    }

    /// Consume the presenter and return the inner terminal writer.
    pub fn into_writer(self) -> TerminalWriter<W> {
        self.writer
    }

    /// Consume the presenter and attempt to extract the underlying writer.
    pub fn into_inner(self) -> Option<W> {
        self.writer.into_inner()
    }

    /// Get the current UI height.
    #[must_use]
    pub fn ui_height(&self) -> u16 {
        self.writer.ui_height()
    }

    /// Set the terminal dimensions.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.writer.set_size(cols, rows);
    }
}

impl<W: Write + Send> From<TerminalWriter<W>> for TerminalPresenter<W> {
    fn from(writer: TerminalWriter<W>) -> Self {
        Self::new(writer)
    }
}

impl TerminalPresenter<Stdout> {
    /// Create a live presenter writing to stdout with default capabilities.
    #[must_use]
    pub fn live(capabilities: TerminalCapabilities) -> Self {
        Self::with_writer(
            io::stdout(),
            ScreenMode::default(),
            UiAnchor::default(),
            capabilities,
        )
    }
}

impl<W: Write + Send> BackendPresenter for TerminalPresenter<W> {
    type Error = io::Error;

    fn capabilities(&self) -> &TerminalCapabilities {
        self.writer.capabilities()
    }

    fn write_log(&mut self, text: &str) -> Result<(), Self::Error> {
        self.writer.write_log(text)
    }

    fn write_log_line_with_mode(
        &mut self,
        text: &str,
        mode: SanitizeMode,
    ) -> Result<(), Self::Error> {
        self.writer.write_log_line_with_mode(text, mode)
    }

    fn present_ui(
        &mut self,
        buf: &Buffer,
        _diff: Option<&BufferDiff>,
        full_repaint_hint: bool,
    ) -> Result<(), Self::Error> {
        // The writer's trailing arguments are the cursor and its visibility.
        // Passing `None, false` hid the cursor on every frame; the trait has
        // no cursor, so its visibility is left as it was. The repaint hint
        // was dropped, so a damaged screen was diffed against a stale
        // baseline and never rewritten.
        if full_repaint_hint {
            self.writer.invalidate_diff_baseline();
        }
        let cursor_visible = self.writer.cursor_visible();
        self.writer.present_ui(buf, None, cursor_visible)
    }

    fn present_ui_owned(
        &mut self,
        buf: Buffer,
        cursor: Option<(u16, u16)>,
        cursor_visible: bool,
    ) -> Result<(), Self::Error> {
        self.writer.present_ui_owned(buf, cursor, cursor_visible)
    }

    fn gc(&mut self) {
        self.writer.gc(None);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.writer.set_size(cols, rows);
    }

    fn screen_mode(&self) -> ScreenMode {
        self.writer.screen_mode()
    }

    fn set_screen_mode(&mut self, mode: ScreenMode) {
        self.writer.set_screen_mode(mode);
    }

    fn set_evidence_writer(&mut self, w: Option<Arc<dyn Fn(&str) + Send + Sync>>) {
        self.writer.set_evidence_callback(w);
    }

    fn last_diff_strategy(&self) -> Option<DiffStrategy> {
        self.writer.last_diff_strategy()
    }

    fn last_diff_elapsed(&self) -> Option<Duration> {
        self.writer
            .last_present_timings()
            .map(|t| Duration::from_micros(t.diff_us))
    }

    fn begin_shutdown(&mut self) -> Result<(), Self::Error> {
        self.writer.flush()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.writer.flush()
    }

    fn take_render_buffer(&mut self, width: u16, height: u16) -> Buffer {
        self.writer.take_render_buffer(width, height)
    }

    fn begin_link_frame(&mut self) {
        self.writer.begin_link_frame();
    }

    fn pool_and_links_mut(&mut self) -> (&mut GraphemePool, &mut LinkRegistry) {
        self.writer.pool_and_links_mut()
    }

    fn pool_mut(&mut self) -> &mut GraphemePool {
        self.writer.pool_mut()
    }

    fn inline_auto_bounds(&self) -> Option<(u16, u16)> {
        self.writer.inline_auto_bounds()
    }

    fn auto_ui_height(&self) -> Option<u16> {
        self.writer.auto_ui_height()
    }

    fn render_height_hint(&self) -> u16 {
        self.writer.render_height_hint()
    }

    fn set_auto_ui_height(&mut self, height: u16) {
        self.writer.set_auto_ui_height(height);
    }

    fn clear_auto_ui_height(&mut self) {
        self.writer.clear_auto_ui_height();
    }

    fn take_last_present_timings(&mut self) -> Option<PresentTimings> {
        self.writer.take_last_present_timings()
    }

    fn estimate_memory_usage(&self) -> usize {
        self.writer.estimate_memory_usage()
    }

    fn write_osc52_set(
        &mut self,
        selection: ClipboardSelection,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        self.writer.write_osc52_set(selection, data)
    }

    fn write_osc52_query(&mut self, selection: ClipboardSelection) -> Result<(), Self::Error> {
        self.writer.write_osc52_query(selection)
    }

    fn set_timing_enabled(&mut self, enabled: bool) {
        self.writer.set_timing_enabled(enabled);
    }

    fn set_hyperlink_limit(&mut self, limit: usize) {
        self.writer.set_hyperlink_limit(limit);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn resize_forwards_to_writer() {
        let caps = TerminalCapabilities::detect();
        let writer = TerminalWriter::new(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            caps,
        );
        let mut presenter = TerminalPresenter::new(writer);
        assert_eq!(presenter.writer.width(), 80);
        assert_eq!(presenter.writer.height(), 24);

        presenter.resize(120, 40);
        assert_eq!(presenter.writer.width(), 120);
        assert_eq!(presenter.writer.height(), 40);
    }

    #[test]
    fn present_ui_honors_the_repaint_hint_and_keeps_the_cursor() {
        let caps = TerminalCapabilities::basic();
        let writer = TerminalWriter::new(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            caps,
        );
        let mut presenter = TerminalPresenter::new(writer);
        presenter.resize(10, 5);
        let mut buf = Buffer::new(10, 5);
        buf.set(2, 1, ftui_render::cell::Cell::from_char('Ж'));

        presenter.present_ui(&buf, None, false).unwrap();
        // Unchanged: nothing to write.
        presenter.present_ui(&buf, None, false).unwrap();
        // The hint repaints every cell.
        presenter.present_ui(&buf, None, true).unwrap();

        let TerminalPresenter { writer, .. } = presenter;
        let bytes = writer.into_inner().expect("writer output");
        let output = String::from_utf8_lossy(&bytes);
        assert_eq!(output.matches('Ж').count(), 2, "{output:?}");
        // The cursor may be hidden while drawing, but ends up visible again.
        let hidden = output.rfind("\x1b[?25l");
        let shown = output.rfind("\x1b[?25h");
        assert!(shown > hidden, "cursor left hidden: {output:?}");
    }

    #[test]
    fn evidence_callback_receives_diff_decision() {
        let caps = TerminalCapabilities::detect();
        let writer = TerminalWriter::new(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            caps,
        );
        let mut presenter = TerminalPresenter::new(writer);
        presenter.resize(10, 5);

        let lines = Arc::new(Mutex::new(Vec::new()));
        let lines_cb = Arc::clone(&lines);
        presenter.set_evidence_writer(Some(Arc::new(move |line| {
            lines_cb.lock().unwrap().push(line.to_string());
        })));

        let mut buf1 = presenter.take_render_buffer(10, 5);
        buf1.set(0, 0, ftui_render::cell::Cell::from_char('A'));
        presenter.present_ui_owned(buf1, None, false).unwrap();

        let mut buf2 = presenter.take_render_buffer(10, 5);
        buf2.set(1, 1, ftui_render::cell::Cell::from_char('B'));
        presenter.present_ui_owned(buf2, None, false).unwrap();

        let captured = lines.lock().unwrap();
        assert!(
            !captured.is_empty(),
            "evidence callback should have received evidence lines, got: {captured:?}"
        );
        assert!(
            captured.iter().any(|l| l.contains("diff_decision")),
            "expected diff_decision event in evidence, got: {captured:?}"
        );
    }
}
