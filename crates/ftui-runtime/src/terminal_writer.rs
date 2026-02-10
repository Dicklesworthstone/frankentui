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
//! writer.present_ui(&buffer, None, true)?;
//! ```

use std::io::{self, BufWriter, Write};
use std::time::Instant;

use crate::evidence_sink::EvidenceSink;
use crate::evidence_telemetry::{DiffDecisionSnapshot, set_diff_snapshot};
use crate::render_trace::{
    RenderTraceFrame, RenderTraceRecorder, build_diff_runs_payload, build_full_buffer_payload,
};
use ftui_core::inline_mode::InlineStrategy;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::buffer::{Buffer, DirtySpanConfig, DirtySpanStats};
use ftui_render::diff::{BufferDiff, TileDiffConfig, TileDiffFallback, TileDiffStats};
use ftui_render::diff_strategy::{DiffStrategy, DiffStrategyConfig, DiffStrategySelector};
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

/// How often to probe with a real diff when FullRedraw is selected.
#[allow(dead_code)] // API for future diff strategy integration
const FULL_REDRAW_PROBE_INTERVAL: u64 = 60;

/// Writer wrapper that can count bytes written when enabled.
struct CountingWriter<W: Write> {
    inner: W,
    count_enabled: bool,
    bytes_written: u64,
}

impl<W: Write> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            count_enabled: false,
            bytes_written: 0,
        }
    }

    #[allow(dead_code)]
    fn enable_counting(&mut self) {
        self.count_enabled = true;
        self.bytes_written = 0;
    }

    #[allow(dead_code)]
    fn disable_counting(&mut self) {
        self.count_enabled = false;
    }

    #[allow(dead_code)]
    fn take_count(&mut self) -> u64 {
        let count = self.bytes_written;
        self.bytes_written = 0;
        count
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        if self.count_enabled {
            self.bytes_written = self.bytes_written.saturating_add(written as u64);
        }
        Ok(written)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)?;
        if self.count_enabled {
            self.bytes_written = self.bytes_written.saturating_add(buf.len() as u64);
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn default_diff_run_id() -> String {
    format!("diff-{}", std::process::id())
}

fn diff_strategy_str(strategy: DiffStrategy) -> &'static str {
    match strategy {
        DiffStrategy::Full => "full",
        DiffStrategy::DirtyRows => "dirty",
        DiffStrategy::FullRedraw => "redraw",
    }
}

fn ui_anchor_str(anchor: UiAnchor) -> &'static str {
    match anchor {
        UiAnchor::Bottom => "bottom",
        UiAnchor::Top => "top",
    }
}

#[allow(dead_code)]
#[inline]
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[allow(dead_code)]
fn estimate_diff_scan_cost(
    strategy: DiffStrategy,
    dirty_rows: usize,
    width: usize,
    height: usize,
    span_stats: &DirtySpanStats,
    tile_stats: Option<TileDiffStats>,
) -> (usize, &'static str) {
    match strategy {
        DiffStrategy::Full => (width.saturating_mul(height), "full_strategy"),
        DiffStrategy::FullRedraw => (0, "full_redraw"),
        DiffStrategy::DirtyRows => {
            if dirty_rows == 0 {
                return (0, "no_dirty_rows");
            }
            if let Some(tile_stats) = tile_stats
                && tile_stats.fallback.is_none()
            {
                return (tile_stats.scan_cells_estimate, "tile_skip");
            }
            let span_cells = span_stats.span_coverage_cells;
            if span_stats.overflows > 0 {
                let estimate = if span_cells > 0 {
                    span_cells
                } else {
                    dirty_rows.saturating_mul(width)
                };
                return (estimate, "span_overflow");
            }
            if span_cells > 0 {
                (span_cells, "none")
            } else {
                (dirty_rows.saturating_mul(width), "no_spans")
            }
        }
    }
}

fn sanitize_auto_bounds(min_height: u16, max_height: u16) -> (u16, u16) {
    let min = min_height.max(1);
    let max = max_height.max(min);
    (min, max)
}

/// Screen mode determines whether we use alternate screen or inline mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenMode {
    /// Inline mode preserves scrollback. UI is anchored at bottom/top.
    Inline {
        /// Height of the UI region in rows.
        ui_height: u16,
    },
    /// Inline mode with automatic UI height based on rendered content.
    ///
    /// The measured height is clamped between `min_height` and `max_height`.
    InlineAuto {
        /// Minimum UI height in rows.
        min_height: u16,
        /// Maximum UI height in rows.
        max_height: u16,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InlineRegion {
    start: u16,
    height: u16,
}

struct DiffDecision {
    #[allow(dead_code)] // reserved for future diff strategy introspection
    strategy: DiffStrategy,
    has_diff: bool,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct EmitStats {
    diff_cells: usize,
    diff_runs: usize,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct FrameEmitStats {
    diff_strategy: DiffStrategy,
    diff_cells: usize,
    diff_runs: usize,
    ui_height: u16,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct PresentTimings {
    pub diff_us: u64,
}

// =============================================================================
// Runtime Diff Configuration
// =============================================================================

/// Runtime-level configuration for diff strategy selection.
///
/// This wraps [`DiffStrategyConfig`] and adds runtime-specific toggles
/// for enabling/disabling features and controlling reset policies.
///
/// # Example
///
/// ```
/// use ftui_runtime::{RuntimeDiffConfig, DiffStrategyConfig};
///
/// // Use defaults (Bayesian selection enabled, dirty-rows enabled)
/// let config = RuntimeDiffConfig::default();
///
/// // Disable Bayesian selection (always use dirty-rows if available)
/// let config = RuntimeDiffConfig::default()
///     .with_bayesian_enabled(false);
///
/// // Custom cost model
/// let config = RuntimeDiffConfig::default()
///     .with_strategy_config(DiffStrategyConfig {
///         c_emit: 10.0,  // Higher I/O cost
///         ..Default::default()
///     });
/// ```
#[derive(Debug, Clone)]
pub struct RuntimeDiffConfig {
    /// Enable Bayesian strategy selection.
    ///
    /// When enabled, the selector uses a Beta posterior over the change rate
    /// to choose between Full, DirtyRows, and FullRedraw strategies.
    ///
    /// When disabled, always uses DirtyRows if dirty tracking is available,
    /// otherwise Full.
    ///
    /// Default: true
    pub bayesian_enabled: bool,

    /// Enable dirty-row optimization.
    ///
    /// When enabled, the DirtyRows strategy is available for selection.
    /// When disabled, the selector chooses between Full and FullRedraw only.
    ///
    /// Default: true
    pub dirty_rows_enabled: bool,

    /// Dirty-span tracking configuration (thresholds + feature flags).
    ///
    /// Controls span merging, guard bands, and enable/disable behavior.
    pub dirty_span_config: DirtySpanConfig,

    /// Tile-based diff skipping configuration (thresholds + feature flags).
    ///
    /// Controls SAT tile size, thresholds, and enable/disable behavior.
    pub tile_diff_config: TileDiffConfig,

    /// Reset posterior on dimension change.
    ///
    /// When true, the Bayesian posterior resets to priors when the buffer
    /// dimensions change (e.g., terminal resize).
    ///
    /// Default: true
    pub reset_on_resize: bool,

    /// Reset posterior on buffer invalidation.
    ///
    /// When true, resets to priors when the previous buffer becomes invalid
    /// (e.g., mode switch, scroll region change).
    ///
    /// Default: true
    pub reset_on_invalidation: bool,

    /// Underlying strategy configuration.
    ///
    /// Contains cost model constants, prior parameters, and decay settings.
    pub strategy_config: DiffStrategyConfig,
}

impl Default for RuntimeDiffConfig {
    fn default() -> Self {
        Self {
            bayesian_enabled: true,
            dirty_rows_enabled: true,
            dirty_span_config: DirtySpanConfig::default(),
            tile_diff_config: TileDiffConfig::default(),
            reset_on_resize: true,
            reset_on_invalidation: true,
            strategy_config: DiffStrategyConfig::default(),
        }
    }
}

impl RuntimeDiffConfig {
    /// Create a new config with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether Bayesian strategy selection is enabled.
    pub fn with_bayesian_enabled(mut self, enabled: bool) -> Self {
        self.bayesian_enabled = enabled;
        self
    }

    /// Set whether dirty-row optimization is enabled.
    pub fn with_dirty_rows_enabled(mut self, enabled: bool) -> Self {
        self.dirty_rows_enabled = enabled;
        self
    }

    /// Set whether dirty-span tracking is enabled.
    pub fn with_dirty_spans_enabled(mut self, enabled: bool) -> Self {
        self.dirty_span_config = self.dirty_span_config.with_enabled(enabled);
        self
    }

    /// Set the dirty-span tracking configuration.
    pub fn with_dirty_span_config(mut self, config: DirtySpanConfig) -> Self {
        self.dirty_span_config = config;
        self
    }

    /// Toggle tile-based skipping.
    pub fn with_tile_skip_enabled(mut self, enabled: bool) -> Self {
        self.tile_diff_config = self.tile_diff_config.with_enabled(enabled);
        self
    }

    /// Set the tile-based diff configuration.
    pub fn with_tile_diff_config(mut self, config: TileDiffConfig) -> Self {
        self.tile_diff_config = config;
        self
    }

    /// Set whether to reset posterior on resize.
    pub fn with_reset_on_resize(mut self, enabled: bool) -> Self {
        self.reset_on_resize = enabled;
        self
    }

    /// Set whether to reset posterior on invalidation.
    pub fn with_reset_on_invalidation(mut self, enabled: bool) -> Self {
        self.reset_on_invalidation = enabled;
        self
    }

    /// Set the underlying strategy configuration.
    pub fn with_strategy_config(mut self, config: DiffStrategyConfig) -> Self {
        self.strategy_config = config;
        self
    }
}

/// Unified terminal output coordinator.
///
/// Enforces the one-writer rule and implements inline mode correctly.
/// All terminal output should go through this struct.
pub struct TerminalWriter<W: Write> {
    /// Buffered writer for efficient output. Option allows moving out for into_inner().
    writer: Option<CountingWriter<BufWriter<W>>>,
    /// Current screen mode.
    screen_mode: ScreenMode,
    /// Last computed auto UI height (inline auto mode only).
    auto_ui_height: Option<u16>,
    /// Where UI is anchored in inline mode.
    ui_anchor: UiAnchor,
    /// Previous buffer for diffing.
    prev_buffer: Option<Buffer>,
    /// Spare buffer for reuse as the next render target.
    spare_buffer: Option<Buffer>,
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
    /// Current cursor visibility state (best-effort).
    cursor_visible: bool,
    /// Inline mode rendering strategy (selected from capabilities).
    inline_strategy: InlineStrategy,
    /// Whether a scroll region is currently active.
    scroll_region_active: bool,
    /// Last inline UI region for clearing on shrink.
    last_inline_region: Option<InlineRegion>,
    /// Bayesian diff strategy selector.
    diff_strategy: DiffStrategySelector,
    /// Reusable diff buffer to avoid per-frame allocations.
    diff_scratch: BufferDiff,
    /// Frames since last diff probe while in FullRedraw.
    full_redraw_probe: u64,
    /// Runtime diff configuration.
    #[allow(dead_code)] // runtime toggles wired up in follow-up work
    diff_config: RuntimeDiffConfig,
    /// Evidence JSONL sink for diff decisions.
    evidence_sink: Option<EvidenceSink>,
    /// Run identifier for diff decision evidence.
    #[allow(dead_code)]
    diff_evidence_run_id: String,
    /// Monotonic event index for diff decision evidence.
    #[allow(dead_code)]
    diff_evidence_idx: u64,
    /// Last diff strategy selected during present.
    last_diff_strategy: Option<DiffStrategy>,
    /// Render-trace recorder (optional).
    render_trace: Option<RenderTraceRecorder>,
    /// Whether per-frame timing capture is enabled.
    timing_enabled: bool,
    /// Last present timings (diff compute duration).
    last_present_timings: Option<PresentTimings>,
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
        Self::with_diff_config(
            writer,
            screen_mode,
            ui_anchor,
            capabilities,
            RuntimeDiffConfig::default(),
        )
    }

    /// Create a new terminal writer with custom diff strategy configuration.
    ///
    /// # Arguments
    ///
    /// * `writer` - Output destination (takes ownership for one-writer rule)
    /// * `screen_mode` - Inline or alternate screen mode
    /// * `ui_anchor` - Where to anchor UI in inline mode
    /// * `capabilities` - Terminal capabilities
    /// * `diff_config` - Configuration for diff strategy selection
    ///
    /// # Example
    ///
    /// ```ignore
    /// use ftui_runtime::{TerminalWriter, ScreenMode, UiAnchor, RuntimeDiffConfig};
    /// use ftui_core::terminal_capabilities::TerminalCapabilities;
    ///
    /// // Disable Bayesian selection for deterministic diffing
    /// let config = RuntimeDiffConfig::default()
    ///     .with_bayesian_enabled(false);
    ///
    /// let writer = TerminalWriter::with_diff_config(
    ///     std::io::stdout(),
    ///     ScreenMode::AltScreen,
    ///     UiAnchor::Bottom,
    ///     TerminalCapabilities::detect(),
    ///     config,
    /// );
    /// ```
    pub fn with_diff_config(
        writer: W,
        screen_mode: ScreenMode,
        ui_anchor: UiAnchor,
        capabilities: TerminalCapabilities,
        diff_config: RuntimeDiffConfig,
    ) -> Self {
        let inline_strategy = InlineStrategy::select(&capabilities);
        let auto_ui_height = None;
        let diff_strategy = DiffStrategySelector::new(diff_config.strategy_config.clone());
        let mut diff_scratch = BufferDiff::new();
        diff_scratch
            .tile_config_mut()
            .clone_from(&diff_config.tile_diff_config);
        Self {
            writer: Some(CountingWriter::new(BufWriter::with_capacity(
                BUFFER_CAPACITY,
                writer,
            ))),
            screen_mode,
            auto_ui_height,
            ui_anchor,
            prev_buffer: None,
            spare_buffer: None,
            pool: GraphemePool::new(),
            links: LinkRegistry::new(),
            capabilities,
            term_width: 80,
            term_height: 24,
            in_sync_block: false,
            cursor_saved: false,
            cursor_visible: true,
            inline_strategy,
            scroll_region_active: false,
            last_inline_region: None,
            diff_strategy,
            diff_scratch,
            full_redraw_probe: 0,
            diff_config,
            evidence_sink: None,
            diff_evidence_run_id: default_diff_run_id(),
            diff_evidence_idx: 0,
            last_diff_strategy: None,
            render_trace: None,
            timing_enabled: false,
            last_present_timings: None,
        }
    }

    /// Get a mutable reference to the internal writer.
    ///
    /// # Panics
    ///
    /// Panics if the writer has been taken (via `into_inner`).
    #[inline]
    fn writer(&mut self) -> &mut CountingWriter<BufWriter<W>> {
        self.writer.as_mut().expect("writer has been consumed")
    }

    /// Reset diff strategy state when the previous buffer is invalidated.
    fn reset_diff_strategy(&mut self) {
        if self.diff_config.reset_on_invalidation {
            self.diff_strategy.reset();
        }
        self.full_redraw_probe = 0;
        self.last_diff_strategy = None;
    }

    /// Reset diff strategy state on terminal resize.
    #[allow(dead_code)] // used by upcoming resize-aware diff strategy work
    fn reset_diff_on_resize(&mut self) {
        if self.diff_config.reset_on_resize {
            self.diff_strategy.reset();
        }
        self.full_redraw_probe = 0;
        self.last_diff_strategy = None;
    }

    /// Get the current diff configuration.
    pub fn diff_config(&self) -> &RuntimeDiffConfig {
        &self.diff_config
    }

    /// Enable or disable per-frame timing capture.
    pub(crate) fn set_timing_enabled(&mut self, enabled: bool) {
        self.timing_enabled = enabled;
        if !enabled {
            self.last_present_timings = None;
        }
    }

    /// Take the last present timings (if available).
    pub(crate) fn take_last_present_timings(&mut self) -> Option<PresentTimings> {
        self.last_present_timings.take()
    }

    /// Attach an evidence sink for diff decision logging.
    #[must_use]
    pub fn with_evidence_sink(mut self, sink: EvidenceSink) -> Self {
        self.evidence_sink = Some(sink);
        self
    }

    /// Set the evidence JSONL sink for diff decision logging.
    pub fn set_evidence_sink(&mut self, sink: Option<EvidenceSink>) {
        self.evidence_sink = sink;
    }

    /// Attach a render-trace recorder.
    #[must_use]
    pub fn with_render_trace(mut self, recorder: RenderTraceRecorder) -> Self {
        self.render_trace = Some(recorder);
        self
    }

    /// Set the render-trace recorder.
    pub fn set_render_trace(&mut self, recorder: Option<RenderTraceRecorder>) {
        self.render_trace = recorder;
    }

    /// Get mutable access to the diff strategy selector.
    ///
    /// Useful for advanced scenarios like manual posterior updates.
    pub fn diff_strategy_mut(&mut self) -> &mut DiffStrategySelector {
        &mut self.diff_strategy
    }

    /// Get the diff strategy selector (read-only).
    pub fn diff_strategy(&self) -> &DiffStrategySelector {
        &self.diff_strategy
    }

    /// Get the last diff strategy selected during present, if any.
    pub fn last_diff_strategy(&self) -> Option<DiffStrategy> {
        self.last_diff_strategy
    }

    /// Set the terminal size.
    ///
    /// Call this when the terminal is resized.
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.term_width = width;
        self.term_height = height;
        if matches!(self.screen_mode, ScreenMode::InlineAuto { .. }) {
            self.auto_ui_height = None;
        }
        // Clear prev_buffer to force full redraw after resize
        self.prev_buffer = None;
        self.spare_buffer = None;
        self.reset_diff_on_resize();
        // Reset scroll region on resize; it will be re-established on next present
        if self.scroll_region_active {
            let _ = self.deactivate_scroll_region();
        }
    }

    /// Take a reusable render buffer sized for the current frame.
    ///
    /// Uses a spare buffer when available to avoid per-frame allocation.
    pub fn take_render_buffer(&mut self, width: u16, height: u16) -> Buffer {
        if let Some(mut buffer) = self.spare_buffer.take()
            && buffer.width() == width
            && buffer.height() == height
        {
            buffer.set_dirty_span_config(self.diff_config.dirty_span_config);
            buffer.reset_for_frame();
            return buffer;
        }

        let mut buffer = Buffer::new(width, height);
        buffer.set_dirty_span_config(self.diff_config.dirty_span_config);
        buffer
    }

    /// Get the current terminal width.
    #[inline]
    pub fn width(&self) -> u16 {
        self.term_width
    }

    /// Get the current terminal height.
    #[inline]
    pub fn height(&self) -> u16 {
        self.term_height
    }

    /// Get the current screen mode.
    #[inline]
    pub fn screen_mode(&self) -> ScreenMode {
        self.screen_mode
    }

    /// Height to use for rendering a frame.
    ///
    /// In inline auto mode, this returns the configured maximum (clamped to
    /// terminal height) so measurement can determine actual UI height.
    pub fn render_height_hint(&self) -> u16 {
        match self.screen_mode {
            ScreenMode::Inline { ui_height } => ui_height,
            ScreenMode::InlineAuto {
                min_height,
                max_height,
            } => {
                let (min, max) = sanitize_auto_bounds(min_height, max_height);
                let max = max.min(self.term_height);
                let min = min.min(max);
                if let Some(current) = self.auto_ui_height {
                    current.clamp(min, max).min(self.term_height).max(min)
                } else {
                    max.max(min)
                }
            }
            ScreenMode::AltScreen => self.term_height,
        }
    }

    /// Get sanitized min/max bounds for inline auto mode (clamped to terminal height).
    pub fn inline_auto_bounds(&self) -> Option<(u16, u16)> {
        match self.screen_mode {
            ScreenMode::InlineAuto {
                min_height,
                max_height,
            } => {
                let (min, max) = sanitize_auto_bounds(min_height, max_height);
                Some((min.min(self.term_height), max.min(self.term_height)))
            }
            _ => None,
        }
    }

    /// Get the cached auto UI height (inline auto mode only).
    pub fn auto_ui_height(&self) -> Option<u16> {
        match self.screen_mode {
            ScreenMode::InlineAuto { .. } => self.auto_ui_height,
            _ => None,
        }
    }

    /// Update the computed height for inline auto mode.
    pub fn set_auto_ui_height(&mut self, height: u16) {
        if let ScreenMode::InlineAuto {
            min_height,
            max_height,
        } = self.screen_mode
        {
            let (min, max) = sanitize_auto_bounds(min_height, max_height);
            let max = max.min(self.term_height);
            let min = min.min(max);
            let clamped = height.clamp(min, max);
            let previous_effective = self.auto_ui_height.unwrap_or(min);
            if self.auto_ui_height != Some(clamped) {
                self.auto_ui_height = Some(clamped);
                if clamped != previous_effective {
                    self.prev_buffer = None;
                    self.reset_diff_strategy();
                    if self.scroll_region_active {
                        let _ = self.deactivate_scroll_region();
                    }
                }
            }
        }
    }

    /// Clear the cached auto UI height (inline auto mode only).
    pub fn clear_auto_ui_height(&mut self) {
        if matches!(self.screen_mode, ScreenMode::InlineAuto { .. })
            && self.auto_ui_height.is_some()
        {
            self.auto_ui_height = None;
            self.prev_buffer = None;
            self.reset_diff_strategy();
            if self.scroll_region_active {
                let _ = self.deactivate_scroll_region();
            }
        }
    }

    fn effective_ui_height(&self) -> u16 {
        match self.screen_mode {
            ScreenMode::Inline { ui_height } => ui_height,
            ScreenMode::InlineAuto {
                min_height,
                max_height,
            } => {
                let (min, max) = sanitize_auto_bounds(min_height, max_height);
                let current = self.auto_ui_height.unwrap_or(min);
                current.clamp(min, max).min(self.term_height)
            }
            ScreenMode::AltScreen => self.term_height,
        }
    }

    /// Get the UI height for the current mode.
    pub fn ui_height(&self) -> u16 {
        self.effective_ui_height()
    }

    /// Calculate the row where the UI starts (0-indexed).
    fn ui_start_row(&self) -> u16 {
        let ui_height = self.effective_ui_height().min(self.term_height);
        match (self.screen_mode, self.ui_anchor) {
            (ScreenMode::Inline { .. }, UiAnchor::Bottom)
            | (ScreenMode::InlineAuto { .. }, UiAnchor::Bottom) => {
                self.term_height.saturating_sub(ui_height)
            }
            (ScreenMode::Inline { .. }, UiAnchor::Top)
            | (ScreenMode::InlineAuto { .. }, UiAnchor::Top) => 0,
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
    /// Sets DECSTBM to constrain scrolling to the log region:
    /// - Bottom-anchored UI: log region is above the UI.
    /// - Top-anchored UI: log region is below the UI.
    ///
    /// Only called when the strategy permits scroll-region usage.
    fn activate_scroll_region(&mut self, ui_height: u16) -> io::Result<()> {
        if self.scroll_region_active {
            return Ok(());
        }

        let ui_height = ui_height.min(self.term_height);
        if ui_height >= self.term_height {
            return Ok(());
        }

        match self.ui_anchor {
            UiAnchor::Bottom => {
                let term_height = self.term_height;
                let log_bottom = term_height.saturating_sub(ui_height);
                if log_bottom > 0 {
                    // DECSTBM: set scroll region to rows 1..log_bottom (1-indexed)
                    write!(self.writer(), "\x1b[1;{}r", log_bottom)?;
                    self.scroll_region_active = true;
                }
            }
            UiAnchor::Top => {
                let term_height = self.term_height;
                let log_top = ui_height.saturating_add(1);
                if log_top <= term_height {
                    // DECSTBM: set scroll region to rows log_top..term_height (1-indexed)
                    write!(self.writer(), "\x1b[{};{}r", log_top, term_height)?;
                    self.scroll_region_active = true;
                    // DECSTBM moves cursor to home; for top-anchored UI we move it
                    // into the log region so any subsequent output stays below UI.
                    write!(self.writer(), "\x1b[{};1H", log_top)?;
                }
            }
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

    fn clear_rows(&mut self, start_row: u16, height: u16) -> io::Result<()> {
        let start_row = start_row.min(self.term_height);
        let end_row = start_row.saturating_add(height).min(self.term_height);
        for row in start_row..end_row {
            write!(self.writer(), "\x1b[{};1H", row.saturating_add(1))?;
            self.writer().write_all(ERASE_LINE)?;
        }
        Ok(())
    }

    fn clear_inline_region_diff(&mut self, current: InlineRegion) -> io::Result<()> {
        let Some(previous) = self.last_inline_region else {
            return Ok(());
        };

        let prev_start = previous.start.min(self.term_height);
        let prev_end = previous
            .start
            .saturating_add(previous.height)
            .min(self.term_height);
        if prev_start >= prev_end {
            return Ok(());
        }

        let curr_start = current.start.min(self.term_height);
        let curr_end = current
            .start
            .saturating_add(current.height)
            .min(self.term_height);

        if curr_start > prev_start {
            let clear_end = curr_start.min(prev_end);
            if clear_end > prev_start {
                self.clear_rows(prev_start, clear_end - prev_start)?;
            }
        }

        if curr_end < prev_end {
            let clear_start = curr_end.max(prev_start);
            if prev_end > clear_start {
                self.clear_rows(clear_start, prev_end - clear_start)?;
            }
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
    /// 6. Moves cursor to requested UI position (if any)
    /// 7. Applies cursor visibility
    /// 8. Ends synchronized output
    ///
    /// In AltScreen mode, this just renders the buffer and positions cursor.
    pub fn present_ui(
        &mut self,
        buffer: &Buffer,
        cursor: Option<(u16, u16)>,
        cursor_visible: bool,
    ) -> io::Result<()> {
        let mode_str = match self.screen_mode {
            ScreenMode::Inline { .. } => "inline",
            ScreenMode::InlineAuto { .. } => "inline_auto",
            ScreenMode::AltScreen => "altscreen",
        };
        let trace_enabled = self.render_trace.is_some();
        if trace_enabled {
            self.writer().enable_counting();
        }
        let present_start = if trace_enabled {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let _span = info_span!(
            "ftui.render.present",
            mode = mode_str,
            width = buffer.width(),
            height = buffer.height(),
        )
        .entered();

        let result = match self.screen_mode {
            ScreenMode::Inline { ui_height } => {
                self.present_inline(buffer, ui_height, cursor, cursor_visible)
            }
            ScreenMode::InlineAuto { .. } => {
                let ui_height = self.effective_ui_height();
                self.present_inline(buffer, ui_height, cursor, cursor_visible)
            }
            ScreenMode::AltScreen => self.present_altscreen(buffer, cursor, cursor_visible),
        };

        let present_us = present_start.map(|start| start.elapsed().as_micros() as u64);
        let present_bytes = if trace_enabled {
            Some(self.writer().take_count())
        } else {
            None
        };
        if trace_enabled {
            self.writer().disable_counting();
        }

        if let Ok(stats) = result {
            self.spare_buffer = self.prev_buffer.take();
            self.prev_buffer = Some(buffer.clone());

            if let Some(ref mut trace) = self.render_trace {
                let payload_info = match stats.diff_strategy {
                    DiffStrategy::FullRedraw => {
                        let payload = build_full_buffer_payload(buffer, &self.pool);
                        trace.write_payload(&payload).ok()
                    }
                    _ => {
                        let payload =
                            build_diff_runs_payload(buffer, &self.diff_scratch, &self.pool);
                        trace.write_payload(&payload).ok()
                    }
                };
                let (payload_kind, payload_path) = match payload_info {
                    Some(info) => (info.kind, Some(info.path)),
                    None => ("none", None),
                };
                let payload_path_ref = payload_path.as_deref();
                let diff_strategy = diff_strategy_str(stats.diff_strategy);
                let ui_anchor = ui_anchor_str(self.ui_anchor);
                let frame = RenderTraceFrame {
                    cols: buffer.width(),
                    rows: buffer.height(),
                    mode: mode_str,
                    ui_height: stats.ui_height,
                    ui_anchor,
                    diff_strategy,
                    diff_cells: stats.diff_cells,
                    diff_runs: stats.diff_runs,
                    present_bytes: present_bytes.unwrap_or(0),
                    render_us: None,
                    present_us,
                    payload_kind,
                    payload_path: payload_path_ref,
                    trace_us: None,
                };
                let _ = trace.record_frame(frame, buffer, &self.pool);
            }
            return Ok(());
        }

        result.map(|_| ())
    }

    /// Present a UI frame, taking ownership of the buffer (O(1) — no clone).
    ///
    /// Prefer this over [`present_ui`] when the caller has an owned buffer
    /// that won't be reused, as it avoids an O(width × height) clone.
    pub fn present_ui_owned(
        &mut self,
        buffer: Buffer,
        cursor: Option<(u16, u16)>,
        cursor_visible: bool,
    ) -> io::Result<()> {
        let mode_str = match self.screen_mode {
            ScreenMode::Inline { .. } => "inline",
            ScreenMode::InlineAuto { .. } => "inline_auto",
            ScreenMode::AltScreen => "altscreen",
        };
        let trace_enabled = self.render_trace.is_some();
        if trace_enabled {
            self.writer().enable_counting();
        }
        let present_start = if trace_enabled {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let _span = info_span!(
            "ftui.render.present",
            mode = mode_str,
            width = buffer.width(),
            height = buffer.height(),
        )
        .entered();

        let result = match self.screen_mode {
            ScreenMode::Inline { ui_height } => {
                self.present_inline(&buffer, ui_height, cursor, cursor_visible)
            }
            ScreenMode::InlineAuto { .. } => {
                let ui_height = self.effective_ui_height();
                self.present_inline(&buffer, ui_height, cursor, cursor_visible)
            }
            ScreenMode::AltScreen => self.present_altscreen(&buffer, cursor, cursor_visible),
        };

        let present_us = present_start.map(|start| start.elapsed().as_micros() as u64);
        let present_bytes = if trace_enabled {
            Some(self.writer().take_count())
        } else {
            None
        };
        if trace_enabled {
            self.writer().disable_counting();
        }

        if let Ok(stats) = result {
            if let Some(ref mut trace) = self.render_trace {
                let payload_info = match stats.diff_strategy {
                    DiffStrategy::FullRedraw => {
                        let payload = build_full_buffer_payload(&buffer, &self.pool);
                        trace.write_payload(&payload).ok()
                    }
                    _ => {
                        let payload =
                            build_diff_runs_payload(&buffer, &self.diff_scratch, &self.pool);
                        trace.write_payload(&payload).ok()
                    }
                };
                let (payload_kind, payload_path) = match payload_info {
                    Some(info) => (info.kind, Some(info.path)),
                    None => ("none", None),
                };
                let payload_path_ref = payload_path.as_deref();
                let diff_strategy = diff_strategy_str(stats.diff_strategy);
                let ui_anchor = ui_anchor_str(self.ui_anchor);
                let frame = RenderTraceFrame {
                    cols: buffer.width(),
                    rows: buffer.height(),
                    mode: mode_str,
                    ui_height: stats.ui_height,
                    ui_anchor,
                    diff_strategy,
                    diff_cells: stats.diff_cells,
                    diff_runs: stats.diff_runs,
                    present_bytes: present_bytes.unwrap_or(0),
                    render_us: None,
                    present_us,
                    payload_kind,
                    payload_path: payload_path_ref,
                    trace_us: None,
                };
                let _ = trace.record_frame(frame, &buffer, &self.pool);
            }

            self.spare_buffer = self.prev_buffer.take();
            self.prev_buffer = Some(buffer);
            return Ok(());
        }

        result.map(|_| ())
    }

    fn decide_diff(&mut self, buffer: &Buffer) -> DiffDecision {
        let prev_dims = self
            .prev_buffer
            .as_ref()
            .map(|prev| (prev.width(), prev.height()));
        if prev_dims.is_none() || prev_dims != Some((buffer.width(), buffer.height())) {
            self.full_redraw_probe = 0;
            self.last_diff_strategy = Some(DiffStrategy::FullRedraw);
            return DiffDecision {
                strategy: DiffStrategy::FullRedraw,
                has_diff: false,
            };
        }

        let dirty_rows = buffer.dirty_row_count();
        let width = buffer.width() as usize;
        let height = buffer.height() as usize;
        let mut span_stats_snapshot: Option<DirtySpanStats> = None;
        let mut dirty_scan_cells_estimate = dirty_rows.saturating_mul(width);

        if self.diff_config.bayesian_enabled {
            let span_stats = buffer.dirty_span_stats();
            if span_stats.span_coverage_cells > 0 {
                dirty_scan_cells_estimate = span_stats.span_coverage_cells;
            }
            span_stats_snapshot = Some(span_stats);
        }

        // Select strategy based on config
        let mut strategy = if self.diff_config.bayesian_enabled {
            // Use Bayesian selector
            self.diff_strategy.select_with_scan_estimate(
                buffer.width(),
                buffer.height(),
                dirty_rows,
                dirty_scan_cells_estimate,
            )
        } else {
            // Simple heuristic: use DirtyRows if few rows dirty, else Full
            if self.diff_config.dirty_rows_enabled && dirty_rows < buffer.height() as usize {
                DiffStrategy::DirtyRows
            } else {
                DiffStrategy::Full
            }
        };

        // Enforce dirty_rows_enabled toggle
        if !self.diff_config.dirty_rows_enabled && strategy == DiffStrategy::DirtyRows {
            strategy = DiffStrategy::Full;
            if self.diff_config.bayesian_enabled {
                self.diff_strategy
                    .override_last_strategy(strategy, "dirty_rows_disabled");
            }
        }

        // Periodic probe when FullRedraw is selected (to update posterior)
        if strategy == DiffStrategy::FullRedraw {
            if self.full_redraw_probe >= FULL_REDRAW_PROBE_INTERVAL {
                self.full_redraw_probe = 0;
                let probed = if self.diff_config.dirty_rows_enabled
                    && dirty_rows < buffer.height() as usize
                {
                    DiffStrategy::DirtyRows
                } else {
                    DiffStrategy::Full
                };
                if probed != strategy {
                    strategy = probed;
                    if self.diff_config.bayesian_enabled {
                        self.diff_strategy
                            .override_last_strategy(strategy, "full_redraw_probe");
                    }
                }
            } else {
                self.full_redraw_probe = self.full_redraw_probe.saturating_add(1);
            }
        } else {
            self.full_redraw_probe = 0;
        }

        let mut has_diff = false;
        match strategy {
            DiffStrategy::Full => {
                let prev = self.prev_buffer.as_ref().expect("prev buffer must exist");
                self.diff_scratch.compute_into(prev, buffer);
                has_diff = true;
            }
            DiffStrategy::DirtyRows => {
                let prev = self.prev_buffer.as_ref().expect("prev buffer must exist");
                self.diff_scratch.compute_dirty_into(prev, buffer);
                has_diff = true;
            }
            DiffStrategy::FullRedraw => {}
        }

        let mut scan_cost_estimate = 0usize;
        let mut fallback_reason: &'static str = "none";
        let tile_stats = if strategy == DiffStrategy::DirtyRows {
            self.diff_scratch.last_tile_stats()
        } else {
            None
        };

        // Update posterior if Bayesian mode is enabled
        if self.diff_config.bayesian_enabled && has_diff {
            let span_stats = span_stats_snapshot.unwrap_or_else(|| buffer.dirty_span_stats());
            let (scan_cost, reason) = estimate_diff_scan_cost(
                strategy,
                dirty_rows,
                width,
                height,
                &span_stats,
                tile_stats,
            );
            let scanned_cells = scan_cost.max(self.diff_scratch.len());
            self.diff_strategy
                .observe(scanned_cells, self.diff_scratch.len());
            span_stats_snapshot = Some(span_stats);
            scan_cost_estimate = scan_cost;
            fallback_reason = reason;
        }

        if let Some(evidence) = self.diff_strategy.last_evidence() {
            let span_stats = span_stats_snapshot.unwrap_or_else(|| buffer.dirty_span_stats());
            let (scan_cost, reason) = if span_stats_snapshot.is_some() {
                (scan_cost_estimate, fallback_reason)
            } else {
                estimate_diff_scan_cost(
                    strategy,
                    dirty_rows,
                    width,
                    height,
                    &span_stats,
                    tile_stats,
                )
            };
            let span_coverage_pct = if evidence.total_cells == 0 {
                0.0
            } else {
                (span_stats.span_coverage_cells as f64 / evidence.total_cells as f64) * 100.0
            };
            let span_count = span_stats.total_spans;
            let max_span_len = span_stats.max_span_len;
            let event_idx = self.diff_evidence_idx;
            self.diff_evidence_idx = self.diff_evidence_idx.saturating_add(1);
            let tile_used = tile_stats.is_some_and(|stats| stats.fallback.is_none());
            let tile_fallback = tile_stats
                .and_then(|stats| stats.fallback)
                .map(TileDiffFallback::as_str)
                .unwrap_or("none");
            let run_id = json_escape(&self.diff_evidence_run_id);
            let strategy_json = json_escape(&strategy.to_string());
            let guard_reason_json = json_escape(evidence.guard_reason);
            let fallback_reason_json = json_escape(reason);
            let tile_fallback_json = json_escape(tile_fallback);
            let schema_version = crate::evidence_sink::EVIDENCE_SCHEMA_VERSION;
            let screen_mode = match self.screen_mode {
                ScreenMode::Inline { .. } => "inline",
                ScreenMode::InlineAuto { .. } => "inline_auto",
                ScreenMode::AltScreen => "altscreen",
            };
            let (
                tile_w,
                tile_h,
                tiles_x,
                tiles_y,
                dirty_tiles,
                dirty_cells,
                dirty_tile_ratio,
                dirty_cell_ratio,
                scanned_tiles,
                skipped_tiles,
                scan_cells_estimate,
                sat_build_cells,
            ) = if let Some(stats) = tile_stats {
                (
                    stats.tile_w,
                    stats.tile_h,
                    stats.tiles_x,
                    stats.tiles_y,
                    stats.dirty_tiles,
                    stats.dirty_cells,
                    stats.dirty_tile_ratio,
                    stats.dirty_cell_ratio,
                    stats.scanned_tiles,
                    stats.skipped_tiles,
                    stats.scan_cells_estimate,
                    stats.sat_build_cells,
                )
            } else {
                (0, 0, 0, 0, 0, 0, 0.0, 0.0, 0, 0, 0, 0)
            };
            let tile_size = tile_w as usize * tile_h as usize;
            let dirty_tile_count = dirty_tiles;
            let skipped_tile_count = skipped_tiles;
            let sat_build_cost_est = sat_build_cells;

            set_diff_snapshot(Some(DiffDecisionSnapshot {
                event_idx,
                screen_mode: screen_mode.to_string(),
                cols: u16::try_from(width).unwrap_or(u16::MAX),
                rows: u16::try_from(height).unwrap_or(u16::MAX),
                evidence: evidence.clone(),
                span_count,
                span_coverage_pct,
                max_span_len,
                scan_cost_estimate: scan_cost,
                fallback_reason: reason.to_string(),
                tile_used,
                tile_fallback: tile_fallback.to_string(),
                strategy_used: strategy,
            }));

            trace!(
                strategy = %strategy,
                selected = %evidence.strategy,
                cost_full = evidence.cost_full,
                cost_dirty = evidence.cost_dirty,
                cost_redraw = evidence.cost_redraw,
                dirty_rows = evidence.dirty_rows,
                total_rows = evidence.total_rows,
                total_cells = evidence.total_cells,
                bayesian_enabled = self.diff_config.bayesian_enabled,
                dirty_rows_enabled = self.diff_config.dirty_rows_enabled,
                "diff strategy selected"
            );
            if let Some(ref sink) = self.evidence_sink {
                let line = format!(
                    r#"{{"schema_version":"{}","event":"diff_decision","run_id":"{}","event_idx":{},"screen_mode":"{}","cols":{},"rows":{},"strategy":"{}","cost_full":{:.6},"cost_dirty":{:.6},"cost_redraw":{:.6},"posterior_mean":{:.6},"posterior_variance":{:.6},"alpha":{:.6},"beta":{:.6},"guard_reason":"{}","hysteresis_applied":{},"hysteresis_ratio":{:.6},"dirty_rows":{},"total_rows":{},"total_cells":{},"span_count":{},"span_coverage_pct":{:.6},"max_span_len":{},"fallback_reason":"{}","scan_cost_estimate":{},"tile_used":{},"tile_fallback":"{}","tile_w":{},"tile_h":{},"tile_size":{},"tiles_x":{},"tiles_y":{},"dirty_tiles":{},"dirty_tile_count":{},"dirty_cells":{},"dirty_tile_ratio":{:.6},"dirty_cell_ratio":{:.6},"scanned_tiles":{},"skipped_tiles":{},"skipped_tile_count":{},"tile_scan_cells_estimate":{},"sat_build_cost_est":{},"bayesian_enabled":{},"dirty_rows_enabled":{}}}"#,
                    schema_version,
                    run_id,
                    event_idx,
                    screen_mode,
                    width,
                    height,
                    strategy_json,
                    evidence.cost_full,
                    evidence.cost_dirty,
                    evidence.cost_redraw,
                    evidence.posterior_mean,
                    evidence.posterior_variance,
                    evidence.alpha,
                    evidence.beta,
                    guard_reason_json,
                    evidence.hysteresis_applied,
                    evidence.hysteresis_ratio,
                    evidence.dirty_rows,
                    evidence.total_rows,
                    evidence.total_cells,
                    span_count,
                    span_coverage_pct,
                    max_span_len,
                    fallback_reason_json,
                    scan_cost,
                    tile_used,
                    tile_fallback_json,
                    tile_w,
                    tile_h,
                    tile_size,
                    tiles_x,
                    tiles_y,
                    dirty_tiles,
                    dirty_tile_count,
                    dirty_cells,
                    dirty_tile_ratio,
                    dirty_cell_ratio,
                    scanned_tiles,
                    skipped_tiles,
                    skipped_tile_count,
                    scan_cells_estimate,
                    sat_build_cost_est,
                    self.diff_config.bayesian_enabled,
                    self.diff_config.dirty_rows_enabled,
                );
                let _ = sink.write_jsonl(&line);
            }
        }

        self.last_diff_strategy = Some(strategy);
        DiffDecision { strategy, has_diff }
    }

    /// Present UI in inline mode with cursor save/restore.
    ///
    /// When the scroll-region strategy is active, DECSTBM is set to constrain
    /// log scrolling to the region above the UI. This prevents log output from
    /// overwriting the UI, reducing redraw work.
    fn present_inline(
        &mut self,
        buffer: &Buffer,
        ui_height: u16,
        cursor: Option<(u16, u16)>,
        cursor_visible: bool,
    ) -> io::Result<FrameEmitStats> {
        let result = (|| -> io::Result<FrameEmitStats> {
            let visible_height = ui_height.min(self.term_height);
            let ui_y_start = self.ui_start_row();
            let current_region = InlineRegion {
                start: ui_y_start,
                height: visible_height,
            };

            // Begin sync output if available
            if self.capabilities.sync_output && !self.in_sync_block {
                self.writer().write_all(SYNC_BEGIN)?;
                self.in_sync_block = true;
            }

            // Save cursor (DEC save)
            self.writer().write_all(CURSOR_SAVE)?;
            self.cursor_saved = true;

            // Activate scroll region if strategy calls for it
            {
                let _span = debug_span!("ftui.render.scroll_region").entered();
                if visible_height > 0 {
                    match self.inline_strategy {
                        InlineStrategy::ScrollRegion | InlineStrategy::Hybrid => {
                            self.activate_scroll_region(visible_height)?;
                        }
                        InlineStrategy::OverlayRedraw => {}
                    }
                } else if self.scroll_region_active {
                    self.deactivate_scroll_region()?;
                }
            }

            self.clear_inline_region_diff(current_region)?;

            let mut diff_strategy = DiffStrategy::FullRedraw;
            let mut diff_us = 0u64;
            let mut emit_stats = EmitStats {
                diff_cells: 0,
                diff_runs: 0,
            };

            if visible_height > 0 {
                // If this is a full redraw (no previous buffer), we must clear the
                // entire UI region first to ensure we aren't diffing against garbage.
                if self.prev_buffer.is_none() {
                    self.clear_rows(ui_y_start, visible_height)?;
                } else {
                    // If the buffer is shorter than the visible height, clear the remaining rows
                    // to prevent ghosting from previous larger buffers.
                    let buf_height = buffer.height().min(visible_height);
                    if buf_height < visible_height {
                        let clear_start = ui_y_start.saturating_add(buf_height);
                        let clear_height = visible_height.saturating_sub(buf_height);
                        self.clear_rows(clear_start, clear_height)?;
                    }
                }

                // Compute diff
                let diff_start = if self.timing_enabled {
                    Some(Instant::now())
                } else {
                    None
                };
                let decision = {
                    let _span = debug_span!("ftui.render.diff_compute").entered();
                    self.decide_diff(buffer)
                };
                if let Some(start) = diff_start {
                    diff_us = start.elapsed().as_micros() as u64;
                }
                diff_strategy = decision.strategy;

                // Emit diff
                {
                    let _span = debug_span!("ftui.render.emit").entered();
                    if decision.has_diff {
                        let diff = std::mem::take(&mut self.diff_scratch);
                        let result =
                            self.emit_diff(buffer, &diff, Some(visible_height), ui_y_start);
                        self.diff_scratch = diff;
                        emit_stats = result?;
                    } else {
                        emit_stats =
                            self.emit_full_redraw(buffer, Some(visible_height), ui_y_start)?;
                    }
                }
            }

            // Reset style so subsequent log output doesn't inherit UI styling.
            self.writer().write_all(b"\x1b[0m")?;

            // Restore cursor
            self.writer().write_all(CURSOR_RESTORE)?;
            self.cursor_saved = false;

            if cursor_visible {
                // Apply requested cursor position (relative to UI)
                if let Some((cx, cy)) = cursor
                    && cy < visible_height
                {
                    // Move to UI start + cursor y
                    let abs_y = ui_y_start.saturating_add(cy);
                    write!(
                        self.writer(),
                        "\x1b[{};{}H",
                        abs_y.saturating_add(1),
                        cx.saturating_add(1)
                    )?;
                }
                self.set_cursor_visibility(true)?;
            } else {
                self.set_cursor_visibility(false)?;
            }

            // End sync output
            if self.in_sync_block {
                self.writer().write_all(SYNC_END)?;
                self.in_sync_block = false;
            }

            self.writer().flush()?;
            self.last_inline_region = if visible_height > 0 {
                Some(current_region)
            } else {
                None
            };

            if self.timing_enabled {
                self.last_present_timings = Some(PresentTimings { diff_us });
            }

            Ok(FrameEmitStats {
                diff_strategy,
                diff_cells: emit_stats.diff_cells,
                diff_runs: emit_stats.diff_runs,
                ui_height: visible_height,
            })
        })();

        if result.is_err() {
            self.best_effort_inline_cleanup();
        }

        result
    }

    /// Present UI in alternate screen mode (simpler, no cursor gymnastics).
    fn present_altscreen(
        &mut self,
        buffer: &Buffer,
        cursor: Option<(u16, u16)>,
        cursor_visible: bool,
    ) -> io::Result<FrameEmitStats> {
        let diff_start = if self.timing_enabled {
            Some(Instant::now())
        } else {
            None
        };
        let decision = {
            let _span = debug_span!("ftui.render.diff_compute").entered();
            self.decide_diff(buffer)
        };
        let diff_us = diff_start
            .map(|start| start.elapsed().as_micros() as u64)
            .unwrap_or(0);

        // Begin sync if available
        if self.capabilities.sync_output {
            self.writer().write_all(SYNC_BEGIN)?;
        }

        let emit_stats = {
            let _span = debug_span!("ftui.render.emit").entered();
            if decision.has_diff {
                let diff = std::mem::take(&mut self.diff_scratch);
                let result = self.emit_diff(buffer, &diff, None, 0);
                self.diff_scratch = diff;
                result?
            } else {
                self.emit_full_redraw(buffer, None, 0)?
            }
        };

        // Reset style at end
        self.writer().write_all(b"\x1b[0m")?;

        if cursor_visible {
            // Apply requested cursor position
            if let Some((cx, cy)) = cursor {
                write!(
                    self.writer(),
                    "\x1b[{};{}H",
                    cy.saturating_add(1),
                    cx.saturating_add(1)
                )?;
            }
            self.set_cursor_visibility(true)?;
        } else {
            self.set_cursor_visibility(false)?;
        }

        if self.capabilities.sync_output {
            self.writer().write_all(SYNC_END)?;
        }

        self.writer().flush()?;

        if self.timing_enabled {
            self.last_present_timings = Some(PresentTimings { diff_us });
        }

        Ok(FrameEmitStats {
            diff_strategy: decision.strategy,
            diff_cells: emit_stats.diff_cells,
            diff_runs: emit_stats.diff_runs,
            ui_height: 0,
        })
    }

    /// Emit a diff directly to the writer.
    fn emit_diff(
        &mut self,
        buffer: &Buffer,
        diff: &BufferDiff,
        max_height: Option<u16>,
        ui_y_start: u16,
    ) -> io::Result<EmitStats> {
        use ftui_render::cell::{Cell, CellAttrs, StyleFlags};

        let runs = diff.runs();
        let diff_runs = runs.len();
        let diff_cells = diff.len();
        let _span = debug_span!("ftui.render.emit_diff", run_count = runs.len()).entered();

        let mut current_style: Option<(
            ftui_render::cell::PackedRgba,
            ftui_render::cell::PackedRgba,
            StyleFlags,
        )> = None;
        let mut current_link: Option<u32> = None;
        let default_cell = Cell::default();

        // Borrow writer once
        let writer = self.writer.as_mut().expect("writer has been consumed");

        for run in runs {
            if let Some(limit) = max_height
                && run.y >= limit
            {
                continue;
            }
            // Move cursor to run start
            write!(
                writer,
                "\x1b[{};{}H",
                ui_y_start.saturating_add(run.y).saturating_add(1),
                run.x0.saturating_add(1)
            )?;

            // Emit cells in the run
            let mut cursor_x = run.x0;
            for x in run.x0..=run.x1 {
                let cell = buffer.get_unchecked(x, run.y);

                // Skip continuation cells unless they are orphaned.
                let is_orphan = cell.is_continuation() && cursor_x <= x;
                if cell.is_continuation() && !is_orphan {
                    continue;
                }
                let effective_cell = if is_orphan { &default_cell } else { cell };

                // Check if style changed
                let cell_style = (
                    effective_cell.fg,
                    effective_cell.bg,
                    effective_cell.attrs.flags(),
                );
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
                let raw_link_id = effective_cell.attrs.link_id();
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
                    // Open new link if present and resolvable
                    let actually_opened = if let Some(link_id) = new_link
                        && let Some(url) = self.links.get(link_id)
                    {
                        write!(writer, "\x1b]8;;{}\x1b\\", url)?;
                        true
                    } else {
                        false
                    };
                    current_link = if actually_opened { new_link } else { None };
                }

                let raw_width = effective_cell.content.width();
                let is_zero_width_content = raw_width == 0
                    && !effective_cell.is_empty()
                    && !effective_cell.is_continuation();

                // Emit content
                if is_zero_width_content {
                    writer.write_all(b"\xEF\xBF\xBD")?;
                } else if let Some(ch) = effective_cell.content.as_char() {
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    writer.write_all(encoded.as_bytes())?;
                } else if let Some(gid) = effective_cell.content.grapheme_id() {
                    // Use pool directly with writer (no clone needed)
                    if let Some(text) = self.pool.get(gid) {
                        writer.write_all(text.as_bytes())?;
                    } else {
                        // Fallback: emit placeholder cells to preserve width.
                        for _ in 0..raw_width.max(1) {
                            writer.write_all(b"?")?;
                        }
                    }
                } else {
                    writer.write_all(b" ")?;
                }

                let advance = if effective_cell.is_empty() || is_zero_width_content {
                    1
                } else {
                    raw_width.max(1)
                };
                cursor_x = cursor_x.saturating_add(advance as u16);
            }
        }

        // Reset style
        writer.write_all(b"\x1b[0m")?;

        // Close any open link
        if current_link.is_some() {
            writer.write_all(b"\x1b]8;;\x1b\\")?;
        }

        trace!("emit_diff complete");
        Ok(EmitStats {
            diff_cells,
            diff_runs,
        })
    }

    /// Emit a full redraw without computing a diff.
    fn emit_full_redraw(
        &mut self,
        buffer: &Buffer,
        max_height: Option<u16>,
        ui_y_start: u16,
    ) -> io::Result<EmitStats> {
        use ftui_render::cell::{Cell, CellAttrs, StyleFlags};

        let height = max_height.unwrap_or(buffer.height()).min(buffer.height());
        let width = buffer.width();
        let diff_cells = width as usize * height as usize;
        let diff_runs = height as usize;

        let _span = debug_span!("ftui.render.emit_full_redraw").entered();

        let mut current_style: Option<(
            ftui_render::cell::PackedRgba,
            ftui_render::cell::PackedRgba,
            StyleFlags,
        )> = None;
        let mut current_link: Option<u32> = None;
        let default_cell = Cell::default();

        // Borrow writer once
        let writer = self.writer.as_mut().expect("writer has been consumed");

        for y in 0..height {
            write!(
                writer,
                "\x1b[{};{}H",
                ui_y_start.saturating_add(y).saturating_add(1),
                1
            )?;

            let mut cursor_x = 0u16;
            for x in 0..width {
                let cell = buffer.get_unchecked(x, y);

                // Skip continuation cells unless they are orphaned.
                let is_orphan = cell.is_continuation() && cursor_x <= x;
                if cell.is_continuation() && !is_orphan {
                    continue;
                }
                let effective_cell = if is_orphan { &default_cell } else { cell };

                // Check if style changed
                let cell_style = (
                    effective_cell.fg,
                    effective_cell.bg,
                    effective_cell.attrs.flags(),
                );
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
                let raw_link_id = effective_cell.attrs.link_id();
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
                    // Open new link if present and resolvable
                    let actually_opened = if let Some(link_id) = new_link
                        && let Some(url) = self.links.get(link_id)
                    {
                        write!(writer, "\x1b]8;;{}\x1b\\", url)?;
                        true
                    } else {
                        false
                    };
                    current_link = if actually_opened { new_link } else { None };
                }

                let raw_width = effective_cell.content.width();
                let is_zero_width_content = raw_width == 0
                    && !effective_cell.is_empty()
                    && !effective_cell.is_continuation();

                // Emit content
                if is_zero_width_content {
                    writer.write_all(b"\xEF\xBF\xBD")?;
                } else if let Some(ch) = effective_cell.content.as_char() {
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    writer.write_all(encoded.as_bytes())?;
                } else if let Some(gid) = effective_cell.content.grapheme_id() {
                    // Use pool directly with writer (no clone needed)
                    if let Some(text) = self.pool.get(gid) {
                        writer.write_all(text.as_bytes())?;
                    } else {
                        // Fallback: emit placeholder cells to preserve width.
                        for _ in 0..raw_width.max(1) {
                            writer.write_all(b"?")?;
                        }
                    }
                } else {
                    writer.write_all(b" ")?;
                }

                let advance = if effective_cell.is_empty() || is_zero_width_content {
                    1
                } else {
                    raw_width.max(1)
                };
                cursor_x = cursor_x.saturating_add(advance as u16);
            }
        }

        // Reset style
        writer.write_all(b"\x1b[0m")?;

        // Close any open link
        if current_link.is_some() {
            writer.write_all(b"\x1b]8;;\x1b\\")?;
        }

        trace!("emit_full_redraw complete");
        Ok(EmitStats {
            diff_cells,
            diff_runs,
        })
    }

    /// Emit SGR flags.
    fn emit_style_flags(
        writer: &mut impl Write,
        flags: ftui_render::cell::StyleFlags,
    ) -> io::Result<()> {
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
    #[allow(dead_code)] // API for future diff strategy integration
    fn create_full_diff(&self, buffer: &Buffer) -> BufferDiff {
        BufferDiff::full(buffer.width(), buffer.height())
    }

    /// Write log output (goes to scrollback region in inline mode).
    ///
    /// In inline mode, this writes to the log region (above UI for bottom-anchored,
    /// below UI for top-anchored). The cursor is explicitly positioned in the log
    /// region before writing to prevent UI corruption.
    ///
    /// If the UI consumes the entire terminal height, there is no log region
    /// available and the write becomes a no-op.
    ///
    /// In AltScreen mode, logs are typically not shown (returns Ok silently).
    pub fn write_log(&mut self, text: &str) -> io::Result<()> {
        match self.screen_mode {
            ScreenMode::Inline { ui_height } => {
                if !self.position_cursor_for_log(ui_height)? {
                    return Ok(());
                }
                // Invalidate state if we are not using a scroll region, as the log write
                // might scroll the terminal and shift/corrupt the UI region.
                if !self.scroll_region_active {
                    self.prev_buffer = None;
                    self.last_inline_region = None;
                    self.reset_diff_strategy();
                }

                self.writer().write_all(text.as_bytes())?;
                self.writer().flush()
            }
            ScreenMode::InlineAuto { .. } => {
                // InlineAuto: use effective_ui_height for positioning.
                let ui_height = self.effective_ui_height();
                if !self.position_cursor_for_log(ui_height)? {
                    return Ok(());
                }
                // Invalidate state if we are not using a scroll region.
                if !self.scroll_region_active {
                    self.prev_buffer = None;
                    self.last_inline_region = None;
                    self.reset_diff_strategy();
                }

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

    /// Position cursor at the bottom of the log region for writing.
    ///
    /// For bottom-anchored UI: log region is above the UI (rows 1 to term_height - ui_height).
    /// For top-anchored UI: log region is below the UI (rows ui_height + 1 to term_height).
    ///
    /// Positions at the bottom row of the log region so newlines cause scrolling.
    fn position_cursor_for_log(&mut self, ui_height: u16) -> io::Result<bool> {
        let visible_height = ui_height.min(self.term_height);
        if visible_height >= self.term_height {
            // No log region available when UI fills the terminal
            return Ok(false);
        }

        let log_row = match self.ui_anchor {
            UiAnchor::Bottom => {
                // Log region is above UI: rows 1 to (term_height - ui_height)
                // Position at the bottom of the log region
                self.term_height.saturating_sub(visible_height)
            }
            UiAnchor::Top => {
                // Log region is below UI: rows (ui_height + 1) to term_height
                // Position at the bottom of the log region (last row)
                self.term_height
            }
        };

        // Move to the target row, column 1 (1-indexed)
        write!(self.writer(), "\x1b[{};1H", log_row)?;
        Ok(true)
    }

    /// Clear the screen.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.writer().write_all(b"\x1b[2J\x1b[1;1H")?;
        self.writer().flush()?;
        self.prev_buffer = None;
        self.last_inline_region = None;
        self.reset_diff_strategy();
        Ok(())
    }

    fn set_cursor_visibility(&mut self, visible: bool) -> io::Result<()> {
        if self.cursor_visible == visible {
            return Ok(());
        }
        self.cursor_visible = visible;
        if visible {
            self.writer().write_all(b"\x1b[?25h")?;
        } else {
            self.writer().write_all(b"\x1b[?25l")?;
        }
        Ok(())
    }

    /// Hide the cursor.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.set_cursor_visibility(false)?;
        self.writer().flush()
    }

    /// Show the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.set_cursor_visibility(true)?;
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

    /// Borrow the grapheme pool and link registry together.
    ///
    /// This avoids double-borrowing `self` at call sites that need both.
    pub fn pool_and_links_mut(&mut self) -> (&mut GraphemePool, &mut LinkRegistry) {
        (&mut self.pool, &mut self.links)
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
        self.writer.take()?.into_inner().into_inner().ok()
    }

    /// Perform garbage collection on the grapheme pool.
    ///
    /// Frees graphemes that are not referenced by the current front buffer (`prev_buffer`).
    /// This should be called periodically (e.g. every N frames) to prevent memory leaks
    /// in long-running applications with dynamic content (e.g. streaming logs with emoji).
    pub fn gc(&mut self) {
        let buffers = if let Some(ref buf) = self.prev_buffer {
            vec![buf]
        } else {
            vec![]
        };
        self.pool.gc(&buffers);
    }

    /// Best-effort cleanup when inline present fails mid-frame.
    ///
    /// This restores sync/cursor/scroll-region state without terminating the writer.
    fn best_effort_inline_cleanup(&mut self) {
        let Some(ref mut writer) = self.writer else {
            return;
        };

        // Emit restorations unconditionally: write errors can occur after bytes
        // were partially written, so internal flags may be stale.
        if self.capabilities.sync_output {
            let _ = writer.write_all(SYNC_END);
        }
        self.in_sync_block = false;

        let _ = writer.write_all(CURSOR_RESTORE);
        self.cursor_saved = false;

        let _ = writer.write_all(b"\x1b[r");
        self.scroll_region_active = false;

        let _ = writer.write_all(b"\x1b[0m");
        let _ = writer.write_all(b"\x1b[?25h");
        self.cursor_visible = true;
        let _ = writer.flush();
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
        self.cursor_visible = true;

        // Flush
        let _ = writer.flush();

        if let Some(ref mut trace) = self.render_trace {
            let _ = trace.finish(None);
        }
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
    use ftui_render::cell::{Cell, PackedRgba};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn max_cursor_row(output: &[u8]) -> u16 {
        let mut max_row = 0u16;
        let mut i = 0;
        while i + 2 < output.len() {
            if output[i] == 0x1b && output[i + 1] == b'[' {
                let mut j = i + 2;
                let mut row: u16 = 0;
                let mut saw_row = false;
                while j < output.len() && output[j].is_ascii_digit() {
                    saw_row = true;
                    row = row
                        .saturating_mul(10)
                        .saturating_add((output[j] - b'0') as u16);
                    j += 1;
                }
                if saw_row && j < output.len() && output[j] == b';' {
                    j += 1;
                    let mut saw_col = false;
                    while j < output.len() && output[j].is_ascii_digit() {
                        saw_col = true;
                        j += 1;
                    }
                    if saw_col && j < output.len() && output[j] == b'H' {
                        max_row = max_row.max(row);
                    }
                }
            }
            i += 1;
        }
        max_row
    }

    fn basic_caps() -> TerminalCapabilities {
        TerminalCapabilities::basic()
    }

    fn full_caps() -> TerminalCapabilities {
        let mut caps = TerminalCapabilities::basic();
        caps.true_color = true;
        caps.sync_output = true;
        caps
    }

    fn find_nth(haystack: &[u8], needle: &[u8], nth: usize) -> Option<usize> {
        if nth == 0 {
            return None;
        }
        let mut count = 0;
        let mut i = 0;
        while i + needle.len() <= haystack.len() {
            if &haystack[i..i + needle.len()] == needle {
                count += 1;
                if count == nth {
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }

    fn temp_evidence_path(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ftui_{}_{}_{}.jsonl",
            label,
            std::process::id(),
            id
        ));
        path
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
            writer.present_ui(&buffer, None, true).unwrap();
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
            writer.present_ui(&buffer, None, true).unwrap();
        }

        // Should contain sync begin and end
        assert!(output.windows(SYNC_BEGIN.len()).any(|w| w == SYNC_BEGIN));
        assert!(output.windows(SYNC_END.len()).any(|w| w == SYNC_END));
    }

    #[test]
    fn present_ui_hides_cursor_when_requested() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 5);

            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer, None, false).unwrap();
        }

        assert!(
            output.windows(6).any(|w| w == b"\x1b[?25l"),
            "expected cursor hide sequence"
        );
    }

    #[test]
    fn present_ui_visible_does_not_hide_cursor() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 5);

            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !output.windows(6).any(|w| w == b"\x1b[?25l"),
            "did not expect cursor hide sequence"
        );
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

        let output_str = String::from_utf8_lossy(&output);
        // Should not contain log text (altscreen drops logs)
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
        writer.present_ui(&buffer, None, true).unwrap();
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
    fn inline_auto_resize_clears_cached_height() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::InlineAuto {
                min_height: 3,
                max_height: 8,
            },
            UiAnchor::Bottom,
            basic_caps(),
        );

        writer.set_size(80, 24);
        writer.set_auto_ui_height(6);
        assert_eq!(writer.auto_ui_height(), Some(6));
        assert_eq!(writer.render_height_hint(), 6);

        writer.set_size(100, 30);
        assert_eq!(writer.auto_ui_height(), None);
        assert_eq!(writer.render_height_hint(), 8);
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
        writer.present_ui(&buffer1, None, true).unwrap();

        // Second frame - same content (diff is empty, minimal output)
        writer.present_ui(&buffer1, None, true).unwrap();

        // Third frame - change one cell
        let mut buffer2 = buffer1.clone();
        buffer2.set_raw(1, 0, Cell::from_char('B'));
        writer.present_ui(&buffer2, None, true).unwrap();

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
            writer.present_ui(&buffer, None, true).unwrap();
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
    fn inline_auto_height_clamps_and_uses_max_for_render() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::InlineAuto {
                min_height: 3,
                max_height: 8,
            },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);

        // Default to min height until measured.
        assert_eq!(writer.ui_height(), 3);
        assert_eq!(writer.auto_ui_height(), None);

        // render_height_hint uses max to allow measurement when cache is empty.
        assert_eq!(writer.render_height_hint(), 8);

        // Cache hit: render_height_hint uses cached height.
        writer.set_auto_ui_height(6);
        assert_eq!(writer.render_height_hint(), 6);

        // Cache miss: clearing restores max hint.
        writer.clear_auto_ui_height();
        assert_eq!(writer.render_height_hint(), 8);

        // Cache should still set when clamped to min.
        writer.set_auto_ui_height(3);
        assert_eq!(writer.auto_ui_height(), Some(3));
        assert_eq!(writer.ui_height(), 3);

        writer.clear_auto_ui_height();
        assert_eq!(writer.render_height_hint(), 8);

        // Clamp to max.
        writer.set_auto_ui_height(10);
        assert_eq!(writer.ui_height(), 8);

        // Clamp to min.
        writer.set_auto_ui_height(1);
        assert_eq!(writer.ui_height(), 3);
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
            writer.present_ui(&buffer, None, true).unwrap();
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
            writer.present_ui(&buffer, None, true).unwrap();

            // Write a log
            writer.write_log("log line\n").unwrap();

            // Present UI again
            writer.present_ui(&buffer, None, true).unwrap();
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

    #[test]
    fn inline_ui_height_clamped_to_terminal_height() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 10 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(8, 3);
            let buffer = Buffer::new(8, 10);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        let max_row = max_cursor_row(&output);
        assert!(
            max_row <= 3,
            "cursor row {} exceeds terminal height",
            max_row
        );
    }

    #[test]
    fn inline_shrink_clears_stale_rows() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::InlineAuto {
                    min_height: 1,
                    max_height: 6,
                },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 10);

            let buffer = Buffer::new(10, 6);
            writer.set_auto_ui_height(6);
            writer.present_ui(&buffer, None, true).unwrap();

            writer.set_auto_ui_height(3);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        let second_save = find_nth(&output, CURSOR_SAVE, 2).expect("expected second cursor save");
        let after_save = &output[second_save..];
        let restore_idx = after_save
            .windows(CURSOR_RESTORE.len())
            .position(|w| w == CURSOR_RESTORE)
            .expect("expected cursor restore after second save");
        let segment = &after_save[..restore_idx];
        let erase_count = segment
            .windows(ERASE_LINE.len())
            .filter(|w| *w == ERASE_LINE)
            .count();

        assert_eq!(erase_count, 6, "expected clears for stale + new rows");
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
    fn scroll_region_bounds_bottom_anchor() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                scroll_region_caps(),
            );
            writer.set_size(10, 10);
            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        let seq = b"\x1b[1;5r";
        assert!(
            output.windows(seq.len()).any(|w| w == seq),
            "expected scroll region for bottom anchor"
        );
    }

    #[test]
    fn scroll_region_bounds_top_anchor() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Top,
                scroll_region_caps(),
            );
            writer.set_size(10, 10);
            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        let seq = b"\x1b[6;10r";
        assert!(
            output.windows(seq.len()).any(|w| w == seq),
            "expected scroll region for top anchor"
        );
        let cursor_seq = b"\x1b[6;1H";
        assert!(
            output.windows(cursor_seq.len()).any(|w| w == cursor_seq),
            "expected cursor move into log region for top anchor"
        );
    }

    #[test]
    fn present_ui_inline_resets_style_before_cursor_restore() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 2 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(5, 5);
            let mut buffer = Buffer::new(5, 2);
            buffer.set_raw(0, 0, Cell::from_char('X').with_fg(PackedRgba::RED));
            writer.present_ui(&buffer, None, true).unwrap();
        }

        let seq = b"\x1b[0m\x1b8";
        assert!(
            output.windows(seq.len()).any(|w| w == seq),
            "expected SGR reset before cursor restore in inline mode"
        );
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
            writer.present_ui(&buffer, None, true).unwrap();
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
            writer.present_ui(&buffer, None, true).unwrap();
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
            writer.present_ui(&buffer, None, true).unwrap();
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
            writer.present_ui(&buffer, None, true).unwrap();
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
            writer.present_ui(&buffer, None, true).unwrap();
            assert!(writer.scroll_region_active());

            // Resize deactivates
            writer.set_size(80, 40);
            assert!(!writer.scroll_region_active());

            // Next present re-activates with new dimensions
            let buffer2 = Buffer::new(80, 5);
            writer.present_ui(&buffer2, None, true).unwrap();
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
            writer.present_ui(&buffer, None, true).unwrap();
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
        writer.present_ui(&buffer, None, true).unwrap();
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
            writer.present_ui(&buffer, None, true).unwrap();
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

    // --- Log write cursor positioning tests (bd-xh8s) ---

    #[test]
    fn write_log_positions_cursor_bottom_anchor() {
        // Verify log writes position cursor at the bottom of the log region
        // for bottom-anchored UI (log region is above UI).
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("test log\n").unwrap();
        }

        // For bottom-anchored with ui_height=5, term_height=24:
        // Log region is rows 1-19 (24-5=19 rows)
        // Cursor should be positioned at row 19 (bottom of log region)
        let expected_pos = b"\x1b[19;1H";
        assert!(
            output
                .windows(expected_pos.len())
                .any(|w| w == expected_pos),
            "Log write should position cursor at row 19 for bottom anchor"
        );
    }

    #[test]
    fn write_log_positions_cursor_top_anchor() {
        // Verify log writes position cursor at the bottom of the log region
        // for top-anchored UI (log region is below UI).
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Top,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("test log\n").unwrap();
        }

        // For top-anchored with ui_height=5, term_height=24:
        // Log region is rows 6-24 (below UI)
        // Cursor should be positioned at row 24 (bottom of log region)
        let expected_pos = b"\x1b[24;1H";
        assert!(
            output
                .windows(expected_pos.len())
                .any(|w| w == expected_pos),
            "Log write should position cursor at row 24 for top anchor"
        );
    }

    #[test]
    fn write_log_contains_text() {
        // Verify the log text is actually written after cursor positioning.
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("hello world\n").unwrap();
        }

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("hello world"));
    }

    #[test]
    fn write_log_multiple_writes_position_each_time() {
        // Verify cursor is positioned for each log write.
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("first\n").unwrap();
            writer.write_log("second\n").unwrap();
        }

        // Should have cursor positioning twice
        let expected_pos = b"\x1b[19;1H";
        let count = output
            .windows(expected_pos.len())
            .filter(|w| *w == expected_pos)
            .count();
        assert_eq!(count, 2, "Should position cursor for each log write");
    }

    #[test]
    fn write_log_after_present_ui_works_correctly() {
        // Verify log writes work correctly after UI presentation.
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            // Present UI first
            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer, None, true).unwrap();

            // Then write log
            writer.write_log("after UI\n").unwrap();
        }

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("after UI"));

        // Log write should still position cursor
        let expected_pos = b"\x1b[19;1H";
        // Find position after cursor restore (log write happens after present_ui)
        assert!(
            output
                .windows(expected_pos.len())
                .any(|w| w == expected_pos),
            "Log write after present_ui should position cursor"
        );
    }

    #[test]
    fn write_log_ui_fills_terminal_is_noop() {
        // When UI fills the entire terminal, there's no log region.
        // Drop cleanup writes reset sequences (\x1b[0m, \x1b[?25h), so we
        // verify the output does not contain the log text itself.
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 24 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("should still write\n").unwrap();
        }
        // Log text must NOT appear; only Drop cleanup sequences are expected.
        assert!(
            !output
                .windows(b"should still write".len())
                .any(|w| w == b"should still write"),
            "write_log should not emit log text when UI fills the terminal"
        );
    }

    #[test]
    fn write_log_with_scroll_region_active() {
        // Verify log writes work correctly when scroll region is active.
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                scroll_region_caps(),
            );
            writer.set_size(80, 24);

            // Present UI to activate scroll region
            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer, None, true).unwrap();
            assert!(writer.scroll_region_active());

            // Log write should still position cursor
            writer.write_log("with scroll region\n").unwrap();
        }

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("with scroll region"));
    }

    #[test]
    fn log_write_cursor_position_not_in_ui_region_bottom_anchor() {
        // Verify the cursor position for log writes is never in the UI region.
        // For bottom-anchored with ui_height=5, term_height=24:
        // UI region is rows 20-24 (1-indexed)
        // Log region is rows 1-19
        // Log cursor should be at row 19 (bottom of log region)
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("test\n").unwrap();
        }

        // Parse cursor position commands in output
        // Looking for ESC [ row ; col H patterns
        let mut found_row = None;
        let mut i = 0;
        while i + 2 < output.len() {
            if output[i] == 0x1b && output[i + 1] == b'[' {
                let mut j = i + 2;
                let mut row: u16 = 0;
                while j < output.len() && output[j].is_ascii_digit() {
                    row = row * 10 + (output[j] - b'0') as u16;
                    j += 1;
                }
                if j < output.len() && output[j] == b';' {
                    j += 1;
                    while j < output.len() && output[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j < output.len() && output[j] == b'H' {
                        found_row = Some(row);
                    }
                }
            }
            i += 1;
        }

        if let Some(row) = found_row {
            // UI region starts at row 20 (24 - 5 + 1 = 20)
            assert!(
                row < 20,
                "Log cursor row {} should be below UI start row 20",
                row
            );
        }
    }

    #[test]
    fn log_write_cursor_position_not_in_ui_region_top_anchor() {
        // Verify the cursor position for log writes is never in the UI region.
        // For top-anchored with ui_height=5, term_height=24:
        // UI region is rows 1-5 (1-indexed)
        // Log region is rows 6-24
        // Log cursor should be at row 24 (bottom of log region)
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Top,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("test\n").unwrap();
        }

        // Parse cursor position commands in output
        let mut found_row = None;
        let mut i = 0;
        while i + 2 < output.len() {
            if output[i] == 0x1b && output[i + 1] == b'[' {
                let mut j = i + 2;
                let mut row: u16 = 0;
                while j < output.len() && output[j].is_ascii_digit() {
                    row = row * 10 + (output[j] - b'0') as u16;
                    j += 1;
                }
                if j < output.len() && output[j] == b';' {
                    j += 1;
                    while j < output.len() && output[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j < output.len() && output[j] == b'H' {
                        found_row = Some(row);
                    }
                }
            }
            i += 1;
        }

        if let Some(row) = found_row {
            // UI region is rows 1-5
            assert!(
                row > 5,
                "Log cursor row {} should be above UI end row 5",
                row
            );
        }
    }

    #[test]
    fn present_ui_positions_cursor_after_restore() {
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
            // Request cursor at (2, 1) in UI coordinates
            writer.present_ui(&buffer, Some((2, 1)), true).unwrap();
        }

        // UI starts at row 20 (24 - 5 + 1 = 20) (1-indexed)
        // Cursor requested at relative (2, 1) -> (x=3, y=2) (1-indexed)
        // Absolute position: y = 20 + 1 = 21. x = 3.
        let expected_pos = b"\x1b[21;3H";

        // Find restore
        let restore_idx = find_nth(&output, CURSOR_RESTORE, 1).expect("expected cursor restore");
        let after_restore = &output[restore_idx..];

        // Ensure cursor positioning happens *after* restore
        assert!(
            after_restore
                .windows(expected_pos.len())
                .any(|w| w == expected_pos),
            "Cursor positioning should happen after restore"
        );
    }

    // =========================================================================
    // RuntimeDiffConfig tests
    // =========================================================================

    #[test]
    fn runtime_diff_config_default() {
        let config = RuntimeDiffConfig::default();
        assert!(config.bayesian_enabled);
        assert!(config.dirty_rows_enabled);
        assert!(config.dirty_span_config.enabled);
        assert!(config.tile_diff_config.enabled);
        assert!(config.reset_on_resize);
        assert!(config.reset_on_invalidation);
    }

    #[test]
    fn runtime_diff_config_builder() {
        let custom_span = DirtySpanConfig::default().with_max_spans_per_row(8);
        let tile_config = TileDiffConfig::default()
            .with_enabled(false)
            .with_tile_size(24, 12)
            .with_dense_tile_ratio(0.75)
            .with_max_tiles(2048);
        let config = RuntimeDiffConfig::new()
            .with_bayesian_enabled(false)
            .with_dirty_rows_enabled(false)
            .with_dirty_span_config(custom_span)
            .with_dirty_spans_enabled(false)
            .with_tile_diff_config(tile_config)
            .with_reset_on_resize(false)
            .with_reset_on_invalidation(false);

        assert!(!config.bayesian_enabled);
        assert!(!config.dirty_rows_enabled);
        assert!(!config.dirty_span_config.enabled);
        assert_eq!(config.dirty_span_config.max_spans_per_row, 8);
        assert!(!config.tile_diff_config.enabled);
        assert_eq!(config.tile_diff_config.tile_w, 24);
        assert_eq!(config.tile_diff_config.tile_h, 12);
        assert_eq!(config.tile_diff_config.max_tiles, 2048);
        assert!(!config.reset_on_resize);
        assert!(!config.reset_on_invalidation);
    }

    #[test]
    fn with_diff_config_applies_strategy_config() {
        use ftui_render::diff_strategy::DiffStrategyConfig;

        let strategy_config = DiffStrategyConfig {
            prior_alpha: 5.0,
            prior_beta: 5.0,
            ..Default::default()
        };

        let runtime_config =
            RuntimeDiffConfig::default().with_strategy_config(strategy_config.clone());

        let writer = TerminalWriter::with_diff_config(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            runtime_config,
        );

        // Verify the strategy config was applied
        let (alpha, beta) = writer.diff_strategy().posterior_params();
        assert!((alpha - 5.0).abs() < 0.001);
        assert!((beta - 5.0).abs() < 0.001);
    }

    #[test]
    fn with_diff_config_applies_tile_config() {
        let tile_config = TileDiffConfig::default()
            .with_enabled(false)
            .with_tile_size(32, 16)
            .with_max_tiles(1024);
        let runtime_config = RuntimeDiffConfig::default().with_tile_diff_config(tile_config);

        let mut writer = TerminalWriter::with_diff_config(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            runtime_config,
        );

        let applied = writer.diff_scratch.tile_config_mut();
        assert!(!applied.enabled);
        assert_eq!(applied.tile_w, 32);
        assert_eq!(applied.tile_h, 16);
        assert_eq!(applied.max_tiles, 1024);
    }

    #[test]
    fn diff_config_accessor() {
        let config = RuntimeDiffConfig::default().with_bayesian_enabled(false);

        let writer = TerminalWriter::with_diff_config(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            config,
        );

        assert!(!writer.diff_config().bayesian_enabled);
    }

    #[test]
    fn last_diff_strategy_updates_after_present() {
        let mut output = Vec::new();
        let mut writer = TerminalWriter::with_diff_config(
            &mut output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            RuntimeDiffConfig::default(),
        );
        writer.set_size(10, 3);

        let mut buffer = Buffer::new(10, 3);
        buffer.set_raw(0, 0, Cell::from_char('X'));

        assert!(writer.last_diff_strategy().is_none());
        writer.present_ui(&buffer, None, false).unwrap();
        assert_eq!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));

        buffer.set_raw(1, 1, Cell::from_char('Y'));
        writer.present_ui(&buffer, None, false).unwrap();
        assert!(writer.last_diff_strategy().is_some());
    }

    #[test]
    fn diff_decision_evidence_schema_includes_span_fields() {
        let evidence_path = temp_evidence_path("diff_decision_schema");
        let sink = EvidenceSink::from_config(
            &crate::evidence_sink::EvidenceSinkConfig::enabled_file(&evidence_path),
        )
        .expect("evidence sink config")
        .expect("evidence sink enabled");

        let mut writer = TerminalWriter::with_diff_config(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            RuntimeDiffConfig::default(),
        )
        .with_evidence_sink(sink);
        writer.set_size(10, 3);

        let mut buffer = Buffer::new(10, 3);
        buffer.set_raw(0, 0, Cell::from_char('X'));
        writer.present_ui(&buffer, None, false).unwrap();

        buffer.set_raw(1, 1, Cell::from_char('Y'));
        writer.present_ui(&buffer, None, false).unwrap();

        let jsonl = std::fs::read_to_string(&evidence_path).expect("read evidence jsonl");
        let line = jsonl
            .lines()
            .find(|line| line.contains("\"event\":\"diff_decision\""))
            .expect("diff_decision line");
        let value: serde_json::Value = serde_json::from_str(line).expect("valid json");

        assert_eq!(
            value["schema_version"],
            crate::evidence_sink::EVIDENCE_SCHEMA_VERSION
        );
        assert_eq!(value["event"], "diff_decision");
        assert!(
            value["run_id"]
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false),
            "run_id should be a non-empty string"
        );
        assert!(
            value["event_idx"].is_number(),
            "event_idx should be numeric"
        );
        assert_eq!(value["screen_mode"], "altscreen");
        assert!(value["cols"].is_number(), "cols should be numeric");
        assert!(value["rows"].is_number(), "rows should be numeric");
        assert!(
            value["span_count"].is_number(),
            "span_count should be numeric"
        );
        assert!(
            value["span_coverage_pct"].is_number(),
            "span_coverage_pct should be numeric"
        );
        assert!(
            value["tile_size"].is_number(),
            "tile_size should be numeric"
        );
        assert!(
            value["dirty_tile_count"].is_number(),
            "dirty_tile_count should be numeric"
        );
        assert!(
            value["skipped_tile_count"].is_number(),
            "skipped_tile_count should be numeric"
        );
        assert!(
            value["sat_build_cost_est"].is_number(),
            "sat_build_cost_est should be numeric"
        );
        assert!(
            value["fallback_reason"].is_string(),
            "fallback_reason should be string"
        );
        assert!(
            value["scan_cost_estimate"].is_number(),
            "scan_cost_estimate should be numeric"
        );
        assert!(
            value["max_span_len"].is_number(),
            "max_span_len should be numeric"
        );
        assert!(
            value["guard_reason"].is_string(),
            "guard_reason should be a string"
        );
        assert!(
            value["hysteresis_applied"].is_boolean(),
            "hysteresis_applied should be boolean"
        );
        assert!(
            value["hysteresis_ratio"].is_number(),
            "hysteresis_ratio should be numeric"
        );
        assert!(
            value["fallback_reason"].is_string(),
            "fallback_reason should be a string"
        );
        assert!(
            value["scan_cost_estimate"].is_number(),
            "scan_cost_estimate should be numeric"
        );
    }

    #[test]
    fn diff_strategy_posterior_updates_with_total_cells() {
        let mut output = Vec::new();
        let mut writer = TerminalWriter::with_diff_config(
            &mut output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            RuntimeDiffConfig::default(),
        );
        writer.set_size(10, 10);

        let mut buffer = Buffer::new(10, 10);
        buffer.set_raw(0, 0, Cell::from_char('A'));
        writer.present_ui(&buffer, None, false).unwrap();

        let mut buffer2 = Buffer::new(10, 10);
        for x in 0..10u16 {
            buffer2.set_raw(x, 0, Cell::from_char('X'));
        }
        writer.present_ui(&buffer2, None, false).unwrap();

        let config = writer.diff_strategy().config().clone();
        let total_cells = 10usize * 10usize;
        let changed = 10usize;
        let alpha = config.prior_alpha * config.decay + changed as f64;
        let beta = config.prior_beta * config.decay + (total_cells - changed) as f64;
        let expected = alpha / (alpha + beta);
        let mean = writer.diff_strategy().posterior_mean();
        assert!(
            (mean - expected).abs() < 1e-9,
            "posterior mean should use total_cells; got {mean:.6}, expected {expected:.6}"
        );
    }

    #[test]
    fn log_write_without_scroll_region_resets_diff_strategy() {
        // When log writes occur without scroll region protection,
        // the diff strategy posterior should be reset to priors.
        let mut output = Vec::new();
        {
            let config = RuntimeDiffConfig::default();
            let mut writer = TerminalWriter::with_diff_config(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(), // no scroll region support
                config,
            );
            writer.set_size(80, 24);

            // Present a frame and observe some changes to modify posterior
            let mut buffer = Buffer::new(80, 5);
            buffer.set_raw(0, 0, Cell::from_char('X'));
            writer.present_ui(&buffer, None, false).unwrap();

            // Posterior should have been updated from initial priors
            let (_alpha_before, _) = writer.diff_strategy().posterior_params();

            // Present another frame
            buffer.set_raw(1, 1, Cell::from_char('Y'));
            writer.present_ui(&buffer, None, false).unwrap();

            // Log write without scroll region should reset
            assert!(!writer.scroll_region_active());
            writer.write_log("log message\n").unwrap();

            // After reset, posterior should be back to priors
            let (alpha_after, beta_after) = writer.diff_strategy().posterior_params();
            assert!(
                (alpha_after - 1.0).abs() < 0.01 && (beta_after - 19.0).abs() < 0.01,
                "posterior should reset to priors after log write: alpha={}, beta={}",
                alpha_after,
                beta_after
            );
        }
    }

    #[test]
    fn log_write_with_scroll_region_preserves_diff_strategy() {
        // When scroll region is active, log writes should NOT reset diff strategy
        let mut output = Vec::new();
        {
            let config = RuntimeDiffConfig::default();
            let mut writer = TerminalWriter::with_diff_config(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                scroll_region_caps(), // has scroll region support
                config,
            );
            writer.set_size(80, 24);

            // Present frames to activate scroll region and update posterior
            let mut buffer = Buffer::new(80, 5);
            buffer.set_raw(0, 0, Cell::from_char('X'));
            writer.present_ui(&buffer, None, false).unwrap();

            buffer.set_raw(1, 1, Cell::from_char('Y'));
            writer.present_ui(&buffer, None, false).unwrap();

            assert!(writer.scroll_region_active());

            // Get posterior before log write
            let (alpha_before, beta_before) = writer.diff_strategy().posterior_params();

            // Log write with scroll region active should NOT reset
            writer.write_log("log message\n").unwrap();

            let (alpha_after, beta_after) = writer.diff_strategy().posterior_params();
            assert!(
                (alpha_after - alpha_before).abs() < 0.01
                    && (beta_after - beta_before).abs() < 0.01,
                "posterior should be preserved with scroll region: before=({}, {}), after=({}, {})",
                alpha_before,
                beta_before,
                alpha_after,
                beta_after
            );
        }
    }

    #[test]
    fn strategy_selection_config_flags_applied() {
        // Verify that RuntimeDiffConfig flags are correctly stored and accessible
        let config = RuntimeDiffConfig::default()
            .with_dirty_rows_enabled(false)
            .with_bayesian_enabled(false);

        let writer = TerminalWriter::with_diff_config(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            config,
        );

        // Config should be accessible
        assert!(!writer.diff_config().dirty_rows_enabled);
        assert!(!writer.diff_config().bayesian_enabled);

        // Diff strategy should use the underlying strategy config
        let (alpha, beta) = writer.diff_strategy().posterior_params();
        // Default priors
        assert!((alpha - 1.0).abs() < 0.01);
        assert!((beta - 19.0).abs() < 0.01);
    }

    #[test]
    fn resize_respects_reset_toggle() {
        // With reset_on_resize disabled, posterior should be preserved after resize
        let config = RuntimeDiffConfig::default().with_reset_on_resize(false);

        let mut writer = TerminalWriter::with_diff_config(
            Vec::<u8>::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            config,
        );
        writer.set_size(80, 24);

        // Present frames to update posterior
        let mut buffer = Buffer::new(80, 24);
        buffer.set_raw(0, 0, Cell::from_char('X'));
        writer.present_ui(&buffer, None, false).unwrap();

        let mut buffer2 = Buffer::new(80, 24);
        buffer2.set_raw(1, 1, Cell::from_char('Y'));
        writer.present_ui(&buffer2, None, false).unwrap();

        // Posterior should have moved from initial priors
        let (alpha_before, beta_before) = writer.diff_strategy().posterior_params();

        // Resize - with reset disabled, posterior should be preserved
        writer.set_size(100, 30);

        let (alpha_after, beta_after) = writer.diff_strategy().posterior_params();
        assert!(
            (alpha_after - alpha_before).abs() < 0.01 && (beta_after - beta_before).abs() < 0.01,
            "posterior should be preserved when reset_on_resize=false"
        );
    }

    // =========================================================================
    // Enum / Default / Debug tests
    // =========================================================================

    #[test]
    fn screen_mode_default_is_altscreen() {
        assert_eq!(ScreenMode::default(), ScreenMode::AltScreen);
    }

    #[test]
    fn screen_mode_debug_format() {
        let dbg = format!("{:?}", ScreenMode::Inline { ui_height: 7 });
        assert!(dbg.contains("Inline"));
        assert!(dbg.contains('7'));
    }

    #[test]
    fn screen_mode_inline_auto_debug_format() {
        let dbg = format!(
            "{:?}",
            ScreenMode::InlineAuto {
                min_height: 3,
                max_height: 10
            }
        );
        assert!(dbg.contains("InlineAuto"));
    }

    #[test]
    fn screen_mode_eq_inline_auto() {
        let a = ScreenMode::InlineAuto {
            min_height: 2,
            max_height: 8,
        };
        let b = ScreenMode::InlineAuto {
            min_height: 2,
            max_height: 8,
        };
        assert_eq!(a, b);
        let c = ScreenMode::InlineAuto {
            min_height: 2,
            max_height: 9,
        };
        assert_ne!(a, c);
    }

    #[test]
    fn ui_anchor_default_is_bottom() {
        assert_eq!(UiAnchor::default(), UiAnchor::Bottom);
    }

    #[test]
    fn ui_anchor_debug_format() {
        assert_eq!(format!("{:?}", UiAnchor::Top), "Top");
        assert_eq!(format!("{:?}", UiAnchor::Bottom), "Bottom");
    }

    // =========================================================================
    // Accessor tests
    // =========================================================================

    #[test]
    fn width_height_accessors() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        // Default dimensions are 80x24
        assert_eq!(writer.width(), 80);
        assert_eq!(writer.height(), 24);

        writer.set_size(120, 40);
        assert_eq!(writer.width(), 120);
        assert_eq!(writer.height(), 40);
    }

    #[test]
    fn screen_mode_accessor() {
        let writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Top,
            basic_caps(),
        );
        assert_eq!(writer.screen_mode(), ScreenMode::Inline { ui_height: 5 });
    }

    #[test]
    fn capabilities_accessor() {
        let caps = full_caps();
        let writer = TerminalWriter::new(Vec::new(), ScreenMode::AltScreen, UiAnchor::Bottom, caps);
        assert!(writer.capabilities().true_color);
        assert!(writer.capabilities().sync_output);
    }

    // =========================================================================
    // into_inner tests
    // =========================================================================

    #[test]
    fn into_inner_returns_writer() {
        let writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        let inner = writer.into_inner();
        assert!(inner.is_some());
    }

    #[test]
    fn into_inner_performs_cleanup() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.cursor_saved = true;
        writer.in_sync_block = false;

        let inner = writer.into_inner().unwrap();
        // Cleanup should have written cursor restore
        assert!(
            inner
                .windows(CURSOR_RESTORE.len())
                .any(|w| w == CURSOR_RESTORE),
            "into_inner should perform cleanup before returning"
        );
    }

    // =========================================================================
    // take_render_buffer tests
    // =========================================================================

    #[test]
    fn take_render_buffer_creates_new_when_no_spare() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        let buf = writer.take_render_buffer(80, 24);
        assert_eq!(buf.width(), 80);
        assert_eq!(buf.height(), 24);
    }

    #[test]
    fn take_render_buffer_reuses_spare_on_match() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        // Inject a spare buffer
        writer.spare_buffer = Some(Buffer::new(80, 24));
        assert!(writer.spare_buffer.is_some());

        let buf = writer.take_render_buffer(80, 24);
        assert_eq!(buf.width(), 80);
        assert_eq!(buf.height(), 24);
        // Spare should have been taken
        assert!(writer.spare_buffer.is_none());
    }

    #[test]
    fn take_render_buffer_ignores_spare_on_size_mismatch() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.spare_buffer = Some(Buffer::new(80, 24));

        // Request different size - should create new, not reuse
        let buf = writer.take_render_buffer(100, 30);
        assert_eq!(buf.width(), 100);
        assert_eq!(buf.height(), 30);
    }

    // =========================================================================
    // gc tests
    // =========================================================================

    #[test]
    fn gc_with_no_prev_buffer() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        assert!(writer.prev_buffer.is_none());
        // Should not panic
        writer.gc();
    }

    #[test]
    fn gc_with_prev_buffer() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.prev_buffer = Some(Buffer::new(10, 5));
        // Should not panic
        writer.gc();
    }

    // =========================================================================
    // hide_cursor / show_cursor tests
    // =========================================================================

    #[test]
    fn hide_cursor_emits_sequence() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.hide_cursor().unwrap();
        }
        assert!(
            output.windows(6).any(|w| w == b"\x1b[?25l"),
            "hide_cursor should emit cursor hide sequence"
        );
    }

    #[test]
    fn show_cursor_emits_sequence() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            // First hide, then show
            writer.hide_cursor().unwrap();
            writer.show_cursor().unwrap();
        }
        assert!(
            output.windows(6).any(|w| w == b"\x1b[?25h"),
            "show_cursor should emit cursor show sequence"
        );
    }

    #[test]
    fn hide_cursor_idempotent() {
        // Use Cursor<Vec<u8>> to own the writer
        use std::io::Cursor;
        let mut writer = TerminalWriter::new(
            Cursor::new(Vec::new()),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.hide_cursor().unwrap();
        let inner = writer.into_inner().unwrap().into_inner();
        let hide_count = inner.windows(6).filter(|w| *w == b"\x1b[?25l").count();
        // Should have exactly 1 hide (from hide_cursor) — Drop cleanup shows cursor (?25h)
        assert_eq!(
            hide_count, 1,
            "hide_cursor called once should emit exactly one hide sequence"
        );
    }

    #[test]
    fn show_cursor_idempotent_when_already_visible() {
        use std::io::Cursor;
        let mut writer = TerminalWriter::new(
            Cursor::new(Vec::new()),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        // Cursor starts visible — show should be noop
        writer.show_cursor().unwrap();
        let inner = writer.into_inner().unwrap().into_inner();
        // No ?25h should appear from show_cursor (only from cleanup)
        let show_count = inner.windows(6).filter(|w| *w == b"\x1b[?25h").count();
        assert!(
            show_count <= 1,
            "show_cursor when already visible should not add extra show sequences"
        );
    }

    // =========================================================================
    // pool / links accessor tests
    // =========================================================================

    #[test]
    fn pool_accessor() {
        let writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        // Pool should be accessible (just testing it doesn't panic)
        let _pool = writer.pool();
    }

    #[test]
    fn pool_mut_accessor() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        let _pool = writer.pool_mut();
    }

    #[test]
    fn links_accessor() {
        let writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        let _links = writer.links();
    }

    #[test]
    fn links_mut_accessor() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        let _links = writer.links_mut();
    }

    #[test]
    fn pool_and_links_mut_accessor() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        let (_pool, _links) = writer.pool_and_links_mut();
    }

    // =========================================================================
    // Helper function tests
    // =========================================================================

    #[test]
    fn sanitize_auto_bounds_normal() {
        assert_eq!(sanitize_auto_bounds(3, 10), (3, 10));
    }

    #[test]
    fn sanitize_auto_bounds_zero_min() {
        // min=0 should become 1
        assert_eq!(sanitize_auto_bounds(0, 10), (1, 10));
    }

    #[test]
    fn sanitize_auto_bounds_max_less_than_min() {
        // max < min should be clamped to min
        assert_eq!(sanitize_auto_bounds(5, 3), (5, 5));
    }

    #[test]
    fn sanitize_auto_bounds_both_zero() {
        assert_eq!(sanitize_auto_bounds(0, 0), (1, 1));
    }

    #[test]
    fn diff_strategy_str_variants() {
        assert_eq!(diff_strategy_str(DiffStrategy::Full), "full");
        assert_eq!(diff_strategy_str(DiffStrategy::DirtyRows), "dirty");
        assert_eq!(diff_strategy_str(DiffStrategy::FullRedraw), "redraw");
    }

    #[test]
    fn ui_anchor_str_variants() {
        assert_eq!(ui_anchor_str(UiAnchor::Bottom), "bottom");
        assert_eq!(ui_anchor_str(UiAnchor::Top), "top");
    }

    #[test]
    fn json_escape_plain_text() {
        assert_eq!(json_escape("hello"), "hello");
    }

    #[test]
    fn json_escape_special_chars() {
        assert_eq!(json_escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(json_escape("a\\b"), r#"a\\b"#);
        assert_eq!(json_escape("a\nb"), r#"a\nb"#);
        assert_eq!(json_escape("a\rb"), r#"a\rb"#);
        assert_eq!(json_escape("a\tb"), r#"a\tb"#);
    }

    #[test]
    fn json_escape_control_chars() {
        let s = String::from("\x00\x01\x1f");
        let escaped = json_escape(&s);
        assert!(escaped.contains("\\u0000"));
        assert!(escaped.contains("\\u0001"));
        assert!(escaped.contains("\\u001F"));
    }

    #[test]
    fn json_escape_unicode_passthrough() {
        assert_eq!(json_escape("caf\u{00e9}"), "caf\u{00e9}");
        assert_eq!(json_escape("\u{1f600}"), "\u{1f600}");
    }

    // =========================================================================
    // CountingWriter tests
    // =========================================================================

    #[test]
    fn counting_writer_no_counting_by_default() {
        let mut cw = CountingWriter::new(Vec::new());
        cw.write_all(b"hello").unwrap();
        assert_eq!(cw.take_count(), 0);
    }

    #[test]
    fn counting_writer_counts_when_enabled() {
        let mut cw = CountingWriter::new(Vec::new());
        cw.enable_counting();
        cw.write_all(b"hello").unwrap();
        assert_eq!(cw.take_count(), 5);
    }

    #[test]
    fn counting_writer_take_count_resets() {
        let mut cw = CountingWriter::new(Vec::new());
        cw.enable_counting();
        cw.write_all(b"abc").unwrap();
        assert_eq!(cw.take_count(), 3);
        // After take, count resets
        assert_eq!(cw.take_count(), 0);
    }

    #[test]
    fn counting_writer_disable_stops_counting() {
        let mut cw = CountingWriter::new(Vec::new());
        cw.enable_counting();
        cw.write_all(b"abc").unwrap();
        cw.disable_counting();
        cw.write_all(b"def").unwrap();
        // Only "abc" was counted
        assert_eq!(cw.take_count(), 3);
    }

    #[test]
    fn counting_writer_write_counts_partial() {
        let mut cw = CountingWriter::new(Vec::new());
        cw.enable_counting();
        let written = cw.write(b"hello world").unwrap();
        assert_eq!(written, 11);
        assert_eq!(cw.take_count(), 11);
    }

    #[test]
    fn counting_writer_flush() {
        let mut cw = CountingWriter::new(Vec::new());
        cw.flush().unwrap();
    }

    #[test]
    fn counting_writer_into_inner() {
        let mut cw = CountingWriter::new(Vec::new());
        cw.write_all(b"data").unwrap();
        let inner = cw.into_inner();
        assert_eq!(inner, b"data");
    }

    // =========================================================================
    // estimate_diff_scan_cost tests
    // =========================================================================

    fn zero_span_stats() -> DirtySpanStats {
        DirtySpanStats {
            rows_full_dirty: 0,
            rows_with_spans: 0,
            total_spans: 0,
            overflows: 0,
            span_coverage_cells: 0,
            max_span_len: 0,
            max_spans_per_row: 4,
        }
    }

    #[test]
    fn estimate_diff_scan_cost_full_strategy() {
        let stats = zero_span_stats();
        let (cost, label) = estimate_diff_scan_cost(DiffStrategy::Full, 0, 80, 24, &stats, None);
        assert_eq!(cost, 80 * 24);
        assert_eq!(label, "full_strategy");
    }

    #[test]
    fn estimate_diff_scan_cost_full_redraw() {
        let stats = zero_span_stats();
        let (cost, label) =
            estimate_diff_scan_cost(DiffStrategy::FullRedraw, 5, 80, 24, &stats, None);
        assert_eq!(cost, 0);
        assert_eq!(label, "full_redraw");
    }

    #[test]
    fn estimate_diff_scan_cost_dirty_rows_no_dirty() {
        let stats = zero_span_stats();
        let (cost, label) =
            estimate_diff_scan_cost(DiffStrategy::DirtyRows, 0, 80, 24, &stats, None);
        assert_eq!(cost, 0);
        assert_eq!(label, "no_dirty_rows");
    }

    #[test]
    fn estimate_diff_scan_cost_dirty_rows_with_span_coverage() {
        let mut stats = zero_span_stats();
        stats.span_coverage_cells = 100;
        let (cost, label) =
            estimate_diff_scan_cost(DiffStrategy::DirtyRows, 5, 80, 24, &stats, None);
        assert_eq!(cost, 100);
        assert_eq!(label, "none");
    }

    #[test]
    fn estimate_diff_scan_cost_dirty_rows_no_spans() {
        let stats = zero_span_stats();
        let (cost, label) =
            estimate_diff_scan_cost(DiffStrategy::DirtyRows, 5, 80, 24, &stats, None);
        assert_eq!(cost, 5 * 80);
        assert_eq!(label, "no_spans");
    }

    #[test]
    fn estimate_diff_scan_cost_dirty_rows_overflow_with_span() {
        let mut stats = zero_span_stats();
        stats.span_coverage_cells = 150;
        stats.overflows = 1;
        let (cost, label) =
            estimate_diff_scan_cost(DiffStrategy::DirtyRows, 5, 80, 24, &stats, None);
        assert_eq!(cost, 150);
        assert_eq!(label, "span_overflow");
    }

    #[test]
    fn estimate_diff_scan_cost_dirty_rows_overflow_no_span() {
        let mut stats = zero_span_stats();
        stats.overflows = 1;
        let (cost, label) =
            estimate_diff_scan_cost(DiffStrategy::DirtyRows, 5, 80, 24, &stats, None);
        assert_eq!(cost, 5 * 80);
        assert_eq!(label, "span_overflow");
    }

    #[test]
    fn estimate_diff_scan_cost_tile_skip() {
        let stats = zero_span_stats();
        let tile = TileDiffStats {
            width: 80,
            height: 24,
            tile_w: 16,
            tile_h: 8,
            tiles_x: 5,
            tiles_y: 3,
            total_tiles: 15,
            dirty_cells: 10,
            dirty_tiles: 2,
            dirty_cell_ratio: 0.005,
            dirty_tile_ratio: 0.13,
            scanned_tiles: 2,
            skipped_tiles: 13,
            sat_build_cells: 1920,
            scan_cells_estimate: 42,
            fallback: None,
        };
        let (cost, label) =
            estimate_diff_scan_cost(DiffStrategy::DirtyRows, 5, 80, 24, &stats, Some(tile));
        assert_eq!(cost, 42);
        assert_eq!(label, "tile_skip");
    }

    #[test]
    fn estimate_diff_scan_cost_tile_with_fallback_uses_spans() {
        let mut stats = zero_span_stats();
        stats.span_coverage_cells = 200;
        let tile = TileDiffStats {
            width: 80,
            height: 24,
            tile_w: 16,
            tile_h: 8,
            tiles_x: 5,
            tiles_y: 3,
            total_tiles: 15,
            dirty_cells: 10,
            dirty_tiles: 2,
            dirty_cell_ratio: 0.005,
            dirty_tile_ratio: 0.13,
            scanned_tiles: 2,
            skipped_tiles: 13,
            sat_build_cells: 1920,
            scan_cells_estimate: 42,
            fallback: Some(TileDiffFallback::SmallScreen),
        };
        let (cost, label) =
            estimate_diff_scan_cost(DiffStrategy::DirtyRows, 5, 80, 24, &stats, Some(tile));
        // Tile has fallback, so falls through to span logic
        assert_eq!(cost, 200);
        assert_eq!(label, "none");
    }

    // =========================================================================
    // InlineAuto edge cases
    // =========================================================================

    #[test]
    fn inline_auto_bounds_accessor() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::InlineAuto {
                min_height: 3,
                max_height: 10,
            },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);
        let bounds = writer.inline_auto_bounds();
        assert_eq!(bounds, Some((3, 10)));
    }

    #[test]
    fn inline_auto_bounds_clamped_to_terminal() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::InlineAuto {
                min_height: 3,
                max_height: 50,
            },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 20);
        let bounds = writer.inline_auto_bounds();
        assert_eq!(bounds, Some((3, 20)));
    }

    #[test]
    fn inline_auto_bounds_returns_none_for_non_auto() {
        let writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        assert_eq!(writer.inline_auto_bounds(), None);

        let writer2 = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        assert_eq!(writer2.inline_auto_bounds(), None);
    }

    #[test]
    fn auto_ui_height_returns_none_for_non_auto() {
        let writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        assert_eq!(writer.auto_ui_height(), None);
    }

    #[test]
    fn render_height_hint_altscreen() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);
        assert_eq!(writer.render_height_hint(), 24);
    }

    #[test]
    fn render_height_hint_inline_fixed() {
        let writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 7 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        assert_eq!(writer.render_height_hint(), 7);
    }

    // =========================================================================
    // RuntimeDiffConfig builder edge cases
    // =========================================================================

    #[test]
    fn runtime_diff_config_tile_skip_toggle() {
        let config = RuntimeDiffConfig::new().with_tile_skip_enabled(false);
        assert!(!config.tile_diff_config.enabled);
    }

    #[test]
    fn runtime_diff_config_dirty_spans_toggle() {
        let config = RuntimeDiffConfig::new().with_dirty_spans_enabled(false);
        assert!(!config.dirty_span_config.enabled);
    }

    // =========================================================================
    // present_ui edge cases
    // =========================================================================

    #[test]
    fn present_ui_altscreen_no_cursor_save_restore() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 5);
            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        // AltScreen should NOT use cursor save/restore (those are inline-mode specific)
        let save_count = output
            .windows(CURSOR_SAVE.len())
            .filter(|w| *w == CURSOR_SAVE)
            .count();
        assert_eq!(save_count, 0, "AltScreen should not save cursor");
    }

    #[test]
    fn clear_screen_emits_ed2() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.clear_screen().unwrap();
        }
        assert!(
            output.windows(4).any(|w| w == b"\x1b[2J"),
            "clear_screen should emit ED2 sequence"
        );
    }

    #[test]
    fn set_size_resets_scroll_region_and_spare_buffer() {
        let output = Vec::new();
        let mut writer = TerminalWriter::new(
            output,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.spare_buffer = Some(Buffer::new(80, 24));
        writer.set_size(100, 30);
        assert!(writer.spare_buffer.is_none());
    }
}
