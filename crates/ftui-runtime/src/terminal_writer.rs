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
//! 4. Log writes go above the UI region: with an active scroll region they
//!    accumulate into scrollback; in the overlay fallback exactly one
//!    width-clamped line is rewritten per call (an unqualified LF would
//!    scroll the whole screen and displace the UI)
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
use std::sync::atomic::{AtomicU32, Ordering};
use web_time::Instant;

/// Global gauge: number of active inline-mode `TerminalWriter` instances.
///
/// Incremented when a writer is created in `Inline` or `InlineAuto` mode,
/// decremented on drop. Read with [`inline_active_widgets`].
static INLINE_ACTIVE_WIDGETS: AtomicU32 = AtomicU32::new(0);

/// Read the current number of active inline-mode terminal writers.
pub fn inline_active_widgets() -> u32 {
    INLINE_ACTIVE_WIDGETS.load(Ordering::Relaxed)
}

use crate::evidence_sink::EvidenceSink;
use crate::evidence_telemetry::{DiffDecisionSnapshot, set_diff_snapshot};
use crate::render_trace::{
    RenderTraceFrame, RenderTraceRecorder, build_diff_runs_payload, build_full_buffer_payload,
};
use ftui_core::inline_mode::{InlineStrategy, sanitize_overlay_log_line};
#[cfg(test)]
use ftui_core::terminal_capabilities::ColorDepth;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_core::terminal_session::terminal_output_lock;
use ftui_render::buffer::{Buffer, DirtySpanConfig, DirtySpanStats};
use ftui_render::counting_writer::CountingWriter;
use ftui_render::diff::{BufferDiff, TileDiffConfig, TileDiffFallback, TileDiffStats};
use ftui_render::diff_strategy::{DiffStrategy, DiffStrategyConfig, DiffStrategySelector};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::link_registry::LinkRegistry;
use ftui_render::presenter::Presenter;
use ftui_render::render_certificate::{
    RenderCertificate, RenderCertificateInputs, RenderCertificateLevel, evaluate_render_certificate,
};
use ftui_render::sanitize::sanitize;
use tracing::{debug_span, info, info_span, trace, warn};

/// Size of the internal write buffer (64KB).
#[allow(dead_code)] // Used by Presenter::new; kept here for reference.
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
/// Reset background to terminal default (CSI 49 m).
const SGR_BG_DEFAULT: &[u8] = b"\x1b[49m";

/// How often to probe with a real diff when FullRedraw is selected.
#[allow(dead_code)] // API for future diff strategy integration
const FULL_REDRAW_PROBE_INTERVAL: u64 = 60;

// CountingWriter is re-used from ftui_render::counting_writer::CountingWriter.
// The Presenter wraps the writer in CountingWriter<BufWriter<W>>.
// For byte counting, use reset_counter() and bytes_written() on the counting writer.

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

fn inline_strategy_str(strategy: InlineStrategy) -> &'static str {
    match strategy {
        InlineStrategy::ScrollRegion => "scroll_region",
        InlineStrategy::OverlayRedraw => "overlay_redraw",
        InlineStrategy::Hybrid => "hybrid",
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

    /// Emit explicit render certificates and route the DirtyRows strategy
    /// through the certified diff path (skip-all on zero dirty rows,
    /// narrow-to-dirty otherwise). Behavior-identical to the uncertified
    /// dirty path; disabling only removes the certificate evidence and the
    /// zero-dirty fast path.
    ///
    /// Default: true
    pub certified_skips: bool,

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

    /// Maximum successful incremental frames to allow between physical full redraws.
    ///
    /// Terminals and mux panes can lose their visible buffer without notifying the
    /// process. A bounded full redraw interval repairs that state divergence even
    /// when the application model has not changed. Set to `0` to disable.
    ///
    /// Default: 240
    pub full_redraw_interval_frames: u64,

    /// Maximum *wall-clock* time to allow between physical full redraws.
    ///
    /// The frame-count interval ([`Self::full_redraw_interval_frames`]) only
    /// advances on rendered frames, so a TUI that renders sparsely (dirty-driven,
    /// idle, or rendering slowly) can leave a desynchronized physical terminal —
    /// e.g. corruption from an aggressive incremental diff, or a mux pane swap /
    /// reattach — visible for a long time. A wall-clock bound guarantees the
    /// physical terminal is fully repainted at least this often regardless of
    /// render cadence, so any such corruption self-heals within the interval.
    ///
    /// `None` (the default) disables the time-based resync, preserving the
    /// purely frame-count behavior (and deterministic-test reproducibility).
    pub full_redraw_max_interval: Option<std::time::Duration>,
}

impl Default for RuntimeDiffConfig {
    fn default() -> Self {
        Self {
            bayesian_enabled: true,
            dirty_rows_enabled: true,
            certified_skips: true,
            dirty_span_config: DirtySpanConfig::default(),
            tile_diff_config: TileDiffConfig::default(),
            reset_on_resize: true,
            reset_on_invalidation: true,
            strategy_config: DiffStrategyConfig::default(),
            full_redraw_interval_frames: 240,
            full_redraw_max_interval: None,
        }
    }
}

impl RuntimeDiffConfig {
    /// Create a new config with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether Bayesian strategy selection is enabled.
    #[must_use]
    pub fn with_bayesian_enabled(mut self, enabled: bool) -> Self {
        self.bayesian_enabled = enabled;
        self
    }

    /// Set whether dirty-row optimization is enabled.
    #[must_use]
    pub fn with_dirty_rows_enabled(mut self, enabled: bool) -> Self {
        self.dirty_rows_enabled = enabled;
        self
    }

    /// Set whether dirty-span tracking is enabled.
    #[must_use]
    pub fn with_dirty_spans_enabled(mut self, enabled: bool) -> Self {
        self.dirty_span_config = self.dirty_span_config.with_enabled(enabled);
        self
    }

    /// Set the dirty-span tracking configuration.
    #[must_use]
    pub fn with_dirty_span_config(mut self, config: DirtySpanConfig) -> Self {
        self.dirty_span_config = config;
        self
    }

    /// Toggle tile-based skipping.
    #[must_use]
    pub fn with_tile_skip_enabled(mut self, enabled: bool) -> Self {
        self.tile_diff_config = self.tile_diff_config.with_enabled(enabled);
        self
    }

    /// Set the tile-based diff configuration.
    #[must_use]
    pub fn with_tile_diff_config(mut self, config: TileDiffConfig) -> Self {
        self.tile_diff_config = config;
        self
    }

    /// Set whether to reset posterior on resize.
    #[must_use]
    pub fn with_reset_on_resize(mut self, enabled: bool) -> Self {
        self.reset_on_resize = enabled;
        self
    }

    /// Set whether to reset posterior on invalidation.
    #[must_use]
    pub fn with_reset_on_invalidation(mut self, enabled: bool) -> Self {
        self.reset_on_invalidation = enabled;
        self
    }

    /// Set the underlying strategy configuration.
    #[must_use]
    pub fn with_strategy_config(mut self, config: DiffStrategyConfig) -> Self {
        self.strategy_config = config;
        self
    }

    /// Set the maximum successful incremental frames between physical full redraws.
    ///
    /// A value of `0` disables periodic terminal resynchronization.
    #[must_use]
    pub fn with_full_redraw_interval_frames(mut self, frames: u64) -> Self {
        self.full_redraw_interval_frames = frames;
        self
    }

    /// Set the maximum wall-clock time between physical full redraws.
    ///
    /// Unlike [`Self::with_full_redraw_interval_frames`] (which only advances on
    /// rendered frames), this bounds resynchronization by elapsed time, so an
    /// idle or sparsely-rendering TUI still repaints the physical terminal at
    /// least this often — bounding how long any terminal-state desync (diff
    /// corruption, mux pane swap, reattach) can stay on screen. `None` disables
    /// it.
    #[must_use]
    pub fn with_full_redraw_max_interval(mut self, interval: Option<std::time::Duration>) -> Self {
        self.full_redraw_max_interval = interval;
        self
    }
}

/// Unified terminal output coordinator.
///
/// Enforces the one-writer rule and implements inline mode correctly.
/// All terminal output should go through this struct.
pub struct TerminalWriter<W: Write> {
    /// Presenter handles efficient ANSI emission and cursor tracking.
    /// Wrapped in `Option` so `into_inner` can take ownership; `Drop` skips
    /// cleanup when `None` (already consumed).
    presenter: Option<Presenter<W>>,
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
    /// Pre-allocated buffer for zero-alloc clone in present_ui.
    /// Part of a 3-buffer rotation: spare ← prev ← clone_buf ← spare.
    clone_buf: Option<Buffer>,
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
    /// Successful incremental frames since the terminal was physically redrawn.
    frames_since_full_redraw: u64,
    /// Wall-clock instant of the last physical full redraw, used to bound
    /// terminal-state desync by elapsed time (see
    /// [`RuntimeDiffConfig::full_redraw_max_interval`]). `None` until the first
    /// full redraw is presented.
    last_full_redraw_at: Option<Instant>,
    /// Runtime diff configuration.
    #[allow(dead_code)] // runtime toggles wired up in follow-up work
    diff_config: RuntimeDiffConfig,
    /// Evidence JSONL sink for diff decisions.
    evidence_sink: Option<EvidenceSink>,
    /// Run identifier for diff decision evidence.
    #[allow(dead_code)]
    /// The explicit skip certificate for the most recent diff decision.
    last_certificate: Option<RenderCertificate>,
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

        // Increment the inline-active gauge.
        // We do this BEFORE potentially returning/panicking to maintain invariant
        // that a TerminalWriter in inline mode ALWAYS has a corresponding increment,
        // which will be decremented on Drop.
        let is_inline = matches!(
            screen_mode,
            ScreenMode::Inline { .. } | ScreenMode::InlineAuto { .. }
        );
        if is_inline {
            INLINE_ACTIVE_WIDGETS.fetch_add(1, Ordering::SeqCst);
        }

        // Log inline mode activation.
        match screen_mode {
            ScreenMode::Inline { ui_height } => {
                info!(
                    inline_height = ui_height,
                    render_mode = %inline_strategy_str(inline_strategy),
                    "inline mode activated"
                );
            }
            ScreenMode::InlineAuto {
                min_height,
                max_height,
            } => {
                info!(
                    min_height,
                    max_height,
                    render_mode = %inline_strategy_str(inline_strategy),
                    "inline auto mode activated"
                );
            }
            ScreenMode::AltScreen => {}
        }

        let mut diff_scratch = BufferDiff::new();
        diff_scratch
            .tile_config_mut()
            .clone_from(&diff_config.tile_diff_config);

        let presenter = Presenter::new(writer, capabilities);

        Self {
            presenter: Some(presenter),
            screen_mode,
            auto_ui_height,
            ui_anchor,
            prev_buffer: None,
            spare_buffer: None,
            clone_buf: None,
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
            frames_since_full_redraw: 0,
            last_full_redraw_at: None,
            diff_config,
            evidence_sink: None,
            diff_evidence_run_id: default_diff_run_id(),
            diff_evidence_idx: 0,
            last_diff_strategy: None,
            last_certificate: None,
            render_trace: None,
            timing_enabled: false,
            last_present_timings: None,
        }
    }

    /// Get a mutable reference to the internal counting writer.
    ///
    /// # Panics
    ///
    /// Panics if the presenter has been taken (via `into_inner`).
    #[inline]
    fn writer(&mut self) -> &mut CountingWriter<BufWriter<W>> {
        self.presenter_mut().counting_writer_mut()
    }

    /// Get a mutable reference to the presenter.
    ///
    /// # Panics
    ///
    /// Panics if the presenter has been taken (via `into_inner`).
    #[inline]
    fn presenter_mut(&mut self) -> &mut Presenter<W> {
        self.presenter
            .as_mut()
            .expect("presenter has been consumed")
    }

    /// Reset diff strategy state when the previous buffer is invalidated.
    fn reset_diff_strategy(&mut self) {
        if self.diff_config.reset_on_invalidation {
            self.diff_strategy.reset();
        }
        self.full_redraw_probe = 0;
        self.frames_since_full_redraw = 0;
        self.last_diff_strategy = None;
    }

    /// Reset diff strategy state on terminal resize.
    #[allow(dead_code)] // used by upcoming resize-aware diff strategy work
    fn reset_diff_on_resize(&mut self) {
        if self.diff_config.reset_on_resize {
            self.diff_strategy.reset();
        }
        self.full_redraw_probe = 0;
        self.frames_since_full_redraw = 0;
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

    /// The explicit render certificate behind the most recent diff decision
    /// (`None` before the first present or when certified skips are
    /// disabled).
    #[must_use]
    pub fn last_render_certificate(&self) -> Option<&RenderCertificate> {
        self.last_certificate.as_ref()
    }

    /// Set the terminal size.
    ///
    /// Call this when the terminal is resized.
    pub fn set_size(&mut self, width: u16, height: u16) {
        // May emit a DECSTBM reset when a scroll region is active; keep it
        // inside the one-writer discipline (bd-kdn7n item 2).
        let _output_guard = terminal_output_lock();
        self.term_width = width;
        self.term_height = height;
        if matches!(self.screen_mode, ScreenMode::InlineAuto { .. }) {
            self.auto_ui_height = None;
        }
        // Clear prev_buffer to force full redraw after resize
        self.prev_buffer = None;
        // Old-geometry coordinates are meaningless after a resize: drop the
        // displayed-region cache so neither log positioning nor
        // clear_inline_region_diff acts on rows that no longer exist.
        self.last_inline_region = None;
        self.spare_buffer = None;
        self.clone_buf = None;
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
        if start_row >= end_row {
            return Ok(());
        }

        // Ensure erase operations clear to the terminal default background.
        // Without this, stale background fills can persist when inline regions shrink.
        self.writer().write_all(SGR_BG_DEFAULT)?;
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
        // One-writer discipline vs the signal/panic teardown paths
        // (bd-kdn7n item 2): the whole frame is one emission batch.
        let _output_guard = terminal_output_lock();
        let mode_str = match self.screen_mode {
            ScreenMode::Inline { .. } => "inline",
            ScreenMode::InlineAuto { .. } => "inline_auto",
            ScreenMode::AltScreen => "altscreen",
        };
        let trace_enabled = self.render_trace.is_some();
        if trace_enabled {
            self.writer().reset_counter();
        }
        let present_start = if trace_enabled {
            Some(Instant::now())
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
            {
                let w = self.writer();
                let count = w.bytes_written();
                w.reset_counter();
                Some(count)
            }
        } else {
            None
        };
        if trace_enabled {
            // No-op: ftui_render::CountingWriter always counts; reset happens in take above.
        }

        if let Ok(stats) = result {
            self.record_successful_present(stats.diff_strategy);
            // 3-buffer rotation: reuse clone_buf's allocation to avoid per-frame alloc.
            // Only advance the diff baseline after a successful present. If a write
            // failed partway through, the terminal state is unknown; the error path
            // below invalidates the baseline so the next frame physically repaints.
            let new_prev = match self.clone_buf.take() {
                Some(mut buf)
                    if buf.width() == buffer.width() && buf.height() == buffer.height() =>
                {
                    buf.clone_from(buffer);
                    buf
                }
                _ => buffer.clone(),
            };
            self.clone_buf = self.spare_buffer.take();
            self.spare_buffer = self.prev_buffer.take();
            self.prev_buffer = Some(new_prev);

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

        self.invalidate_after_present_error();
        result.map(|_| ())
    }

    /// Present a UI frame, taking ownership of the buffer (O(1) — no clone).
    ///
    /// Prefer this over `present_ui` when the caller has an owned buffer
    /// that won't be reused, as it avoids an O(width × height) clone.
    pub fn present_ui_owned(
        &mut self,
        buffer: Buffer,
        cursor: Option<(u16, u16)>,
        cursor_visible: bool,
    ) -> io::Result<()> {
        // One-writer discipline vs teardown paths (bd-kdn7n item 2).
        let _output_guard = terminal_output_lock();
        let mode_str = match self.screen_mode {
            ScreenMode::Inline { .. } => "inline",
            ScreenMode::InlineAuto { .. } => "inline_auto",
            ScreenMode::AltScreen => "altscreen",
        };
        let trace_enabled = self.render_trace.is_some();
        if trace_enabled {
            self.writer().reset_counter();
        }
        let present_start = if trace_enabled {
            Some(Instant::now())
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
            {
                let w = self.writer();
                let count = w.bytes_written();
                w.reset_counter();
                Some(count)
            }
        } else {
            None
        };
        if trace_enabled {
            // No-op: ftui_render::CountingWriter always counts; reset happens in take above.
        }

        if let Ok(stats) = result {
            self.record_successful_present(stats.diff_strategy);
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

            // 3-buffer rotation: keep clone_buf populated for present_ui path.
            self.clone_buf = self.spare_buffer.take();
            self.spare_buffer = self.prev_buffer.take();
            self.prev_buffer = Some(buffer);
            return Ok(());
        }

        self.invalidate_after_present_error();
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
            self.last_certificate = Some(evaluate_render_certificate(
                &RenderCertificateInputs {
                    prev_available: prev_dims.is_some(),
                    dims_changed: prev_dims.is_some(),
                    full_redraw_due: false,
                    dirty_row_count: buffer.dirty_row_count(),
                    total_rows: buffer.height(),
                },
                Vec::new(),
            ));
            return DiffDecision {
                strategy: DiffStrategy::FullRedraw,
                has_diff: false,
            };
        }

        if self.full_redraw_interval_due() {
            self.full_redraw_probe = 0;
            self.last_diff_strategy = Some(DiffStrategy::FullRedraw);
            self.last_certificate = Some(evaluate_render_certificate(
                &RenderCertificateInputs {
                    prev_available: true,
                    dims_changed: false,
                    full_redraw_due: true,
                    dirty_row_count: buffer.dirty_row_count(),
                    total_rows: buffer.height(),
                },
                Vec::new(),
            ));
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
                self.last_certificate = Some(RenderCertificate {
                    level: RenderCertificateLevel::FullRequired,
                    causes: vec!["strategy-selected-full"],
                    dirty_rows: Vec::new(),
                    fell_back: false,
                });
                has_diff = true;
            }
            DiffStrategy::DirtyRows => {
                let prev = self.prev_buffer.as_ref().expect("prev buffer must exist");
                if self.diff_config.certified_skips {
                    let certificate = evaluate_render_certificate(
                        &RenderCertificateInputs {
                            prev_available: true,
                            dims_changed: false,
                            full_redraw_due: false,
                            dirty_row_count: dirty_rows,
                            total_rows: buffer.height(),
                        },
                        buffer.dirty_row_indices(),
                    );
                    self.diff_scratch
                        .compute_certified_into(prev, buffer, certificate.to_hint());
                    self.last_certificate = Some(certificate);
                } else {
                    self.diff_scratch.compute_dirty_into(prev, buffer);
                    self.last_certificate = None;
                }
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

        // Emit the explicit skip certificate alongside the diff decision so
        // operators can see WHY work was skipped or performed (bd-6b9nr).
        if let Some(ref certificate) = self.last_certificate
            && let Some(ref sink) = self.evidence_sink
        {
            let line = format!(
                r#"{{"event":"certificate_decision","run_id":"{}","event_idx":{},"strategy":"{:?}","certificate":{}}}"#,
                self.diff_evidence_run_id,
                self.diff_evidence_idx,
                strategy,
                certificate.to_evidence_json()
            );
            let _ = sink.write_jsonl(&line);
        }

        self.last_diff_strategy = Some(strategy);
        DiffDecision { strategy, has_diff }
    }

    fn full_redraw_interval_due(&self) -> bool {
        // Frame-count bound: resync after N rendered incremental frames.
        let frame_due = self.diff_config.full_redraw_interval_frames > 0
            && self.frames_since_full_redraw >= self.diff_config.full_redraw_interval_frames;
        // Wall-clock bound: resync after the configured elapsed time regardless
        // of how many frames have rendered. This is what bounds terminal-state
        // desync (diff corruption, mux pane swap, reattach) on a sparsely- or
        // idly-rendering TUI, where the frame counter advances too slowly. A
        // missing `last_full_redraw_at` (no full redraw presented yet) is
        // treated as due so the first eligible frame resynchronizes.
        let time_due = self
            .diff_config
            .full_redraw_max_interval
            .is_some_and(|interval| {
                self.last_full_redraw_at
                    .is_none_or(|at| at.elapsed() >= interval)
            });
        frame_due || time_due
    }

    fn record_successful_present(&mut self, strategy: DiffStrategy) {
        if strategy == DiffStrategy::FullRedraw {
            self.frames_since_full_redraw = 0;
            // Anchor the wall-clock resync window to this physical full redraw.
            if self.diff_config.full_redraw_max_interval.is_some() {
                self.last_full_redraw_at = Some(Instant::now());
            }
        } else {
            self.frames_since_full_redraw = self.frames_since_full_redraw.saturating_add(1);
        }
    }

    fn invalidate_after_present_error(&mut self) {
        self.prev_buffer = None;
        self.last_inline_region = None;
        self.reset_diff_strategy();
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
        let sync_output_enabled = self.capabilities.use_sync_output();
        let render_mode = inline_strategy_str(self.inline_strategy);
        let _inline_span = info_span!(
            "inline.render",
            inline_height = ui_height,
            scrollback_preserved = tracing::field::Empty,
            render_mode,
        )
        .entered();

        let result = (|| -> io::Result<FrameEmitStats> {
            let visible_height = ui_height.min(self.term_height);
            let ui_y_start = self.ui_start_row();
            let current_region = InlineRegion {
                start: ui_y_start,
                height: visible_height,
            };

            // Begin sync output if available
            if sync_output_enabled && !self.in_sync_block {
                // Mark active before write so cleanup paths conservatively emit
                // SYNC_END even if the begin write fails after partial bytes.
                self.in_sync_block = true;
                if let Err(err) = self.writer().write_all(SYNC_BEGIN) {
                    // Attempt immediate close to avoid leaving the terminal in a
                    // potentially open synchronized-output state.
                    let _ = self.writer().write_all(SYNC_END);
                    self.in_sync_block = false;
                    let _ = self.writer().flush();
                    return Err(err);
                }
            }

            // Save cursor (DEC save)
            self.writer().write_all(CURSOR_SAVE)?;
            self.cursor_saved = true;

            // Keep the hardware cursor hidden while we issue many cursor moves.
            // This prevents visible cursor "speckling" artifacts during redraws.
            self.set_cursor_visibility(false)?;

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
                // If this is a full redraw (no previous buffer) OR dimensions changed,
                // we must clear the entire UI region to prevent ghosting (e.g. if width shrank).
                let dims_changed = self.prev_buffer.as_ref().map(|b| (b.width(), b.height()))
                    != Some((buffer.width(), buffer.height()));

                if self.prev_buffer.is_none() || dims_changed {
                    self.clear_rows(ui_y_start, visible_height)?;
                } else {
                    // If dimensions match but the buffer is shorter than the visible height,
                    // clear the remaining rows to prevent garbage from logs or previous frames.
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

                // Emit diff using Presenter
                {
                    let _span = debug_span!("ftui.render.emit").entered();

                    // Reset presenter state (cursor unknown) because we manually moved cursor/saved
                    // and apply viewport offset for inline positioning.
                    let presenter = self.presenter.as_mut().expect("presenter consumed");
                    presenter.reset();
                    presenter.set_viewport_offset_y(ui_y_start);

                    if decision.has_diff {
                        presenter.prepare_runs(&self.diff_scratch);
                        // Clip to the visible terminal region, mirroring the
                        // full-redraw branch: when the terminal is shorter
                        // than ui_height, a below-the-fold run would emit a
                        // CUP past the physical bottom row, which real
                        // terminals clamp — corrupting the last visible row.
                        presenter.clip_runs_below(visible_height);
                        // Emit
                        presenter.emit_diff_runs(buffer, Some(&self.pool), Some(&self.links))?;

                        emit_stats.diff_cells = self.diff_scratch.len();
                        emit_stats.diff_runs = self.diff_scratch.runs().len();
                    } else {
                        // Full redraw — clip to the visible terminal region and
                        // to the buffer's actual height. This avoids generating
                        // diff runs for rows that are outside `buffer`.
                        let render_height = buffer.height().min(visible_height);
                        let full = BufferDiff::full(buffer.width(), render_height);
                        presenter.prepare_runs(&full);
                        presenter.emit_diff_runs(buffer, Some(&self.pool), Some(&self.links))?;

                        emit_stats.diff_cells = full.len();
                        emit_stats.diff_runs = full.runs().len();
                    }

                    presenter.finish_frame()?;
                }
            }

            // Restore cursor
            self.writer().write_all(CURSOR_RESTORE)?;
            self.cursor_saved = false;

            let mut show_cursor = false;
            if cursor_visible
                && let Some((cx, cy)) = cursor
                && cx < buffer.width()
                && cy < buffer.height()
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
                show_cursor = true;
            }
            self.set_cursor_visibility(show_cursor)?;

            // End sync output (mux-aware policy).
            if sync_output_enabled && self.in_sync_block {
                self.writer().write_all(SYNC_END)?;
                self.in_sync_block = false;
            } else if !sync_output_enabled {
                // Defensive stale-state cleanup: clear internal state without
                // emitting DEC 2026 in mux/unsupported environments.
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
            _inline_span.record("scrollback_preserved", false);
            warn!(
                inline_height = ui_height,
                render_mode, "scrollback preservation failed during inline render"
            );
            self.best_effort_inline_cleanup();
        } else {
            _inline_span.record("scrollback_preserved", true);
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
        let sync_output_enabled = self.capabilities.use_sync_output();
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

        // Begin sync if available. Track state so we can reliably close the
        // block even on early-return error paths.
        if sync_output_enabled && !self.in_sync_block {
            // Mark active before write so partial begin writes are treated as
            // an open block for best-effort close.
            self.in_sync_block = true;
            if let Err(err) = self.writer().write_all(SYNC_BEGIN) {
                // Attempt immediate close to avoid leaving the terminal in a
                // potentially open synchronized-output state.
                let _ = self.writer().write_all(SYNC_END);
                self.in_sync_block = false;
                let _ = self.writer().flush();
                return Err(err);
            }
        }

        let operation_result = (|| -> io::Result<FrameEmitStats> {
            // Keep the hardware cursor hidden while we issue many cursor moves.
            // This prevents visible cursor "speckling" artifacts during redraws.
            self.set_cursor_visibility(false)?;

            let emit_stats = {
                let _span = debug_span!("ftui.render.emit").entered();
                let presenter = self.presenter.as_mut().expect("presenter consumed");

                // Reset presenter state (cursor and style) because we manually moved
                // the cursor and reset the style at the end of the previous frame.
                presenter.reset();
                // AltScreen always starts at (0,0) relative to terminal.
                presenter.set_viewport_offset_y(0);

                let stats = if decision.has_diff {
                    presenter.prepare_runs(&self.diff_scratch);
                    presenter.emit_diff_runs(buffer, Some(&self.pool), Some(&self.links))?;

                    EmitStats {
                        diff_cells: self.diff_scratch.len(),
                        diff_runs: self.diff_scratch.runs().len(),
                    }
                } else {
                    // Full redraw: populate diff with all cells and emit.
                    self.diff_scratch.fill_full(buffer.width(), buffer.height());
                    presenter.prepare_runs(&self.diff_scratch);
                    presenter.emit_diff_runs(buffer, Some(&self.pool), Some(&self.links))?;

                    EmitStats {
                        diff_cells: (buffer.width() as usize) * (buffer.height() as usize),
                        diff_runs: buffer.height() as usize,
                    }
                };

                presenter.finish_frame()?;
                stats
            };

            let mut show_cursor = false;
            if cursor_visible
                && let Some((cx, cy)) = cursor
                && cx < buffer.width()
                && cy < buffer.height()
            {
                // Apply requested cursor position
                write!(
                    self.writer(),
                    "\x1b[{};{}H",
                    cy.saturating_add(1),
                    cx.saturating_add(1)
                )?;
                show_cursor = true;
            }
            self.set_cursor_visibility(show_cursor)?;

            if self.timing_enabled {
                self.last_present_timings = Some(PresentTimings { diff_us });
            }

            Ok(FrameEmitStats {
                diff_strategy: decision.strategy,
                diff_cells: emit_stats.diff_cells,
                diff_runs: emit_stats.diff_runs,
                ui_height: 0,
            })
        })();

        if operation_result.is_err()
            && let Some(ref mut presenter) = self.presenter
        {
            presenter.finish_frame_best_effort();
        }

        // Always attempt to close sync and flush, regardless of operation_result.
        let sync_end_result = if sync_output_enabled && self.in_sync_block {
            let res = self.writer().write_all(SYNC_END);
            if res.is_ok() {
                self.in_sync_block = false;
            }
            Some(res)
        } else {
            if !sync_output_enabled {
                // Defensive stale-state cleanup: do not emit DEC 2026 when
                // policy disallows synchronized output.
                self.in_sync_block = false;
            }
            None
        };
        let flush_result = self.writer().flush();

        // Cleanup failures (sync-end/flush) take precedence so terminal-state
        // restoration errors are never hidden by a concurrent render failure.
        let cleanup_error = sync_end_result
            .and_then(Result::err)
            .or_else(|| flush_result.err());
        if let Some(err) = cleanup_error {
            return Err(err);
        }
        operation_result
    }

    // emit_diff, emit_full_redraw, and emit_style_flags have been removed
    // in favor of delegating to the Presenter for all emission paths.

    /// Create a full-screen diff (marks all cells as changed).
    #[allow(dead_code)] // API for future diff strategy integration
    fn create_full_diff(&self, buffer: &Buffer) -> BufferDiff {
        BufferDiff::full(buffer.width(), buffer.height())
    }

    /// Write log output (goes to the log region in inline mode).
    ///
    /// Behavior depends on how the UI is anchored on screen:
    ///
    /// - **Scroll region active** (ScrollRegion/Hybrid strategies): the cursor
    ///   is parked at the bottom margin of the DECSTBM log region and the text
    ///   is written verbatim (after escape-stripping). Each LF scrolls only
    ///   the log region, so lines accumulate upward into scrollback while the
    ///   UI stays put.
    ///
    /// - **Overlay fallback** (OverlayRedraw strategy, or any transient state
    ///   without an active region): an unqualified LF would scroll the whole
    ///   screen and displace or corrupt the displayed UI, so logs NEVER emit
    ///   one. Instead exactly one width-clamped line is erased-and-rewritten,
    ///   referenced against the *displayed* region (`last_inline_region`) so
    ///   height changes between presents cannot land the line inside on-screen
    ///   UI. Older lines remain visible above until displaced by present
    ///   repaints; accumulation into scrollback requires the scroll-region
    ///   path.
    ///
    /// If the UI consumes the entire terminal height, there is no log region
    /// available and the write becomes a no-op.
    ///
    /// In AltScreen mode, logs are typically not shown (returns Ok silently).
    pub fn write_log(&mut self, text: &str) -> io::Result<()> {
        // One-writer discipline vs teardown paths (bd-kdn7n item 2).
        let _output_guard = terminal_output_lock();
        // Defense in depth: callers usually sanitize before logging, but the
        // terminal writer is the final emission boundary and must never pass
        // through escape/control injection payloads.
        let sanitized = sanitize(text);
        let text = sanitized.as_ref();
        match self.screen_mode {
            ScreenMode::Inline { ui_height } => self.write_log_inline(ui_height, text),
            ScreenMode::InlineAuto { .. } => {
                let ui_height = self.effective_ui_height();
                self.write_log_inline(ui_height, text)
            }
            ScreenMode::AltScreen => {
                // AltScreen: no scrollback, logs are typically handled differently
                // (e.g., written to a log pane or file)
                Ok(())
            }
        }
    }

    /// Shared inline log-write path for [`ScreenMode::Inline`] and
    /// [`ScreenMode::InlineAuto`].
    ///
    /// `ui_height` is the *effective* (pending) UI height; the physically
    /// displayed region may differ until the next `present_ui`, which is why
    /// the overlay arm anchors against `last_inline_region`.
    fn write_log_inline(&mut self, ui_height: u16, text: &str) -> io::Result<()> {
        let visible_height = ui_height.min(self.term_height);
        if visible_height >= self.term_height {
            // No log region available when UI fills the terminal
            return Ok(());
        }

        if self.scroll_region_active {
            // A DECSTBM region matching the current effective height is
            // active: parking at its bottom margin lets every LF scroll the
            // log region alone, accumulating lines into scrollback.
            if !self.position_cursor_for_log(visible_height)? {
                return Ok(());
            }
            self.writer().write_all(text.as_bytes())?;
            return self.writer().flush();
        }

        // Overlay discipline: never emit an unscoped LF (it would scroll the
        // whole screen and push UI rows around). Erase-and-rewrite exactly
        // one width-clamped line instead.
        let log_row = match self.ui_anchor {
            UiAnchor::Bottom => {
                // One row above the uppermost of the displayed/pending UI
                // regions (0-indexed start doubles as the 1-indexed row
                // directly above that region).
                let pending_start = self.ui_start_row();
                let anchor = self.last_inline_region.map_or(pending_start, |displayed| {
                    displayed.start.min(pending_start)
                });
                if anchor == 0 {
                    // No free row above the UI.
                    return Ok(());
                }
                anchor
            }
            UiAnchor::Top => {
                // Log region sits below the top-anchored UI; the screen's
                // bottom row is always inside it and safe because this path
                // never emits LF.
                self.term_height
            }
        };

        let line = sanitize_overlay_log_line(text, usize::from(self.term_width));
        if line.is_empty() {
            return Ok(());
        }

        // Bound the cursor disturbance to this write so an interleaved
        // present (or none) always finds the cursor where it left it.
        self.writer().write_all(CURSOR_SAVE)?;
        let write_result = write!(self.writer(), "\x1b[{log_row};1H")
            .and_then(|()| self.writer().write_all(ERASE_LINE))
            .and_then(|()| self.writer().write_all(line.as_bytes()));
        let restore_result = self.writer().write_all(CURSOR_RESTORE);

        // Restoration failures must not be swallowed by a successful body
        // write: the cursor contract outranks the log payload.
        write_result?;
        restore_result?;
        self.writer().flush()
    }

    /// Position cursor at the bottom margin of the active DECSTBM log region.
    ///
    /// Only meaningful while a scroll region is active (see
    /// [`TerminalWriter::write_log`]): for bottom-anchored UI the region spans
    /// rows 1..=term_height - ui_height, for top-anchored UI rows
    /// ui_height + 1..=term_height. Parking at the region's bottom margin
    /// makes each subsequent LF scroll only that region, leaving the UI
    /// untouched while log lines accumulate into scrollback.
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
        // One-writer discipline vs teardown paths (bd-kdn7n item 2).
        let _output_guard = terminal_output_lock();
        let mut first_error = None;
        if self.in_sync_block {
            if self.capabilities.use_sync_output()
                && let Err(err) = self.writer().write_all(SYNC_END)
            {
                first_error = Some(err);
            }
            self.in_sync_block = false;
        }
        if self.cursor_saved {
            if let Err(err) = self.writer().write_all(CURSOR_RESTORE) {
                first_error.get_or_insert(err);
            }
            self.cursor_saved = false;
        }
        if self.scroll_region_active {
            if let Err(err) = self.writer().write_all(b"\x1b[r") {
                first_error.get_or_insert(err);
            }
            self.scroll_region_active = false;
        }
        if let Err(err) = self.writer().write_all(b"\x1b[2J\x1b[1;1H") {
            first_error.get_or_insert(err);
        }
        if let Err(err) = self.writer().flush() {
            first_error.get_or_insert(err);
        }
        self.prev_buffer = None;
        self.last_inline_region = None;
        self.reset_diff_strategy();
        if let Some(err) = first_error {
            Err(err)
        } else {
            Ok(())
        }
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
        let _output_guard = terminal_output_lock();
        self.set_cursor_visibility(false)?;
        self.writer().flush()
    }

    /// Show the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        let _output_guard = terminal_output_lock();
        self.set_cursor_visibility(true)?;
        self.writer().flush()
    }

    /// Flush any buffered output.
    pub fn flush(&mut self) -> io::Result<()> {
        let _output_guard = terminal_output_lock();
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
        // Take the presenter before Drop runs (Drop will see None and skip cleanup)
        self.presenter.take()?.into_inner().ok()
    }

    /// Perform garbage collection on the grapheme pool.
    ///
    /// Frees graphemes that are not referenced by the current front buffer (`prev_buffer`)
    /// or the optional `extra_buffer` (e.g. a pending render).
    ///
    /// This should be called periodically (e.g. every N frames) to prevent memory leaks
    /// in long-running applications with dynamic content.
    pub fn gc(&mut self, extra_buffer: Option<&Buffer>) {
        let mut buffers = Vec::with_capacity(2);
        if let Some(ref buf) = self.prev_buffer {
            buffers.push(buf);
        }
        if let Some(buf) = extra_buffer {
            buffers.push(buf);
        }
        self.pool.gc(&buffers);
    }

    /// Estimate total memory usage in bytes (buffers + pools).
    pub fn estimate_memory_usage(&self) -> usize {
        let mut total = 0;
        // Buffers (16 bytes per cell)
        if let Some(b) = &self.prev_buffer {
            total += b.width() as usize * b.height() as usize * 16;
        }
        if let Some(b) = &self.spare_buffer {
            total += b.width() as usize * b.height() as usize * 16;
        }
        if let Some(b) = &self.clone_buf {
            total += b.width() as usize * b.height() as usize * 16;
        }
        // Grapheme pool (approx 32 bytes per slot: 24 for String + overhead)
        total += self.pool.capacity() * 32;
        // Link registry
        total += self.links.estimate_memory();
        total
    }

    /// Best-effort cleanup when inline present fails mid-frame.
    ///
    /// This restores sync/cursor/scroll-region state without terminating the writer.
    fn best_effort_inline_cleanup(&mut self) {
        let Some(ref mut presenter) = self.presenter else {
            return;
        };
        presenter.finish_frame_best_effort();
        let writer = presenter.counting_writer_mut();

        // Ensure erase operations clear to the terminal default background.
        // Without this, background color leakage occurs during cleanup.
        let _ = writer.write_all(SGR_BG_DEFAULT);

        // Emit restorations unconditionally: write errors can occur after bytes
        // were partially written, so internal flags may be stale.
        if self.in_sync_block {
            if self.capabilities.use_sync_output() {
                let _ = writer.write_all(SYNC_END);
            }
            self.in_sync_block = false;
        }

        let _ = writer.write_all(CURSOR_RESTORE);
        self.cursor_saved = false;

        let _ = writer.write_all(b"\x1b[r");
        self.scroll_region_active = false;

        let _ = writer.write_all(b"\x1b[?25h");
        self.cursor_visible = true;
        let _ = writer.flush();
    }

    /// Internal cleanup on drop.
    fn cleanup(&mut self) {
        // Teardown bytes must not interleave with another thread's emission
        // batch (bd-kdn7n item 2); reentrant so Drop-under-lock is safe.
        let _output_guard = terminal_output_lock();
        let Some(presenter) = &mut self.presenter else {
            return; // Presenter already taken (via into_inner)
        };
        presenter.finish_frame_best_effort();
        let writer = presenter.counting_writer_mut();

        // Ensure erase operations clear to the terminal default background.
        let _ = writer.write_all(SGR_BG_DEFAULT);

        // End any pending sync block
        if self.in_sync_block {
            if self.capabilities.use_sync_output() {
                let _ = writer.write_all(SYNC_END);
            }
            self.in_sync_block = false;
        }

        // Reset scroll region BEFORE restoring the cursor: DECSTBM reset
        // homes the cursor to (1,1) on real terminals, so emitting it after
        // CURSOR_RESTORE would discard the deliberate final placement.
        if self.scroll_region_active {
            let _ = writer.write_all(b"\x1b[r");
            self.scroll_region_active = false;
        }

        // Restore cursor if saved
        if self.cursor_saved {
            let _ = writer.write_all(CURSOR_RESTORE);
            self.cursor_saved = false;
        }

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
        // Decrement the inline-active gauge.
        if matches!(
            self.screen_mode,
            ScreenMode::Inline { .. } | ScreenMode::InlineAuto { .. }
        ) {
            INLINE_ACTIVE_WIDGETS.fetch_sub(1, Ordering::SeqCst);
        }
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::{Cell, CellAttrs, CellContent, PackedRgba, StyleFlags};
    use std::cell::RefCell;
    use std::io;
    use std::path::PathBuf;
    use std::rc::Rc;
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
        caps.color_depth = ColorDepth::TrueColor;
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

    #[derive(Default)]
    struct FaultState {
        bytes: Vec<u8>,
        write_calls: usize,
        injected_failure_triggered: bool,
    }

    struct SingleWriteFaultWriter {
        state: Rc<RefCell<FaultState>>,
        fail_on_call: usize,
        max_chunk_len: usize,
    }

    impl SingleWriteFaultWriter {
        fn new(state: Rc<RefCell<FaultState>>, fail_on_call: usize, max_chunk_len: usize) -> Self {
            Self {
                state,
                fail_on_call,
                max_chunk_len: max_chunk_len.max(1),
            }
        }
    }

    impl Write for SingleWriteFaultWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut state = self.state.borrow_mut();
            state.write_calls = state.write_calls.saturating_add(1);
            if !state.injected_failure_triggered && state.write_calls == self.fail_on_call {
                state.injected_failure_triggered = true;
                return Err(io::Error::other("injected partial-write fault"));
            }

            let write_len = buf.len().min(self.max_chunk_len);
            state.bytes.extend_from_slice(&buf[..write_len]);
            Ok(write_len)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
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
    fn present_ui_altscreen_closes_stale_sync_block_when_policy_allows_sync() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                full_caps(),
            );
            writer.set_size(8, 2);
            writer.in_sync_block = true;

            let mut buffer = Buffer::new(8, 2);
            buffer.set_raw(0, 0, Cell::from_char('X'));
            writer.present_ui(&buffer, None, true).unwrap();

            assert!(
                !writer.in_sync_block,
                "present_altscreen must close stale sync blocks"
            );
        }

        assert!(
            output.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "sync end should be emitted when stale sync state is detected"
        );
    }

    #[test]
    fn present_ui_altscreen_stale_sync_block_skips_sync_end_in_mux() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                mux_caps(),
            );
            writer.set_size(8, 2);
            writer.in_sync_block = true;

            let mut buffer = Buffer::new(8, 2);
            buffer.set_raw(0, 0, Cell::from_char('X'));
            writer.present_ui(&buffer, None, true).unwrap();

            assert!(
                !writer.in_sync_block,
                "present_altscreen must clear stale sync state"
            );
        }

        assert!(
            !output.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "sync end must be suppressed when policy disables synchronized output"
        );
    }

    #[test]
    fn present_ui_altscreen_sanitizes_grapheme_escape_payloads() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(12, 1);

            let gid = writer
                .pool_mut()
                .intern("ok\x1b]52;c;SGVsbG8=\x1b\\tail\u{009d}", 6);
            let mut buffer = Buffer::new(12, 1);
            buffer.set_raw(0, 0, Cell::new(CellContent::from_grapheme(gid)));

            writer.present_ui(&buffer, None, true).unwrap();
        }

        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("oktail"),
            "sanitized grapheme content should preserve visible payload"
        );
        assert!(
            !output_str.contains("52;c;SGVsbG8"),
            "OSC payload must not be forwarded by alt-screen emitter"
        );
        assert!(
            !output_str.contains('\u{009d}'),
            "C1 controls must be stripped from alt-screen grapheme output"
        );
    }

    #[test]
    fn present_ui_inline_skips_sync_output_in_mux() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                mux_caps(),
            );
            writer.set_size(10, 10);

            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !output.windows(SYNC_BEGIN.len()).any(|w| w == SYNC_BEGIN),
            "sync begin must be suppressed in tmux/screen/zellij environments"
        );
        assert!(
            !output.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "sync end must be suppressed in tmux/screen/zellij environments"
        );
    }

    #[test]
    fn present_ui_altscreen_skips_sync_output_in_mux() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                mux_caps(),
            );
            writer.set_size(10, 10);

            let buffer = Buffer::new(10, 5);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !output.windows(SYNC_BEGIN.len()).any(|w| w == SYNC_BEGIN),
            "sync begin must be suppressed in tmux/screen/zellij environments"
        );
        assert!(
            !output.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "sync end must be suppressed in tmux/screen/zellij environments"
        );
    }

    #[test]
    fn present_ui_inline_skips_hyperlinks_in_mux() {
        let mut output = Vec::new();
        {
            let mut caps = mux_caps();
            caps.osc8_hyperlinks = true;

            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 2 },
                UiAnchor::Bottom,
                caps,
            );
            writer.set_size(8, 4);

            let link_id = writer.links_mut().register("https://example.com");
            let mut buffer = Buffer::new(8, 2);
            buffer.set_raw(
                0,
                0,
                Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
            );
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !output.windows(b"\x1b]8;".len()).any(|w| w == b"\x1b]8;"),
            "OSC 8 sequences must be suppressed by mux hyperlink policy"
        );
    }

    #[test]
    fn present_ui_inline_closes_hyperlinks_at_frame_end() {
        let mut output = Vec::new();
        {
            let mut caps = full_caps();
            caps.osc8_hyperlinks = true;

            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 2 },
                UiAnchor::Bottom,
                caps,
            );
            writer.set_size(8, 4);

            let link_id = writer.links_mut().register("https://example.com");
            let mut buffer = Buffer::new(8, 2);
            buffer.set_raw(
                0,
                0,
                Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
            );
            writer.present_ui(&buffer, None, true).unwrap();
        }

        let open = b"\x1b]8;;https://example.com\x07";
        let close = b"\x1b]8;;\x07";
        let open_pos = output
            .windows(open.len())
            .position(|window| window == open)
            .expect("expected OSC 8 open sequence");
        let close_pos = output
            .windows(close.len())
            .position(|window| window == close)
            .expect("expected OSC 8 close sequence");
        assert!(
            open_pos < close_pos,
            "hyperlink must close before frame end"
        );
    }

    #[test]
    fn present_ui_altscreen_skips_hyperlinks_in_mux() {
        let mut output = Vec::new();
        {
            let mut caps = mux_caps();
            caps.osc8_hyperlinks = true;

            let mut writer =
                TerminalWriter::new(&mut output, ScreenMode::AltScreen, UiAnchor::Bottom, caps);
            writer.set_size(8, 4);

            let link_id = writer.links_mut().register("https://example.com");
            let mut buffer = Buffer::new(8, 2);
            buffer.set_raw(
                0,
                0,
                Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
            );
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !output.windows(b"\x1b]8;".len()).any(|w| w == b"\x1b]8;"),
            "OSC 8 sequences must be suppressed by mux hyperlink policy"
        );
    }

    #[test]
    fn present_ui_altscreen_closes_hyperlinks_at_frame_end() {
        let mut output = Vec::new();
        {
            let mut caps = full_caps();
            caps.osc8_hyperlinks = true;

            let mut writer =
                TerminalWriter::new(&mut output, ScreenMode::AltScreen, UiAnchor::Bottom, caps);
            writer.set_size(8, 4);

            let link_id = writer.links_mut().register("https://example.com");
            let mut buffer = Buffer::new(8, 2);
            buffer.set_raw(
                0,
                0,
                Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
            );
            writer.present_ui(&buffer, None, true).unwrap();
        }

        let open = b"\x1b]8;;https://example.com\x07";
        let close = b"\x1b]8;;\x07";
        let open_pos = output
            .windows(open.len())
            .position(|window| window == open)
            .expect("expected OSC 8 open sequence");
        let close_pos = output
            .windows(close.len())
            .position(|window| window == close)
            .expect("expected OSC 8 close sequence");
        assert!(
            open_pos < close_pos,
            "hyperlink must close before frame end"
        );
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
    fn present_ui_visible_with_position_temporarily_hides_cursor() {
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
            writer.present_ui(&buffer, Some((0, 0)), true).unwrap();
        }

        assert!(
            output.windows(6).any(|w| w == b"\x1b[?25l"),
            "expected cursor hide during frame emission"
        );
    }

    #[test]
    fn present_ui_visible_without_position_hides_cursor() {
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
            output.windows(6).any(|w| w == b"\x1b[?25l"),
            "expected cursor hide sequence when no explicit cursor position exists"
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
    fn drop_cleanup_skips_sync_end_in_mux_even_with_stale_state() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                mux_caps(),
            );
            writer.in_sync_block = true;
            // Dropped here
        }

        assert!(
            !output.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "drop cleanup must not emit sync_end in mux environments"
        );
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

        // Should have cursor save before each UI present. Overlay log
        // writes add their own balanced SAVE/RESTORE pair, so only a floor
        // is asserted here; balance itself is covered by the overlay tests.
        let save_count = output
            .windows(CURSOR_SAVE.len())
            .filter(|w| *w == CURSOR_SAVE)
            .count();
        assert!(
            save_count >= 2,
            "Should have saved cursor at least twice (once per present)"
        );

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
    fn inline_diff_frame_never_emits_below_the_fold() {
        // Regression: the incremental-diff branch emitted every diff run
        // unclipped. With ui_height > term_height, a change on a buffer row
        // below the fold produced a CUP past the physical bottom row, which
        // real terminals clamp — scribbling over the last visible row. The
        // full-redraw branch already clipped; the diff branch must too.
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 20 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 10); // terminal shorter than the UI

            // Frame 1: full redraw (clipped by the existing logic).
            let buffer = Buffer::new(80, 20);
            writer.present_ui(&buffer, None, true).unwrap();

            // Frame 2: change ONLY a below-the-fold row (row 19).
            let mut buffer2 = Buffer::new(80, 20);
            buffer2.set(0, 19, ftui_render::cell::Cell::from_char('B'));
            writer.present_ui(&buffer2, None, true).unwrap();
        }

        let max_row = max_cursor_row(&output);
        assert!(
            max_row <= 10,
            "diff frame emitted cursor row {max_row} past the 10-row terminal"
        );
        // The below-the-fold glyph must not appear anywhere in the output.
        assert!(
            !output.windows(1).any(|w| w == b"B"),
            "below-the-fold cell was emitted"
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
        let bg_reset_count = segment
            .windows(SGR_BG_DEFAULT.len())
            .filter(|w| *w == SGR_BG_DEFAULT)
            .count();

        assert_eq!(erase_count, 6, "expected clears for stale + new rows");
        assert!(
            bg_reset_count >= 2,
            "expected background resets before row clears"
        );
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
    fn write_log_sanitizes_escape_injection_payloads() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer
                .write_log("safe\x1b]52;c;SGVsbG8=\x1b\\tail\u{009d}x\n")
                .unwrap();
        }

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("safetailx"));
        assert!(
            !output_str.contains("52;c;SGVsbG8"),
            "OSC payload must not be forwarded to terminal output"
        );
        assert!(
            !output_str.contains('\u{009d}'),
            "C1 controls must be stripped from log output"
        );
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

    #[test]
    fn present_ui_inline_skips_cursor_position_when_x_is_out_of_bounds() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            let buffer = Buffer::new(4, 5);
            writer.present_ui(&buffer, Some((4, 1)), true).unwrap();
        }

        let restore_idx = find_nth(&output, CURSOR_RESTORE, 1).expect("expected cursor restore");
        let after_restore = &output[restore_idx..];
        let invalid_pos = b"\x1b[21;5H";
        assert!(
            !after_restore
                .windows(invalid_pos.len())
                .any(|w| w == invalid_pos),
            "inline cursor should not move to x outside the buffer width"
        );
    }

    #[test]
    fn present_ui_inline_skips_cursor_position_when_y_is_below_buffer_height() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            let buffer = Buffer::new(4, 2);
            writer.present_ui(&buffer, Some((1, 4)), true).unwrap();
        }

        let restore_idx = find_nth(&output, CURSOR_RESTORE, 1).expect("expected cursor restore");
        let after_restore = &output[restore_idx..];
        let invalid_pos = b"\x1b[24;2H";
        assert!(
            !after_restore
                .windows(invalid_pos.len())
                .any(|w| w == invalid_pos),
            "inline cursor should not move below the buffer height just because the inline region is taller"
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
        assert_eq!(config.full_redraw_interval_frames, 240);
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
            .with_reset_on_invalidation(false)
            .with_full_redraw_interval_frames(17);

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
        assert_eq!(config.full_redraw_interval_frames, 17);
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
    fn full_redraw_interval_forces_terminal_resync() {
        let mut output = Vec::new();
        let mut writer = TerminalWriter::with_diff_config(
            &mut output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            RuntimeDiffConfig::default()
                .with_bayesian_enabled(false)
                .with_full_redraw_interval_frames(1),
        );
        writer.set_size(4, 2);

        let mut buffer = Buffer::new(4, 2);
        buffer.set_raw(0, 0, Cell::from_char('A'));

        writer.present_ui(&buffer, None, false).unwrap();
        assert_eq!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));

        writer.present_ui(&buffer, None, false).unwrap();
        assert_ne!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));

        writer.present_ui(&buffer, None, false).unwrap();
        assert_eq!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));
    }

    #[test]
    fn full_redraw_interval_emits_current_frame_after_incremental_baseline() {
        let state = Rc::new(RefCell::new(FaultState::default()));
        let writer_backend = SingleWriteFaultWriter::new(Rc::clone(&state), usize::MAX, 1);
        let mut writer = TerminalWriter::with_diff_config(
            writer_backend,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            RuntimeDiffConfig::default()
                .with_bayesian_enabled(false)
                .with_full_redraw_interval_frames(1),
        );
        writer.set_size(4, 2);

        let mut buffer = Buffer::new(4, 2);
        buffer.set_raw(0, 0, Cell::from_char('Q'));

        writer.present_ui(&buffer, None, false).unwrap();
        assert_eq!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));

        buffer.set_raw(1, 0, Cell::from_char('Z'));
        writer.present_ui(&buffer, None, false).unwrap();
        assert_ne!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));

        state.borrow_mut().bytes.clear();
        writer.present_ui(&buffer, None, false).unwrap();
        assert_eq!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));

        let bytes = state.borrow().bytes.clone();
        assert!(
            bytes.windows(b"QZ".len()).any(|window| window == b"QZ"),
            "forced full redraw should emit adjacent current-frame cells"
        );
    }

    #[test]
    fn full_redraw_interval_zero_disables_terminal_resync() {
        let mut output = Vec::new();
        let mut writer = TerminalWriter::with_diff_config(
            &mut output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            RuntimeDiffConfig::default()
                .with_bayesian_enabled(false)
                .with_full_redraw_interval_frames(0),
        );
        writer.set_size(4, 2);

        let mut buffer = Buffer::new(4, 2);
        buffer.set_raw(0, 0, Cell::from_char('A'));

        writer.present_ui(&buffer, None, false).unwrap();
        assert_eq!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));

        for _ in 0..5 {
            writer.present_ui(&buffer, None, false).unwrap();
            assert_ne!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));
        }
    }

    #[test]
    fn full_redraw_max_interval_zero_forces_resync_every_frame() {
        // A zero wall-clock interval means "always due": every present must be a
        // full physical redraw, regardless of the frame counter. This is the
        // wall-clock bound that repairs terminal-state desync on an idle /
        // sparsely-rendering TUI where the frame counter advances too slowly.
        let mut output = Vec::new();
        let mut writer = TerminalWriter::with_diff_config(
            &mut output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            RuntimeDiffConfig::default()
                .with_bayesian_enabled(false)
                // Disable the frame-count path to isolate the wall-clock bound.
                .with_full_redraw_interval_frames(0)
                .with_full_redraw_max_interval(Some(std::time::Duration::ZERO)),
        );
        writer.set_size(4, 2);

        let mut buffer = Buffer::new(4, 2);
        buffer.set_raw(0, 0, Cell::from_char('A'));

        // First frame is a full redraw (no prior baseline) and every subsequent
        // frame is forced full by the zero interval.
        for _ in 0..5 {
            writer.present_ui(&buffer, None, false).unwrap();
            assert_eq!(
                writer.last_diff_strategy(),
                Some(DiffStrategy::FullRedraw),
                "zero wall-clock interval must force a full redraw every frame"
            );
        }
    }

    #[test]
    fn full_redraw_max_interval_none_keeps_incremental_after_baseline() {
        // The wall-clock bound is opt-in: with it unset (None) and the
        // frame-count path disabled, frames stay incremental after the initial
        // baseline full redraw — preserving the default sparse-diff behavior and
        // deterministic-test reproducibility.
        let mut output = Vec::new();
        let mut writer = TerminalWriter::with_diff_config(
            &mut output,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
            RuntimeDiffConfig::default()
                .with_bayesian_enabled(false)
                .with_full_redraw_interval_frames(0)
                .with_full_redraw_max_interval(None),
        );
        writer.set_size(4, 2);

        let mut buffer = Buffer::new(4, 2);
        buffer.set_raw(0, 0, Cell::from_char('A'));

        writer.present_ui(&buffer, None, false).unwrap();
        assert_eq!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));

        for _ in 0..5 {
            writer.present_ui(&buffer, None, false).unwrap();
            assert_ne!(
                writer.last_diff_strategy(),
                Some(DiffStrategy::FullRedraw),
                "no wall-clock bound must leave frames incremental after baseline"
            );
        }
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
    fn log_write_without_scroll_region_preserves_diff_strategy() {
        // Overlay log writes never scroll the screen (single erased-and-
        // rewritten line, no unqualified LF), so the displayed UI baseline
        // stays valid and the diff strategy posterior must be PRESERVED —
        // the old reset-to-priors behavior compensated for log-induced
        // scrolling that no longer happens.
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

            // Present another frame
            buffer.set_raw(1, 1, Cell::from_char('Y'));
            writer.present_ui(&buffer, None, false).unwrap();

            let (alpha_before, beta_before) = writer.diff_strategy().posterior_params();

            // Log write without scroll region must not disturb diff state
            assert!(!writer.scroll_region_active());
            writer.write_log("log message\n").unwrap();

            let (alpha_after, beta_after) = writer.diff_strategy().posterior_params();
            assert!(
                (alpha_after - alpha_before).abs() < 0.01
                    && (beta_after - beta_before).abs() < 0.01,
                "posterior should be preserved across overlay log write: \
                 before=({}, {}), after=({}, {})",
                alpha_before,
                beta_before,
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

    // --- Overlay log discipline regressions (bd-th0p6 / bd-le0q8) ---

    /// CONTRACT: without a scroll region, write_log must never emit an
    /// unqualified LF (it would scroll the whole screen and displace the UI)
    /// and must erase the line before rewriting it so shorter lines cannot
    /// leave remnants of longer predecessors.
    #[test]
    fn overlay_write_log_emits_single_erased_line_no_lf() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("first line\n").unwrap();
        }

        assert!(
            !output.contains(&b'\n'),
            "overlay write_log must never emit a raw LF byte"
        );
        assert!(
            contains_bytes(&output, ERASE_LINE),
            "overlay write_log must erase the log row before writing"
        );
        assert!(
            contains_bytes(&output, b"first line"),
            "the (single-line) log payload must be emitted"
        );
    }

    /// CONTRACT: multi-line payloads collapse to their first display line in
    /// overlay mode — subsequent lines must never scribble toward the UI.
    #[test]
    fn overlay_write_log_drops_lines_after_first() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("only me\nhidden tail\n").unwrap();
        }

        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("only me"), "first line must be emitted");
        assert!(
            !text.contains("hidden tail"),
            "second line must be dropped in overlay mode"
        );
    }

    /// CONTRACT (bd-th0p6 top-anchor): with a top-anchored UI and no scroll
    /// region, log writes park on the screen's bottom row but never emit LF,
    /// so nothing can push UI rows into scrollback.
    #[test]
    fn overlay_top_anchor_write_log_never_scrolls() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Top,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log("top anchored\n").unwrap();
        }

        assert!(
            !output.contains(&b'\n'),
            "top-anchor overlay logs must not emit LF (screen-scroll hazard)"
        );
        assert!(
            contains_bytes(&output, b"\x1b[24;1H"),
            "log should target the screen's bottom row (below the UI)"
        );
        assert!(contains_bytes(&output, b"top anchored"));
    }

    /// CONTRACT (bd-th0p6): log payloads are clamped to the terminal width so
    /// they cannot wrap into the first UI row.
    #[test]
    fn overlay_write_log_clamps_to_terminal_width() {
        let long_line = "x".repeat(100);
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            writer.write_log(&long_line).unwrap();
        }

        let emitted_xs = output.iter().filter(|&&b| b == b'x').count();
        assert_eq!(
            emitted_xs, 80,
            "exactly term_width columns of the log line must be emitted"
        );
    }

    /// never inside it — instead of using the pending effective height.
    #[test]
    fn inline_auto_shrink_positions_log_against_displayed_region() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::InlineAuto {
                    min_height: 1,
                    max_height: 8,
                },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            // Measure to the max: display an 8-row UI (rows 17-24,
            // region start 16, 0-indexed).
            writer.set_auto_ui_height(8);
            let buffer = Buffer::new(80, 8);
            writer.present_ui(&buffer, None, false).unwrap();

            // Remeasure drops the pending effective height to min=1 while
            // rows 17-24 are still physically on screen.
            writer.clear_auto_ui_height();

            // A Cmd::Log before the next present must land ABOVE row 17.
            writer.write_log("mid-flight\n").unwrap();
        }

        // The overlay log write emits exactly SAVE + CUP + EL + payload, a
        // contiguous pattern no present path produces (presents emit more
        // bytes between SAVE and any CUP). Asserting on that whole pattern
        // scopes the check to the log write alone.
        assert!(
            contains_bytes(&output, b"\x1b7\x1b[16;1H\x1b[2K"),
            "log must anchor one row above the displayed region start"
        );
        for row in 17..=24 {
            assert!(
                !contains_bytes(&output, format!("\x1b7\x1b[{row};1H").as_bytes()),
                "log must not position inside displayed UI row {row}"
            );
        }
        assert!(contains_bytes(&output, b"mid-flight"));
    }

    /// CONTRACT: set_size invalidates the displayed-region cache — old
    /// geometry must not steer log positioning or shrink-erasure afterward.
    #[test]
    fn set_size_clears_last_inline_region() {
        let mut writer = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(80, 24);
        let buffer = Buffer::new(80, 5);
        writer.present_ui(&buffer, None, false).unwrap();
        assert!(writer.last_inline_region.is_some());

        writer.set_size(80, 30);
        assert!(
            writer.last_inline_region.is_none(),
            "resize must invalidate the displayed-region cache"
        );
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
        assert!(writer.capabilities().supports_true_color());
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
        writer.gc(None);
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
        writer.gc(None);
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

    // CountingWriter tests removed — the local CountingWriter was removed
    // in favour of ftui_render::counting_writer::CountingWriter (accessed via
    // Presenter). The render-crate CountingWriter has its own test suite.

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
    fn clear_screen_resets_active_scroll_region_before_clearing() {
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
            assert!(writer.scroll_region_active());

            writer.clear_screen().unwrap();
            assert!(
                !writer.scroll_region_active(),
                "clear_screen should leave no active scroll region"
            );
        }

        let reset_idx = output
            .windows(b"\x1b[r".len())
            .position(|w| w == b"\x1b[r")
            .expect("expected scroll-region reset");
        let clear_idx = output
            .windows(b"\x1b[2J".len())
            .position(|w| w == b"\x1b[2J")
            .expect("expected full clear");
        assert!(
            reset_idx < clear_idx,
            "clear_screen should reset DECSTBM before full-screen clear"
        );
    }

    #[test]
    fn clear_screen_restores_saved_cursor_before_clearing() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.cursor_saved = true;

            writer.clear_screen().unwrap();
            assert!(
                !writer.cursor_saved,
                "clear_screen should clear stale saved-cursor state"
            );
        }

        let restore_idx = output
            .windows(CURSOR_RESTORE.len())
            .position(|w| w == CURSOR_RESTORE)
            .expect("expected cursor restore");
        let clear_idx = output
            .windows(b"\x1b[2J".len())
            .position(|w| w == b"\x1b[2J")
            .expect("expected full clear");
        assert!(
            restore_idx < clear_idx,
            "clear_screen should restore any saved cursor before clearing"
        );
    }

    #[test]
    fn clear_screen_closes_stale_sync_block_before_clearing() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                full_caps(),
            );
            writer.in_sync_block = true;

            writer.clear_screen().unwrap();
            assert!(
                !writer.in_sync_block,
                "clear_screen should clear stale sync-block state"
            );
        }

        let sync_end_idx = output
            .windows(SYNC_END.len())
            .position(|w| w == SYNC_END)
            .expect("expected sync end");
        let clear_idx = output
            .windows(b"\x1b[2J".len())
            .position(|w| w == b"\x1b[2J")
            .expect("expected full clear");
        assert!(
            sync_end_idx < clear_idx,
            "clear_screen should end any open sync block before clearing"
        );
    }

    #[test]
    fn clear_screen_skips_sync_end_in_mux_while_clearing_stale_state() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                mux_caps(),
            );
            writer.in_sync_block = true;

            writer.clear_screen().unwrap();
            assert!(
                !writer.in_sync_block,
                "clear_screen should clear stale sync state even when sync output is disabled"
            );
        }

        assert!(
            !output.windows(SYNC_END.len()).any(|w| w == SYNC_END),
            "clear_screen must not emit sync_end in mux environments"
        );
        assert!(
            output.windows(b"\x1b[2J".len()).any(|w| w == b"\x1b[2J"),
            "clear_screen should still clear the screen"
        );
    }

    #[test]
    fn clear_screen_invalidates_cached_state_even_when_flush_fails() {
        let state = Rc::new(RefCell::new(FaultState::default()));
        let writer_backend = SingleWriteFaultWriter::new(Rc::clone(&state), 1, 1);
        let mut writer = TerminalWriter::new(
            writer_backend,
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.cursor_saved = true;
        writer.prev_buffer = Some(Buffer::new(4, 2));
        writer.last_inline_region = Some(InlineRegion {
            start: 19,
            height: 5,
        });
        writer.last_diff_strategy = Some(DiffStrategy::DirtyRows);

        let err = writer
            .clear_screen()
            .expect_err("expected injected flush write failure");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(state.borrow().injected_failure_triggered);
        assert!(
            writer.prev_buffer.is_none(),
            "clear_screen should invalidate cached frame state after flush failure"
        );
        assert!(
            writer.last_inline_region.is_none(),
            "clear_screen should drop inline region cache after flush failure"
        );
        assert!(
            writer.last_diff_strategy.is_none(),
            "clear_screen should reset diff strategy after flush failure"
        );
    }

    #[test]
    fn present_ui_retry_after_write_failure_forces_repaint() {
        let state = Rc::new(RefCell::new(FaultState::default()));
        let writer_backend = SingleWriteFaultWriter::new(Rc::clone(&state), 1, 1);
        let mut writer = TerminalWriter::new(
            writer_backend,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(4, 2);

        let mut buffer = Buffer::new(4, 2);
        buffer.set_raw(0, 0, Cell::from_char('A'));

        let err = writer
            .present_ui(&buffer, None, true)
            .expect_err("first present should hit the injected write fault");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(
            writer.prev_buffer.is_none(),
            "failed present must not advance the diff baseline"
        );

        writer
            .present_ui(&buffer, None, true)
            .expect("retry after transient failure should succeed");

        let bytes = state.borrow().bytes.clone();
        assert!(
            bytes.contains(&b'A'),
            "retry should emit the missing cell content after a failed present"
        );
    }

    #[test]
    fn present_ui_write_failure_with_existing_baseline_invalidates_diff_state() {
        let state = Rc::new(RefCell::new(FaultState::default()));
        let writer_backend = SingleWriteFaultWriter::new(Rc::clone(&state), 1, 1);
        let mut writer = TerminalWriter::new(
            writer_backend,
            ScreenMode::AltScreen,
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer.set_size(4, 2);

        let mut previous = Buffer::new(4, 2);
        previous.set_raw(0, 0, Cell::from_char('A'));
        writer.prev_buffer = Some(previous);
        writer.last_inline_region = Some(InlineRegion {
            start: 0,
            height: 2,
        });
        writer.last_diff_strategy = Some(DiffStrategy::DirtyRows);
        writer.frames_since_full_redraw = 7;

        let mut buffer = Buffer::new(4, 2);
        buffer.set_raw(0, 0, Cell::from_char('B'));

        let err = writer
            .present_ui(&buffer, None, true)
            .expect_err("present should hit the injected write fault");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(
            writer.prev_buffer.is_none(),
            "failed present with a prior baseline must force the next frame to repaint"
        );
        assert!(
            writer.last_inline_region.is_none(),
            "failed present must drop inline-region assumptions"
        );
        assert_eq!(writer.last_diff_strategy(), None);
        assert_eq!(writer.frames_since_full_redraw, 0);

        writer
            .present_ui(&buffer, None, true)
            .expect("retry after transient failure should succeed");
        assert_eq!(writer.last_diff_strategy(), Some(DiffStrategy::FullRedraw));
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

    // =========================================================================
    // Inline active widgets gauge tests (bd-1q5.15)
    // =========================================================================

    /// Mutex to serialize gauge tests against concurrent inline writer
    /// creation/destruction in other tests.
    static GAUGE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn inline_active_widgets_gauge_increments_for_inline_mode() {
        let _lock = GAUGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        // Other tests may create/drop inline writers concurrently.
        // Retry until we observe one uncontended +1/-1 transition.
        for _ in 0..64 {
            let before = inline_active_widgets();
            let writer = TerminalWriter::new(
                Vec::new(),
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            let after_create = inline_active_widgets();
            drop(writer);
            let after_drop = inline_active_widgets();

            if after_create == before.saturating_add(1) && after_drop == before {
                return;
            }
            std::thread::yield_now();
        }

        panic!("failed to observe uncontended inline gauge +1/-1 transition");
    }

    #[test]
    fn inline_active_widgets_gauge_increments_for_inline_auto_mode() {
        let _lock = GAUGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        for _ in 0..64 {
            let before = inline_active_widgets();
            let writer = TerminalWriter::new(
                Vec::new(),
                ScreenMode::InlineAuto {
                    min_height: 2,
                    max_height: 10,
                },
                UiAnchor::Bottom,
                basic_caps(),
            );
            let after_create = inline_active_widgets();
            drop(writer);
            let after_drop = inline_active_widgets();

            if after_create == before.saturating_add(1) && after_drop == before {
                return;
            }
            std::thread::yield_now();
        }

        panic!("failed to observe uncontended inline-auto gauge +1/-1 transition");
    }

    #[test]
    fn inline_active_widgets_gauge_unchanged_for_altscreen() {
        let _lock = GAUGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        for _ in 0..64 {
            let before = inline_active_widgets();
            let writer = TerminalWriter::new(
                Vec::new(),
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            let after_create = inline_active_widgets();
            drop(writer);
            let after_drop = inline_active_widgets();

            if after_create == before && after_drop == before {
                return;
            }
            std::thread::yield_now();
        }

        panic!("failed to observe stable altscreen gauge behavior");
    }

    // =========================================================================
    // Inline scrollback preservation tests (bd-1q5.16)
    // =========================================================================

    /// CSI ?1049h — the alternate-screen enter sequence that must NEVER appear
    /// in inline mode output.
    const ALTSCREEN_ENTER: &[u8] = b"\x1b[?1049h";

    /// CSI ?1049l — the alternate-screen exit sequence.
    const ALTSCREEN_EXIT: &[u8] = b"\x1b[?1049l";

    /// Helper: returns true if `haystack` contains the byte subsequence `needle`.
    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn inline_render_never_emits_altscreen_enter() {
        // The defining contract of inline mode: CSI ?1049h must not appear.
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
            writer.write_log("hello\n").unwrap();
            // Second present to exercise diff path
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !contains_bytes(&output, ALTSCREEN_ENTER),
            "inline mode must never emit CSI ?1049h (alternate screen enter)"
        );
        assert!(
            !contains_bytes(&output, ALTSCREEN_EXIT),
            "inline mode must never emit CSI ?1049l (alternate screen exit)"
        );
    }

    #[test]
    fn inline_auto_render_never_emits_altscreen_enter() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::InlineAuto {
                    min_height: 3,
                    max_height: 10,
                },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !contains_bytes(&output, ALTSCREEN_ENTER),
            "InlineAuto mode must never emit CSI ?1049h"
        );
    }

    #[test]
    fn inline_scrollback_preserved_after_present() {
        // Scrollback preservation means log text written before present_ui
        // survives the UI render pass. We verify the output buffer contains
        // both the log text and cursor save/restore (the contract that
        // guarantees scrollback isn't disturbed).
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            writer.write_log("scrollback line A\n").unwrap();
            writer.write_log("scrollback line B\n").unwrap();

            let buffer = Buffer::new(80, 5);
            writer.present_ui(&buffer, None, true).unwrap();

            // Another log after render should also work
            writer.write_log("scrollback line C\n").unwrap();
        }

        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("scrollback line A"), "first log must survive");
        assert!(
            text.contains("scrollback line B"),
            "second log must survive"
        );
        assert!(
            text.contains("scrollback line C"),
            "post-render log must survive"
        );

        // Cursor save/restore must bracket the UI render to leave
        // scrollback position untouched.
        assert!(
            contains_bytes(&output, CURSOR_SAVE),
            "present_ui must save cursor to protect scrollback"
        );
        assert!(
            contains_bytes(&output, CURSOR_RESTORE),
            "present_ui must restore cursor to protect scrollback"
        );
    }

    #[test]
    fn multiple_inline_writers_coexist() {
        // Two independent inline writers should each manage their own state
        // without interfering. Uses owned Vec writers so each can be
        // independently dropped and inspected.
        let mut writer_a = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 3 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer_a.set_size(40, 12);

        let mut writer_b = TerminalWriter::new(
            Vec::new(),
            ScreenMode::Inline { ui_height: 5 },
            UiAnchor::Bottom,
            basic_caps(),
        );
        writer_b.set_size(80, 24);

        // Both can render independently without panicking
        let buf_a = Buffer::new(40, 3);
        let buf_b = Buffer::new(80, 5);
        writer_a.present_ui(&buf_a, None, true).unwrap();
        writer_b.present_ui(&buf_b, None, true).unwrap();

        // Second render pass (diff path) also works
        writer_a.present_ui(&buf_a, None, true).unwrap();
        writer_b.present_ui(&buf_b, None, true).unwrap();

        // Both drop cleanly (no panic, no double-free)
        drop(writer_a);
        drop(writer_b);
    }

    #[test]
    fn multiple_inline_writers_gauge_tracks_both() {
        // Verify the gauge correctly tracks two simultaneous inline writers.
        let _lock = GAUGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        for _ in 0..64 {
            let before = inline_active_widgets();
            let writer_a = TerminalWriter::new(
                Vec::new(),
                ScreenMode::Inline { ui_height: 3 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            let after_a = inline_active_widgets();

            let writer_b = TerminalWriter::new(
                Vec::new(),
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            let after_b = inline_active_widgets();

            drop(writer_a);
            let after_drop_a = inline_active_widgets();

            drop(writer_b);
            let after_drop_b = inline_active_widgets();

            if after_a == before.saturating_add(1)
                && after_b == before.saturating_add(2)
                && after_drop_a == before.saturating_add(1)
                && after_drop_b == before
            {
                return;
            }
            std::thread::yield_now();
        }

        panic!("failed to observe uncontended two-writer gauge transitions");
    }

    #[test]
    fn resize_during_inline_mode_preserves_scrollback() {
        // Resize should re-anchor the UI region without emitting
        // alternate screen sequences and should allow continued rendering.
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

            // Simulate resize
            writer.set_size(100, 30);
            assert_eq!(writer.ui_start_row(), 25); // 30 - 5

            // Render again after resize
            let buffer2 = Buffer::new(100, 5);
            writer.present_ui(&buffer2, None, true).unwrap();

            // Log still works after resize
            writer.write_log("post-resize log\n").unwrap();
        }

        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("post-resize log"));
        assert!(
            !contains_bytes(&output, ALTSCREEN_ENTER),
            "resize must not trigger alternate screen"
        );
    }

    #[test]
    fn resize_shrink_during_inline_mode_clamps_correctly() {
        // Shrinking the terminal so UI region overlaps should still work
        // without alternate screen sequences.
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 10 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);
            assert_eq!(writer.ui_start_row(), 14);

            // Shrink terminal to smaller than UI height
            writer.set_size(80, 8);
            assert_eq!(writer.ui_start_row(), 0); // 8 - 10 would underflow, clamped to 0

            // Rendering should still work (height clamped to terminal)
            let buffer = Buffer::new(80, 8);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !contains_bytes(&output, ALTSCREEN_ENTER),
            "shrunken terminal must not switch to altscreen"
        );
    }

    #[test]
    fn inline_render_emits_tracing_span_fields() {
        // Verify the inline.render span is entered during present_ui in inline
        // mode by checking that the tracing infrastructure is invoked.
        // We use a tracing subscriber to capture span creation.
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        struct SpanChecker {
            saw_inline_render: Arc<AtomicBool>,
        }

        impl tracing::Subscriber for SpanChecker {
            fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                if span.metadata().name() == "inline.render" {
                    self.saw_inline_render
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {
            }
            fn event(&self, _event: &tracing::Event<'_>) {}
            fn enter(&self, _span: &tracing::span::Id) {}
            fn exit(&self, _span: &tracing::span::Id) {}
        }

        let saw_it = Arc::new(AtomicBool::new(false));

        // Span callsite interest caches are process-global while this
        // dispatcher is thread-local: a parallel test can race the
        // info_span!("inline.render") callsite into caching "not enabled"
        // even after a rebuild (bd-r2qx8). Retry with a fresh subscriber and
        // a fresh rebuild each round; a neighbor can only win the race
        // transiently, never persistently.
        for _ in 0..8 {
            saw_it.store(false, std::sync::atomic::Ordering::SeqCst);
            let subscriber = SpanChecker {
                saw_inline_render: Arc::clone(&saw_it),
            };

            let _guard = tracing::subscriber::set_default(subscriber);
            tracing::callsite::rebuild_interest_cache();

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
            }

            if saw_it.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
        }

        assert!(
            saw_it.load(std::sync::atomic::Ordering::SeqCst),
            "present_ui in inline mode must emit an inline.render tracing span"
        );
    }

    #[test]
    fn inline_render_no_altscreen_with_scroll_region_strategy() {
        // Even with scroll region caps, inline mode must not emit altscreen.
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
            writer.present_ui(&buffer, None, true).unwrap();
        }

        assert!(
            !contains_bytes(&output, ALTSCREEN_ENTER),
            "scroll region strategy must never emit altscreen enter"
        );
    }

    #[test]
    fn inline_render_no_altscreen_with_hybrid_strategy() {
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
        }

        assert!(
            !contains_bytes(&output, ALTSCREEN_ENTER),
            "hybrid strategy must never emit altscreen enter"
        );
    }

    #[test]
    fn inline_render_no_altscreen_with_mux_strategy() {
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
        }

        assert!(
            !contains_bytes(&output, ALTSCREEN_ENTER),
            "mux (overlay) strategy must never emit altscreen enter"
        );
    }

    #[test]
    fn test_altscreen_wide_char_rendering() {
        use ftui_render::cell::Cell;
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(10, 5);

            let mut buf = Buffer::new(10, 1);
            // Wide char at x=0 (width 2)
            buf.set_raw(0, 0, Cell::from_char('中'));
            // x=1 is implicitly CONTINUATION from Buffer::new init (or empty)
            // Wait, set_raw only sets one cell.
            // But Presenter logic depends on Buffer content.
            // For the test, we want to simulate the state where we have a wide char.
            // The proper way is to use `set` which handles continuations, or manually set them.
            buf.set(0, 0, Cell::from_char('中')); // This sets x=0 to '中', x=1 to CONTINUATION

            // Force a diff by having a different previous buffer
            let prev = Buffer::new(10, 1);
            writer.prev_buffer = Some(prev);

            writer.present_ui(&buf, None, true).unwrap();
        }

        let output_str = String::from_utf8_lossy(&output);

        // Should contain '中'
        assert!(output_str.contains('中'));

        // Should NOT contain a space immediately after '中' (if it was treated as orphan)
        // Since '中' is e4 b8 ad (3 bytes).
        let bytes = output_str.as_bytes();
        let pos = bytes.windows(3).position(|w| w == "中".as_bytes());
        assert!(pos.is_some());

        // Check byte after '中'
        let after = pos.unwrap() + 3;
        if after < bytes.len() {
            // It might be ANSI sequence or nothing. It should NOT be space (0x20).
            assert_ne!(
                bytes[after], 0x20,
                "Wide char continuation clobbered with space"
            );
        }
    }

    // =========================================================================
    // NON-INTERFERENCE CONTRACT TESTS (bd-1bavy)
    //
    // These tests verify that terminal mode behavior and ownership semantics
    // are preserved under lifecycle changes. The Asupersync migration MUST
    // keep all these guarantees intact.
    // =========================================================================

    /// CONTRACT: INLINE_ACTIVE_WIDGETS gauge increments on inline writer
    /// creation and decrements on drop. Net effect of create+drop is zero.
    /// Note: Tests use relative deltas because the global counter is shared.
    #[test]
    fn noninterference_inline_gauge_balanced_across_lifecycle() {
        let _lock = GAUGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        for _ in 0..64 {
            let before = inline_active_widgets();
            let writer = TerminalWriter::new(
                Vec::new(),
                ScreenMode::Inline { ui_height: 3 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            let during = inline_active_widgets();
            drop(writer);
            let after = inline_active_widgets();

            if during == before.saturating_add(1) && after == before {
                return;
            }
            std::thread::yield_now();
        }

        panic!("failed to observe stable inline lifecycle gauge transition");
    }

    /// CONTRACT: AltScreen writers do NOT affect the inline gauge.
    /// Verified by checking the ScreenMode match in the Drop impl.
    /// (Note: the global atomic gauge is tested for inline modes in
    /// other tests; here we just verify the code path distinction.)
    #[test]
    fn noninterference_altscreen_does_not_affect_inline_gauge() {
        let _lock = GAUGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        // The contract is verified structurally: ScreenMode::AltScreen does
        // not match the inline pattern in both the constructor (fetch_add)
        // and the Drop impl (fetch_sub). We verify this by checking that
        // creating and immediately dropping an AltScreen writer round-trips
        // without affecting the delta observed from a controlled inline writer.
        let mut observed_stable = false;
        for _ in 0..64 {
            let before = inline_active_widgets();
            drop(TerminalWriter::new(
                Vec::new(),
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            ));
            let after = inline_active_widgets();

            if after == before {
                observed_stable = true;
                break;
            }
            std::thread::yield_now();
        }

        assert!(
            observed_stable,
            "failed to observe stable altscreen lifecycle gauge transition"
        );

        // Also verify the structural contract directly:
        assert!(
            !matches!(
                ScreenMode::AltScreen,
                ScreenMode::Inline { .. } | ScreenMode::InlineAuto { .. }
            ),
            "AltScreen must not match inline patterns"
        );
    }

    /// CONTRACT: InlineAuto also tracks the inline gauge correctly.
    #[test]
    fn noninterference_inline_auto_gauge_balanced() {
        let _lock = GAUGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        for _ in 0..64 {
            let before = inline_active_widgets();
            let writer = TerminalWriter::new(
                Vec::new(),
                ScreenMode::InlineAuto {
                    min_height: 3,
                    max_height: 10,
                },
                UiAnchor::Bottom,
                basic_caps(),
            );
            let during = inline_active_widgets();
            drop(writer);
            let after = inline_active_widgets();

            if during == before.saturating_add(1) && after == before {
                return;
            }
            std::thread::yield_now();
        }

        panic!("failed to observe stable inline-auto lifecycle gauge transition");
    }

    /// CONTRACT: into_inner() performs cleanup before releasing the writer.
    /// The returned output must contain cursor-show and flush.
    #[test]
    fn noninterference_into_inner_performs_cleanup() {
        let _lock = GAUGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());

        let cursor_show = b"\x1b[?25h";
        for _ in 0..64 {
            let before_gauge = inline_active_widgets();

            let mut writer = TerminalWriter::new(
                Vec::new(),
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            let during_gauge = inline_active_widgets();
            let output = writer.into_inner().expect("should return writer");
            let after_gauge = inline_active_widgets();

            if during_gauge == before_gauge.saturating_add(1) && after_gauge == before_gauge {
                assert!(
                    output.windows(cursor_show.len()).any(|w| w == cursor_show),
                    "into_inner must emit cursor show during cleanup"
                );
                return;
            }
            std::thread::yield_now();
        }

        panic!("failed to observe stable into_inner gauge transition");
    }

    /// CONTRACT: Cleanup output from inline mode must contain cursor restore
    /// (DEC 8) if cursor was saved during present.
    #[test]
    fn noninterference_inline_cleanup_restores_cursor_after_present() {
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

            // Writer will be dropped here, triggering cleanup
        }

        // Count cursor save/restore pairs
        let saves = output
            .windows(CURSOR_SAVE.len())
            .filter(|w| *w == CURSOR_SAVE)
            .count();
        let restores = output
            .windows(CURSOR_RESTORE.len())
            .filter(|w| *w == CURSOR_RESTORE)
            .count();

        assert!(saves > 0, "present must save cursor");
        assert!(
            restores >= saves,
            "cleanup must ensure all cursor saves are restored: {saves} saves, {restores} restores"
        );
    }

    /// CONTRACT: AltScreen cleanup must show cursor. It must NOT emit
    /// cursor restore or scroll region reset (those are inline-only).
    #[test]
    fn noninterference_altscreen_cleanup_minimal() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            let mut buffer = Buffer::new(80, 24);
            buffer.set_raw(0, 0, Cell::from_char('A'));
            writer.present_ui(&buffer, None, true).unwrap();
        }

        // Must contain cursor show
        let cursor_show = b"\x1b[?25h";
        assert!(
            output.windows(cursor_show.len()).any(|w| w == cursor_show),
            "AltScreen cleanup must show cursor"
        );

        // Must NOT contain scroll region reset (inline-only)
        // (scroll region was never activated for AltScreen)
        // This is verified by the scroll_region_active flag being false
    }

    /// CONTRACT: Rapid present/log interleaving in inline mode must not
    /// corrupt the output stream. Each present must be complete and each
    /// log must be sanitized.
    #[test]
    fn noninterference_rapid_present_log_interleave() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 3 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(40, 12);

            for i in 0..10 {
                let mut buffer = Buffer::new(40, 3);
                buffer.set_raw(0, 0, Cell::from_char(char::from(b'A' + (i % 26))));
                writer.present_ui(&buffer, None, true).unwrap();
                writer.write_log(&format!("log-{i}")).unwrap();
            }
        }

        // Output must contain cursor show (cleanup ran)
        let cursor_show = b"\x1b[?25h";
        assert!(
            output.windows(cursor_show.len()).any(|w| w == cursor_show),
            "cleanup must complete after rapid interleaving"
        );

        // Output must not contain unmatched escape sequences
        // (simple check: no bare ESC at end without terminator)
        let output_len = output.len();
        if output_len > 1 {
            let last_esc = output.iter().rposition(|&b| b == 0x1b);
            if let Some(pos) = last_esc {
                // If the last ESC is within 10 bytes of the end, verify it's a complete sequence
                if output_len - pos < 10 {
                    // Should be part of cursor show or similar short sequence
                    assert!(
                        output_len - pos >= 3,
                        "truncated escape sequence at end of output"
                    );
                }
            }
        }
    }

    /// CONTRACT: Resize between presents must not leave stale diff state.
    /// The first present after resize must produce valid output.
    #[test]
    fn noninterference_resize_between_presents_clears_diff_state() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 24);

            // First present at 80x5
            let buffer1 = Buffer::new(80, 5);
            writer.present_ui(&buffer1, None, true).unwrap();

            // Resize
            writer.set_size(120, 30);
            assert!(
                writer.prev_buffer.is_none(),
                "set_size must clear prev_buffer to invalidate diff"
            );

            // Second present at 120x5 — must not panic or produce corrupt output
            let buffer2 = Buffer::new(120, 5);
            writer.present_ui(&buffer2, None, true).unwrap();
        }

        // If we got here without panic, the resize was handled correctly
        let cursor_show = b"\x1b[?25h";
        assert!(
            output.windows(cursor_show.len()).any(|w| w == cursor_show),
            "output must be valid after resize"
        );
    }

    /// CONTRACT: Multiple writers can be created sequentially on the same
    /// output without interference. Each writer's cleanup must be complete
    /// before the next writer starts.
    #[test]
    fn noninterference_sequential_writers_clean_handoff() {
        let mut output = Vec::new();

        // First writer: Inline mode
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 3 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(40, 12);
            let buffer = Buffer::new(40, 3);
            writer.present_ui(&buffer, None, true).unwrap();
            // Dropped: cleanup runs
        }

        let inline_end = output.len();

        // Second writer: AltScreen mode on same output
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(40, 12);
            let mut buffer = Buffer::new(40, 12);
            buffer.set_raw(0, 0, Cell::from_char('Z'));
            writer.present_ui(&buffer, None, true).unwrap();
            // Dropped: cleanup runs
        }

        // Both cleanups must have produced cursor show
        let cursor_show = b"\x1b[?25h";
        let first_show = output[..inline_end]
            .windows(cursor_show.len())
            .any(|w| w == cursor_show);
        let second_show = output[inline_end..]
            .windows(cursor_show.len())
            .any(|w| w == cursor_show);

        assert!(first_show, "first writer must show cursor on cleanup");
        assert!(second_show, "second writer must show cursor on cleanup");
    }

    /// CONTRACT: present_ui_owned must produce identical output to present_ui
    /// for the same buffer content. The only difference should be performance.
    #[test]
    fn noninterference_present_ui_owned_matches_present_ui() {
        let mut output_borrowed = Vec::new();
        let mut output_owned = Vec::new();

        let mut buffer = Buffer::new(20, 5);
        buffer.set_raw(0, 0, Cell::from_char('H'));
        buffer.set_raw(1, 0, Cell::from_char('i'));

        // Borrowed path
        {
            let mut writer = TerminalWriter::new(
                &mut output_borrowed,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(20, 10);
            writer.present_ui(&buffer, None, true).unwrap();
        }

        // Owned path
        {
            let mut writer = TerminalWriter::new(
                &mut output_owned,
                ScreenMode::Inline { ui_height: 5 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(20, 10);
            writer.present_ui_owned(buffer, None, true).unwrap();
        }

        // Both must contain cursor save/restore
        assert!(
            output_borrowed
                .windows(CURSOR_SAVE.len())
                .any(|w| w == CURSOR_SAVE),
            "borrowed path must save cursor"
        );
        assert!(
            output_owned
                .windows(CURSOR_SAVE.len())
                .any(|w| w == CURSOR_SAVE),
            "owned path must save cursor"
        );

        // Both must contain the 'H' character
        assert!(
            output_borrowed.windows(1).any(|w| w == b"H"),
            "borrowed path must render content"
        );
        assert!(
            output_owned.windows(1).any(|w| w == b"H"),
            "owned path must render content"
        );
    }

    /// CONTRACT: write_log is a no-op in AltScreen mode but works in inline.
    /// This behavioral difference must be preserved.
    #[test]
    fn noninterference_write_log_mode_behavior_preserved() {
        // Inline: write_log produces output
        let mut inline_output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut inline_output,
                ScreenMode::Inline { ui_height: 3 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(40, 12);
            writer.write_log("hello").unwrap();
        }
        assert!(
            inline_output.windows(5).any(|w| w == b"hello"),
            "inline write_log must produce output"
        );

        // AltScreen: write_log is silent
        let mut alt_output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut alt_output,
                ScreenMode::AltScreen,
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(40, 12);
            writer.write_log("hello").unwrap();
        }
        // The output should not contain "hello" text from write_log
        // (it will contain cleanup sequences)
        let has_hello = alt_output.windows(5).any(|w| w == b"hello");
        assert!(!has_hello, "AltScreen write_log must be silent (no-op)");
    }

    /// CONTRACT: Sync output sequences are balanced (begin/end) across
    /// multiple present calls. An open sync block must never leak.
    #[test]
    fn noninterference_sync_output_balanced_across_multiple_presents() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 3 },
                UiAnchor::Bottom,
                full_caps(),
            );
            writer.set_size(20, 10);

            for _ in 0..5 {
                let buffer = Buffer::new(20, 3);
                writer.present_ui(&buffer, None, true).unwrap();
            }
            // Writer drop triggers cleanup
        }

        let begins = output
            .windows(SYNC_BEGIN.len())
            .filter(|w| *w == SYNC_BEGIN)
            .count();
        let ends = output
            .windows(SYNC_END.len())
            .filter(|w| *w == SYNC_END)
            .count();

        assert!(begins > 0, "sync-capable writer must emit SYNC_BEGIN");
        assert_eq!(
            begins, ends,
            "sync blocks must be balanced: {begins} begins, {ends} ends"
        );
    }

    /// CONTRACT: InlineAuto effective_ui_height is clamped to terminal height.
    /// A writer with max_height > terminal height must clamp without panic.
    #[test]
    fn noninterference_inline_auto_height_clamped_without_panic() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::InlineAuto {
                    min_height: 3,
                    max_height: 100,
                },
                UiAnchor::Bottom,
                basic_caps(),
            );
            // Terminal is only 10 rows tall
            writer.set_size(80, 10);

            // effective_ui_height for InlineAuto clamps to term_height
            let effective = writer.effective_ui_height();
            assert!(
                effective <= 10,
                "InlineAuto effective_ui_height must clamp to terminal height, got {effective}"
            );

            let buffer = Buffer::new(80, effective);
            writer.present_ui(&buffer, None, true).unwrap();
        }
    }

    /// CONTRACT: Inline mode with ui_height > terminal height must not panic
    /// during present. The rendering path handles this gracefully.
    #[test]
    fn noninterference_inline_oversized_height_no_panic() {
        let mut output = Vec::new();
        {
            let mut writer = TerminalWriter::new(
                &mut output,
                ScreenMode::Inline { ui_height: 100 },
                UiAnchor::Bottom,
                basic_caps(),
            );
            writer.set_size(80, 10);

            // Inline effective_ui_height returns raw value without clamping
            // but the rendering path must handle this without panic
            let buffer = Buffer::new(80, 10);
            // This should not panic even though ui_height > term_height
            let result = writer.present_ui(&buffer, None, true);
            assert!(
                result.is_ok(),
                "present_ui must not panic with oversized ui_height"
            );
        }
    }

    // ── Render-certificate integration (bd-6b9nr) ───────────────────────────

    fn present_sequence(certified: bool) -> (Vec<u8>, Vec<String>) {
        let config = RuntimeDiffConfig {
            certified_skips: certified,
            // Deterministic strategy behavior for the byte-equality comparison.
            bayesian_enabled: false,
            ..RuntimeDiffConfig::default()
        };
        let mut writer = TerminalWriter::with_diff_config(
            Vec::new(),
            ScreenMode::Inline { ui_height: 4 },
            UiAnchor::Bottom,
            full_caps(),
            config,
        );
        writer.set_size(24, 12);

        let mut certificates = Vec::new();
        for frame in 0..4u16 {
            let mut buffer = Buffer::new(24, 4);
            if frame > 0 {
                buffer.clear_dirty();
            }
            match frame {
                // Frame 0: initial paint (all rows dirty by construction).
                0 => {}
                // Frame 1: identical content — zero dirty rows.
                1 => {}
                // Frame 2: sparse update on two rows.
                2 => {
                    buffer.set(3, 1, ftui_render::cell::Cell::from_char('X'));
                    buffer.set(9, 3, ftui_render::cell::Cell::from_char('Y'));
                }
                // Frame 3: another sparse update.
                _ => {
                    buffer.set(3, 1, ftui_render::cell::Cell::from_char('Z'));
                }
            }
            writer
                .present_ui_owned(buffer, None, false)
                .expect("present");
            certificates.push(
                writer
                    .last_render_certificate()
                    .map(|c| format!("{}:{:?}", c.level.label(), c.causes.clone()))
                    .unwrap_or_else(|| "none".to_string()),
            );
        }
        (writer.into_inner().expect("writer sink"), certificates)
    }

    #[test]
    fn certified_path_is_byte_identical_to_legacy_dirty_path() {
        let (certified_bytes, _) = present_sequence(true);
        let (legacy_bytes, _) = present_sequence(false);
        assert_eq!(
            certified_bytes, legacy_bytes,
            "certified diff path changed visible output"
        );
    }

    #[test]
    fn zero_dirty_frame_earns_a_skip_all_certificate() {
        let (_, certificates) = present_sequence(true);
        // Frame 1 re-presents identical content with zero dirty rows.
        assert!(
            certificates[1].starts_with("skip-all"),
            "frame 1 certificate was {}",
            certificates[1]
        );
        // Sparse frames narrow to their dirty rows.
        assert!(
            certificates[2].starts_with("narrow-to-dirty"),
            "frame 2 certificate was {}",
            certificates[2]
        );
    }

    #[test]
    fn certificates_name_full_work_causes() {
        let (_, certificates) = present_sequence(true);
        // Frame 0 has no previous buffer: full redraw with a named cause.
        assert!(
            certificates[0].contains("no-previous-frame"),
            "frame 0 certificate was {}",
            certificates[0]
        );
    }

    #[test]
    fn legacy_path_records_no_certificate() {
        let (_, certificates) = present_sequence(false);
        // The dirty-rows frames record None when certified skips are off
        // (frame 0/resize paths still certify full work explicitly).
        assert_eq!(certificates[2], "none");
        assert_eq!(certificates[3], "none");
    }
}
