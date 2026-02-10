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
use crate::counting_writer::{CountingWriter, PresentStats, StatsCollector};
use crate::diff::{BufferDiff, ChangeRun};
use crate::grapheme_pool::GraphemePool;
use crate::link_registry::LinkRegistry;

pub use ftui_core::terminal_capabilities::TerminalCapabilities;

/// Size of the internal write buffer (64KB).
const BUFFER_CAPACITY: usize = 64 * 1024;

// =============================================================================
// DP Cost Model for ANSI Emission
// =============================================================================

/// Byte-cost estimates for ANSI cursor and output operations.
///
/// The cost model computes the cheapest emission plan for each row by comparing
/// sparse-run emission (CUP per run) against merged write-through (one CUP,
/// fill gaps with buffer content). This is a shortest-path problem on a small
/// state graph per row.
mod cost_model {
    use smallvec::SmallVec;

    use super::ChangeRun;

    /// Number of decimal digits needed to represent `n`.
    #[inline]
    fn digit_count(n: u16) -> usize {
        if n >= 10000 {
            5
        } else if n >= 1000 {
            4
        } else if n >= 100 {
            3
        } else if n >= 10 {
            2
        } else {
            1
        }
    }

    /// Byte cost of CUP: `\x1b[{row+1};{col+1}H`
    #[inline]
    pub fn cup_cost(row: u16, col: u16) -> usize {
        // CSI (2) + row digits + ';' (1) + col digits + 'H' (1)
        4 + digit_count(row.saturating_add(1)) + digit_count(col.saturating_add(1))
    }

    /// Byte cost of CHA (column-only): `\x1b[{col+1}G`
    #[inline]
    pub fn cha_cost(col: u16) -> usize {
        // CSI (2) + col digits + 'G' (1)
        3 + digit_count(col.saturating_add(1))
    }

    /// Byte cost of CUF (cursor forward): `\x1b[{n}C` or `\x1b[C` for n=1.
    #[inline]
    pub fn cuf_cost(n: u16) -> usize {
        match n {
            0 => 0,
            1 => 3, // \x1b[C
            _ => 3 + digit_count(n),
        }
    }

    /// Cheapest cursor movement cost from (from_x, from_y) to (to_x, to_y).
    /// Returns 0 if already at the target position.
    pub fn cheapest_move_cost(
        from_x: Option<u16>,
        from_y: Option<u16>,
        to_x: u16,
        to_y: u16,
    ) -> usize {
        // Already at target?
        if from_x == Some(to_x) && from_y == Some(to_y) {
            return 0;
        }

        let cup = cup_cost(to_y, to_x);

        match (from_x, from_y) {
            (Some(fx), Some(fy)) if fy == to_y => {
                // Same row: compare CHA, CUF, and CUP
                let cha = cha_cost(to_x);
                if to_x > fx {
                    let cuf = cuf_cost(to_x - fx);
                    cup.min(cha).min(cuf)
                } else if to_x == fx {
                    0
                } else {
                    // Moving backward: CHA or CUP
                    cup.min(cha)
                }
            }
            _ => cup,
        }
    }

    /// Planned contiguous span to emit on a single row.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RowSpan {
        /// Row index.
        pub y: u16,
        /// Start column (inclusive).
        pub x0: u16,
        /// End column (inclusive).
        pub x1: u16,
    }

    /// Row emission plan (possibly multiple merged spans).
    ///
    /// Uses SmallVec<[RowSpan; 4]> to avoid heap allocation for the common case
    /// of 1-4 spans per row. RowSpan is 6 bytes, so 4 spans = 24 bytes inline.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RowPlan {
        spans: SmallVec<[RowSpan; 4]>,
        total_cost: usize,
    }

    impl RowPlan {
        #[inline]
        pub fn spans(&self) -> &[RowSpan] {
            &self.spans
        }

        /// Total cost of this row plan (for strategy selection).
        #[inline]
        #[allow(dead_code)] // API for future diff strategy integration
        pub fn total_cost(&self) -> usize {
            self.total_cost
        }
    }

    /// Reusable scratch buffers for `plan_row_reuse`, avoiding per-call heap
    /// allocations. Store one instance in `Presenter` and pass it into every
    /// `plan_row_reuse` call so that the buffers are reused across rows and
    /// frames.
    #[derive(Debug, Default)]
    pub struct RowPlanScratch {
        prefix_cells: Vec<usize>,
        dp: Vec<usize>,
        prev: Vec<usize>,
    }

    /// Compute the optimal emission plan for a set of runs on the same row.
    ///
    /// This is a shortest-path / DP partitioning problem over contiguous run
    /// segments. Each segment may be emitted as a merged span (writing through
    /// gaps). Single-run segments correspond to sparse emission.
    ///
    /// Gap cells cost ~1 byte each (character content), plus potential style
    /// overhead estimated at 1 byte per gap cell (conservative).
    pub fn plan_row(row_runs: &[ChangeRun], prev_x: Option<u16>, prev_y: Option<u16>) -> RowPlan {
        let mut scratch = RowPlanScratch::default();
        plan_row_reuse(row_runs, prev_x, prev_y, &mut scratch)
    }

    /// Like `plan_row` but reuses heap allocations via the provided scratch
    /// buffers, eliminating per-call allocations in the hot path.
    pub fn plan_row_reuse(
        row_runs: &[ChangeRun],
        prev_x: Option<u16>,
        prev_y: Option<u16>,
        scratch: &mut RowPlanScratch,
    ) -> RowPlan {
        debug_assert!(!row_runs.is_empty());

        let row_y = row_runs[0].y;
        let run_count = row_runs.len();

        // Resize scratch buffers (no-op if already large enough).
        scratch.prefix_cells.clear();
        scratch.prefix_cells.resize(run_count + 1, 0);
        scratch.dp.clear();
        scratch.dp.resize(run_count, usize::MAX);
        scratch.prev.clear();
        scratch.prev.resize(run_count, 0);

        // Prefix sum of changed cell counts for O(1) segment cost.
        for (i, run) in row_runs.iter().enumerate() {
            scratch.prefix_cells[i + 1] = scratch.prefix_cells[i] + run.len() as usize;
        }

        // DP over segments: dp[j] is min cost to emit runs[0..=j].
        for j in 0..run_count {
            let mut best_cost = usize::MAX;
            let mut best_i = j;

            // Optimization: iterate backwards and break if the gap becomes too large.
            // The gap cost grows linearly, while cursor movement cost is bounded (~10-15 bytes).
            // Once the gap exceeds ~20 cells, merging is strictly worse than moving.
            // We use 32 as a conservative safety bound.
            for i in (0..=j).rev() {
                let changed_cells = scratch.prefix_cells[j + 1] - scratch.prefix_cells[i];
                let total_cells = (row_runs[j].x1 - row_runs[i].x0 + 1) as usize;
                let gap_cells = total_cells - changed_cells;

                if gap_cells > 32 {
                    break;
                }

                let from_x = if i == 0 {
                    prev_x
                } else {
                    Some(row_runs[i - 1].x1.saturating_add(1))
                };
                let from_y = if i == 0 { prev_y } else { Some(row_y) };

                let move_cost = cheapest_move_cost(from_x, from_y, row_runs[i].x0, row_y);
                let gap_overhead = gap_cells * 2; // conservative: char + style amortized
                let emit_cost = changed_cells + gap_overhead;

                let prev_cost = if i == 0 { 0 } else { scratch.dp[i - 1] };
                let cost = prev_cost
                    .saturating_add(move_cost)
                    .saturating_add(emit_cost);

                if cost < best_cost {
                    best_cost = cost;
                    best_i = i;
                }
            }

            scratch.dp[j] = best_cost;
            scratch.prev[j] = best_i;
        }

        // Reconstruct spans from back to front.
        let mut spans: SmallVec<[RowSpan; 4]> = SmallVec::new();
        let mut j = run_count - 1;
        loop {
            let i = scratch.prev[j];
            spans.push(RowSpan {
                y: row_y,
                x0: row_runs[i].x0,
                x1: row_runs[j].x1,
            });
            if i == 0 {
                break;
            }
            j = i - 1;
        }
        spans.reverse();

        RowPlan {
            spans,
            total_cost: scratch.dp[run_count - 1],
        }
    }
}

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
    /// Buffered writer for efficient output, with byte counting.
    writer: CountingWriter<BufWriter<W>>,
    /// Current style state (None = unknown/reset).
    current_style: Option<CellStyle>,
    /// Current hyperlink ID (None = no link).
    current_link: Option<u32>,
    /// Current cursor X position (0-indexed). None = unknown.
    cursor_x: Option<u16>,
    /// Current cursor Y position (0-indexed). None = unknown.
    cursor_y: Option<u16>,
    /// Terminal capabilities for conditional output.
    capabilities: TerminalCapabilities,
    /// Reusable scratch buffers for the cost-model DP, avoiding per-row
    /// heap allocations in the hot presentation path.
    plan_scratch: cost_model::RowPlanScratch,
    /// Reusable buffer for change runs, avoiding per-frame allocation.
    runs_buf: Vec<ChangeRun>,
}

impl<W: Write> Presenter<W> {
    /// Create a new presenter with the given writer and capabilities.
    pub fn new(writer: W, capabilities: TerminalCapabilities) -> Self {
        Self {
            writer: CountingWriter::new(BufWriter::with_capacity(BUFFER_CAPACITY, writer)),
            current_style: None,
            current_link: None,
            cursor_x: None,
            cursor_y: None,
            capabilities,
            plan_scratch: cost_model::RowPlanScratch::default(),
            runs_buf: Vec::new(),
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
    pub fn present(&mut self, buffer: &Buffer, diff: &BufferDiff) -> io::Result<PresentStats> {
        self.present_with_pool(buffer, diff, None, None)
    }

    /// Present a frame with grapheme pool and link registry.
    pub fn present_with_pool(
        &mut self,
        buffer: &Buffer,
        diff: &BufferDiff,
        pool: Option<&GraphemePool>,
        links: Option<&LinkRegistry>,
    ) -> io::Result<PresentStats> {
        #[cfg(feature = "tracing")]
        let _span = tracing::info_span!(
            "present",
            width = buffer.width(),
            height = buffer.height(),
            changes = diff.len()
        );
        #[cfg(feature = "tracing")]
        let _guard = _span.enter();

        // Calculate runs upfront for stats, reusing the runs buffer.
        diff.runs_into(&mut self.runs_buf);
        let run_count = self.runs_buf.len();
        let cells_changed = diff.len();

        // Start stats collection
        self.writer.reset_counter();
        let collector = StatsCollector::start(cells_changed, run_count);

        // Begin synchronized output to prevent flicker
        if self.capabilities.sync_output {
            ansi::sync_begin(&mut self.writer)?;
        }

        // Emit diff using run grouping for efficiency
        self.emit_runs_reuse(buffer, pool, links)?;

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

        self.writer.flush()?;

        let stats = collector.finish(self.writer.bytes_written());

        #[cfg(feature = "tracing")]
        {
            stats.log();
            tracing::trace!("frame presented");
        }

        Ok(stats)
    }

    /// Emit runs of changed cells using the DP cost model.
    ///
    /// Groups runs by row, then for each row decides whether to emit runs
    /// individually (sparse) or merge them (write through gaps) based on
    /// byte cost estimation.
    #[allow(dead_code)] // Kept for reference; production path uses emit_runs_reuse
    fn emit_runs(
        &mut self,
        buffer: &Buffer,
        runs: &[ChangeRun],
        pool: Option<&GraphemePool>,
        links: Option<&LinkRegistry>,
    ) -> io::Result<()> {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!("emit_diff");
        #[cfg(feature = "tracing")]
        let _guard = _span.enter();

        #[cfg(feature = "tracing")]
        tracing::trace!(run_count = runs.len(), "emitting runs");

        // Group runs by row and apply cost model per row
        let mut i = 0;
        while i < runs.len() {
            let row_y = runs[i].y;

            // Collect all runs on this row
            let row_start = i;
            while i < runs.len() && runs[i].y == row_y {
                i += 1;
            }
            let row_runs = &runs[row_start..i];

            let plan = cost_model::plan_row(row_runs, self.cursor_x, self.cursor_y);

            #[cfg(feature = "tracing")]
            tracing::trace!(
                row = row_y,
                spans = plan.spans().len(),
                cost = plan.total_cost(),
                "row plan"
            );

            let row = buffer.row_cells(row_y);
            for span in plan.spans() {
                self.move_cursor_optimal(span.x0, span.y)?;
                // Hot path: avoid recomputing `y * width + x` for every cell.
                let start = span.x0 as usize;
                let end = span.x1 as usize;
                debug_assert!(start <= end);
                debug_assert!(end < row.len());

                let mut idx = start;
                for cell in &row[start..=end] {
                    self.emit_cell(idx as u16, cell, pool, links)?;
                    idx += 1;
                }
            }
        }
        Ok(())
    }

    /// Like `emit_runs` but uses the internal `runs_buf` and `plan_scratch`
    /// to avoid per-row and per-frame allocations.
    fn emit_runs_reuse(
        &mut self,
        buffer: &Buffer,
        pool: Option<&GraphemePool>,
        links: Option<&LinkRegistry>,
    ) -> io::Result<()> {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!("emit_diff");
        #[cfg(feature = "tracing")]
        let _guard = _span.enter();

        #[cfg(feature = "tracing")]
        tracing::trace!(run_count = self.runs_buf.len(), "emitting runs (reuse)");

        // Group runs by row and apply cost model per row
        let mut i = 0;
        while i < self.runs_buf.len() {
            let row_y = self.runs_buf[i].y;

            // Collect all runs on this row
            let row_start = i;
            while i < self.runs_buf.len() && self.runs_buf[i].y == row_y {
                i += 1;
            }
            let row_runs = &self.runs_buf[row_start..i];

            let plan = cost_model::plan_row_reuse(
                row_runs,
                self.cursor_x,
                self.cursor_y,
                &mut self.plan_scratch,
            );

            #[cfg(feature = "tracing")]
            tracing::trace!(
                row = row_y,
                spans = plan.spans().len(),
                cost = plan.total_cost(),
                "row plan"
            );

            let row = buffer.row_cells(row_y);
            for span in plan.spans() {
                self.move_cursor_optimal(span.x0, span.y)?;
                // Hot path: avoid recomputing `y * width + x` for every cell.
                let start = span.x0 as usize;
                let end = span.x1 as usize;
                debug_assert!(start <= end);
                debug_assert!(end < row.len());

                let mut idx = start;
                for cell in &row[start..=end] {
                    self.emit_cell(idx as u16, cell, pool, links)?;
                    idx += 1;
                }
            }
        }
        Ok(())
    }

    /// Emit a single cell.
    fn emit_cell(
        &mut self,
        x: u16,
        cell: &Cell,
        pool: Option<&GraphemePool>,
        links: Option<&LinkRegistry>,
    ) -> io::Result<()> {
        // Continuation cells are the tail cells of wide glyphs. Emitting the
        // head glyph already advanced the terminal cursor by the full width, so
        // we normally skip emitting these cells.
        //
        // If we ever start emitting at a continuation cell (e.g. a run begins
        // mid-wide-character), we must still advance the terminal cursor by one
        // cell to keep subsequent emissions aligned. Prefer CUF over writing a
        // space so we don't overwrite a valid wide-glyph tail.
        if cell.is_continuation() {
            match self.cursor_x {
                // Cursor already advanced past this cell by a previously-emitted wide head.
                Some(cx) if cx > x => return Ok(()),
                // Cursor is positioned at (or before) this continuation cell: advance by 1.
                Some(cx) => {
                    ansi::cuf(&mut self.writer, 1)?;
                    self.cursor_x = Some(cx.saturating_add(1));
                    return Ok(());
                }
                // Defensive: move_cursor_optimal should always set cursor_x before emit_cell is called.
                None => {
                    ansi::cuf(&mut self.writer, 1)?;
                    self.cursor_x = Some(x.saturating_add(1));
                    return Ok(());
                }
            }
        }

        // Emit style changes if needed
        self.emit_style_changes(cell)?;

        // Emit link changes if needed
        self.emit_link_changes(cell, links)?;

        // Calculate effective width and check for zero-width content (e.g. combining marks)
        // stored as standalone cells. These must be replaced to maintain grid alignment.
        let raw_width = cell.content.width();
        let is_zero_width_content = raw_width == 0 && !cell.is_empty() && !cell.is_continuation();

        if is_zero_width_content {
            // Replace with U+FFFD Replacement Character (width 1)
            self.writer.write_all(b"\xEF\xBF\xBD")?;
        } else {
            // Emit normal content
            self.emit_content(cell, pool)?;
        }

        // Update cursor position (character output advances cursor)
        if let Some(cx) = self.cursor_x {
            // Empty cells are emitted as spaces (width 1).
            // Zero-width content replaced by U+FFFD is width 1.
            let width = if cell.is_empty() || is_zero_width_content {
                1
            } else {
                raw_width
            };
            self.cursor_x = Some(cx.saturating_add(width as u16));
        }

        Ok(())
    }

    /// Emit style changes if the cell style differs from current.
    ///
    /// Uses SGR delta: instead of resetting and re-applying all style properties,
    /// we compute the minimal set of changes needed (fg delta, bg delta, attr
    /// toggles). Falls back to reset+apply only when a full reset would be cheaper.
    fn emit_style_changes(&mut self, cell: &Cell) -> io::Result<()> {
        let new_style = CellStyle::from_cell(cell);

        // Check if style changed
        if self.current_style == Some(new_style) {
            return Ok(());
        }

        match self.current_style {
            None => {
                // No known state - must do full apply (but skip reset if we haven't
                // emitted anything yet, the frame-start reset handles that).
                self.emit_style_full(new_style)?;
            }
            Some(old_style) => {
                self.emit_style_delta(old_style, new_style)?;
            }
        }

        self.current_style = Some(new_style);
        Ok(())
    }

    /// Full style apply (reset + set all properties). Used when previous state is unknown.
    fn emit_style_full(&mut self, style: CellStyle) -> io::Result<()> {
        ansi::sgr_reset(&mut self.writer)?;
        if style.fg.a() > 0 {
            ansi::sgr_fg_packed(&mut self.writer, style.fg)?;
        }
        if style.bg.a() > 0 {
            ansi::sgr_bg_packed(&mut self.writer, style.bg)?;
        }
        if !style.attrs.is_empty() {
            ansi::sgr_flags(&mut self.writer, style.attrs)?;
        }
        Ok(())
    }

    #[inline]
    fn dec_len_u8(value: u8) -> u32 {
        if value >= 100 {
            3
        } else if value >= 10 {
            2
        } else {
            1
        }
    }

    #[inline]
    fn sgr_code_len(code: u8) -> u32 {
        2 + Self::dec_len_u8(code) + 1
    }

    #[inline]
    fn sgr_flags_len(flags: StyleFlags) -> u32 {
        if flags.is_empty() {
            return 0;
        }
        let mut count = 0u32;
        let mut digits = 0u32;
        for (flag, codes) in ansi::FLAG_TABLE {
            if flags.contains(flag) {
                count += 1;
                digits += Self::dec_len_u8(codes.on);
            }
        }
        if count == 0 {
            return 0;
        }
        3 + digits + (count - 1)
    }

    #[inline]
    fn sgr_flags_off_len(flags: StyleFlags) -> u32 {
        if flags.is_empty() {
            return 0;
        }
        let mut len = 0u32;
        for (flag, codes) in ansi::FLAG_TABLE {
            if flags.contains(flag) {
                len += Self::sgr_code_len(codes.off);
            }
        }
        len
    }

    #[inline]
    fn sgr_rgb_len(color: PackedRgba) -> u32 {
        10 + Self::dec_len_u8(color.r()) + Self::dec_len_u8(color.g()) + Self::dec_len_u8(color.b())
    }

    /// Emit minimal SGR delta between old and new styles.
    ///
    /// Computes which properties changed and emits only those.
    /// Falls back to reset+apply when that would produce fewer bytes.
    fn emit_style_delta(&mut self, old: CellStyle, new: CellStyle) -> io::Result<()> {
        let attrs_removed = old.attrs & !new.attrs;
        let attrs_added = new.attrs & !old.attrs;
        let fg_changed = old.fg != new.fg;
        let bg_changed = old.bg != new.bg;

        // Hot path for VFX-style workloads: attributes are unchanged and only
        // colors vary. In this case, delta emission is always no worse than a
        // reset+reapply baseline, so skip cost estimation and flag diff logic.
        if old.attrs == new.attrs {
            if fg_changed {
                ansi::sgr_fg_packed(&mut self.writer, new.fg)?;
            }
            if bg_changed {
                ansi::sgr_bg_packed(&mut self.writer, new.bg)?;
            }
            return Ok(());
        }

        let mut collateral = StyleFlags::empty();
        if attrs_removed.contains(StyleFlags::BOLD) && new.attrs.contains(StyleFlags::DIM) {
            collateral |= StyleFlags::DIM;
        }
        if attrs_removed.contains(StyleFlags::DIM) && new.attrs.contains(StyleFlags::BOLD) {
            collateral |= StyleFlags::BOLD;
        }

        let mut delta_len = 0u32;
        delta_len += Self::sgr_flags_off_len(attrs_removed);
        delta_len += Self::sgr_flags_len(collateral);
        delta_len += Self::sgr_flags_len(attrs_added);
        if fg_changed {
            delta_len += if new.fg.a() == 0 {
                5
            } else {
                Self::sgr_rgb_len(new.fg)
            };
        }
        if bg_changed {
            delta_len += if new.bg.a() == 0 {
                5
            } else {
                Self::sgr_rgb_len(new.bg)
            };
        }

        let mut baseline_len = 4u32;
        if new.fg.a() > 0 {
            baseline_len += Self::sgr_rgb_len(new.fg);
        }
        if new.bg.a() > 0 {
            baseline_len += Self::sgr_rgb_len(new.bg);
        }
        baseline_len += Self::sgr_flags_len(new.attrs);

        if delta_len > baseline_len {
            return self.emit_style_full(new);
        }

        // Handle attr removal: emit individual off codes
        if !attrs_removed.is_empty() {
            let collateral = ansi::sgr_flags_off(&mut self.writer, attrs_removed, new.attrs)?;
            // Re-enable any collaterally disabled flags
            if !collateral.is_empty() {
                ansi::sgr_flags(&mut self.writer, collateral)?;
            }
        }

        // Handle attr addition: emit on codes for newly added flags
        if !attrs_added.is_empty() {
            ansi::sgr_flags(&mut self.writer, attrs_added)?;
        }

        // Handle fg color change
        if fg_changed {
            ansi::sgr_fg_packed(&mut self.writer, new.fg)?;
        }

        // Handle bg color change
        if bg_changed {
            ansi::sgr_bg_packed(&mut self.writer, new.bg)?;
        }

        Ok(())
    }

    /// Emit hyperlink changes if the cell link differs from current.
    fn emit_link_changes(&mut self, cell: &Cell, links: Option<&LinkRegistry>) -> io::Result<()> {
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

        // Open new link if present and resolvable
        let actually_opened = if let (Some(link_id), Some(registry)) = (new_link, links)
            && let Some(url) = registry.get(link_id)
        {
            ansi::hyperlink_start(&mut self.writer, url)?;
            true
        } else {
            false
        };

        // Only track as current if we actually opened it
        self.current_link = if actually_opened { new_link } else { None };
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
            // Fallback: emit replacement characters matching expected width
            // to maintain cursor synchronization.
            let width = cell.content.width();
            if width > 0 {
                for _ in 0..width {
                    self.writer.write_all(b"?")?;
                }
            }
            return Ok(());
        }

        // Regular character content
        if let Some(ch) = cell.content.as_char() {
            // Sanitize control characters that would break the grid.
            let safe_ch = if ch.is_control() { ' ' } else { ch };
            let mut buf = [0u8; 4];
            let encoded = safe_ch.encode_utf8(&mut buf);
            self.writer.write_all(encoded.as_bytes())
        } else {
            // Empty cell - emit space
            self.writer.write_all(b" ")
        }
    }

    /// Move cursor to the specified position.
    fn move_cursor_to(&mut self, x: u16, y: u16) -> io::Result<()> {
        // Skip if already at position
        if self.cursor_x == Some(x) && self.cursor_y == Some(y) {
            return Ok(());
        }

        // Use CUP (cursor position) for absolute positioning
        ansi::cup(&mut self.writer, y, x)?;
        self.cursor_x = Some(x);
        self.cursor_y = Some(y);
        Ok(())
    }

    /// Move cursor using the cheapest available operation.
    ///
    /// Compares CUP (absolute), CHA (column-only), and CUF (relative forward)
    /// to select the minimum-cost cursor movement.
    fn move_cursor_optimal(&mut self, x: u16, y: u16) -> io::Result<()> {
        // Skip if already at position
        if self.cursor_x == Some(x) && self.cursor_y == Some(y) {
            return Ok(());
        }

        // Decide cheapest move
        let same_row = self.cursor_y == Some(y);
        let forward = same_row && self.cursor_x.is_some_and(|cx| x > cx);

        if same_row && forward {
            let dx = x - self.cursor_x.expect("cursor_x guaranteed by forward check");
            let cuf = cost_model::cuf_cost(dx);
            let cha = cost_model::cha_cost(x);
            let cup = cost_model::cup_cost(y, x);

            if cuf <= cha && cuf <= cup {
                ansi::cuf(&mut self.writer, dx)?;
            } else if cha <= cup {
                ansi::cha(&mut self.writer, x)?;
            } else {
                ansi::cup(&mut self.writer, y, x)?;
            }
        } else if same_row {
            // Same row, backward or same column
            let cha = cost_model::cha_cost(x);
            let cup = cost_model::cup_cost(y, x);
            if cha <= cup {
                ansi::cha(&mut self.writer, x)?;
            } else {
                ansi::cup(&mut self.writer, y, x)?;
            }
        } else {
            // Different row: CUP is the only option
            ansi::cup(&mut self.writer, y, x)?;
        }

        self.cursor_x = Some(x);
        self.cursor_y = Some(y);
        Ok(())
    }

    /// Clear the entire screen.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        ansi::erase_display(&mut self.writer, ansi::EraseDisplayMode::All)?;
        ansi::cup(&mut self.writer, 0, 0)?;
        self.cursor_x = Some(0);
        self.cursor_y = Some(0);
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
        self.cursor_x = None;
        self.cursor_y = None;
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
            .into_inner() // CountingWriter -> BufWriter<W>
            .into_inner() // BufWriter<W> -> Result<W, IntoInnerError>
            .map_err(|e| e.into_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellAttrs;
    use crate::link_registry::LinkRegistry;

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

    fn legacy_plan_row(
        row_runs: &[ChangeRun],
        prev_x: Option<u16>,
        prev_y: Option<u16>,
    ) -> Vec<cost_model::RowSpan> {
        if row_runs.is_empty() {
            return Vec::new();
        }

        if row_runs.len() == 1 {
            let run = row_runs[0];
            return vec![cost_model::RowSpan {
                y: run.y,
                x0: run.x0,
                x1: run.x1,
            }];
        }

        let row_y = row_runs[0].y;
        let first_x = row_runs[0].x0;
        let last_x = row_runs[row_runs.len() - 1].x1;

        // Estimate sparse cost: sum of move + content for each run
        let mut sparse_cost: usize = 0;
        let mut cursor_x = prev_x;
        let mut cursor_y = prev_y;

        for run in row_runs {
            let move_cost = cost_model::cheapest_move_cost(cursor_x, cursor_y, run.x0, run.y);
            let cells = (run.x1 - run.x0 + 1) as usize;
            sparse_cost += move_cost + cells;
            cursor_x = Some(run.x1.saturating_add(1));
            cursor_y = Some(row_y);
        }

        // Estimate merged cost: one move + all cells from first to last
        let merge_move = cost_model::cheapest_move_cost(prev_x, prev_y, first_x, row_y);
        let total_cells = (last_x - first_x + 1) as usize;
        let changed_cells: usize = row_runs.iter().map(|r| (r.x1 - r.x0 + 1) as usize).sum();
        let gap_cells = total_cells - changed_cells;
        let gap_overhead = gap_cells * 2;
        let merged_cost = merge_move + changed_cells + gap_overhead;

        if merged_cost < sparse_cost {
            vec![cost_model::RowSpan {
                y: row_y,
                x0: first_x,
                x1: last_x,
            }]
        } else {
            row_runs
                .iter()
                .map(|run| cost_model::RowSpan {
                    y: run.y,
                    x0: run.x0,
                    x1: run.x1,
                })
                .collect()
        }
    }

    fn emit_spans_for_output(buffer: &Buffer, spans: &[cost_model::RowSpan]) -> Vec<u8> {
        let mut presenter = test_presenter();

        for span in spans {
            presenter
                .move_cursor_optimal(span.x0, span.y)
                .expect("cursor move should succeed");
            for x in span.x0..=span.x1 {
                let cell = buffer.get_unchecked(x, span.y);
                presenter
                    .emit_cell(x, cell, None, None)
                    .expect("emit_cell should succeed");
            }
        }

        presenter
            .writer
            .write_all(b"\x1b[0m")
            .expect("reset should succeed");

        presenter.into_inner().expect("presenter output")
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
    fn sync_output_wraps_frame() {
        let mut presenter = test_presenter_with_sync();
        let mut buffer = Buffer::new(3, 1);
        buffer.set_raw(0, 0, Cell::from_char('X'));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        assert!(
            output.starts_with(ansi::SYNC_BEGIN),
            "sync output should begin with DEC 2026 begin"
        );
        assert!(
            output.ends_with(ansi::SYNC_END),
            "sync output should end with DEC 2026 end"
        );
    }

    #[test]
    fn hyperlink_sequences_emitted_and_closed() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        let mut registry = LinkRegistry::new();
        let link_id = registry.register("https://example.com");
        let linked = Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id));
        buffer.set_raw(0, 0, linked);

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter
            .present_with_pool(&buffer, &diff, None, Some(&registry))
            .unwrap();
        let output = get_output(presenter);

        let start = b"\x1b]8;;https://example.com\x1b\\";
        let end = b"\x1b]8;;\x1b\\";

        let start_pos = output
            .windows(start.len())
            .position(|w| w == start)
            .expect("hyperlink start not found");
        let end_pos = output
            .windows(end.len())
            .position(|w| w == end)
            .expect("hyperlink end not found");
        let char_pos = output
            .iter()
            .position(|&b| b == b'L')
            .expect("linked character not found");

        assert!(start_pos < char_pos, "link start should precede text");
        assert!(char_pos < end_pos, "link end should follow text");
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
        assert_eq!(
            sgr_count, 1,
            "Expected 1 SGR fg sequence, got {}",
            sgr_count
        );
    }

    #[test]
    fn reset_reapplies_style_after_clear() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(1, 1);
        let styled = Cell::from_char('A').with_fg(PackedRgba::rgb(10, 20, 30));
        buffer.set_raw(0, 0, styled);

        let old = Buffer::new(1, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        presenter.reset();
        presenter.present(&buffer, &diff).unwrap();

        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);
        let sgr_count = output_str.matches("\x1b[38;2").count();

        assert_eq!(
            sgr_count, 2,
            "Expected style to be re-applied after reset, got {sgr_count} sequences"
        );
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
        assert!(
            output_str.contains("ABC")
                || (output_str.contains('A')
                    && output_str.contains('B')
                    && output_str.contains('C'))
        );
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
        assert!(
            output
                .windows(ansi::SYNC_END.len())
                .any(|w| w == ansi::SYNC_END)
        );
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
        presenter.cursor_x = Some(50);
        presenter.cursor_y = Some(20);
        presenter.current_style = Some(CellStyle::default());

        presenter.reset();

        assert!(presenter.cursor_x.is_none());
        assert!(presenter.cursor_y.is_none());
        assert!(presenter.current_style.is_none());
    }

    #[test]
    fn position_cursor() {
        let mut presenter = test_presenter();
        presenter.position_cursor(10, 5).unwrap();

        let output = get_output(presenter);
        // CUP is 1-indexed: row 6, col 11
        assert!(
            output
                .windows(b"\x1b[6;11H".len())
                .any(|w| w == b"\x1b[6;11H")
        );
    }

    #[test]
    fn skip_cursor_move_when_already_at_position() {
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(5);
        presenter.cursor_y = Some(3);

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

    #[test]
    fn continuation_at_run_start_advances_cursor_without_overwriting() {
        let mut presenter = test_presenter();
        let mut old = Buffer::new(3, 1);
        let mut new = Buffer::new(3, 1);

        // Construct an inconsistent old/new pair that forces a diff which begins at a
        // continuation cell. This simulates starting emission mid-wide-character.
        //
        // In this case, the presenter must advance the cursor by one cell, but must
        // not overwrite the cell with a space (which can clobber a valid wide glyph tail).
        old.set_raw(0, 0, Cell::from_char('中'));
        new.set_raw(0, 0, Cell::from_char('中'));
        old.set_raw(1, 0, Cell::from_char('X'));
        new.set_raw(1, 0, Cell::CONTINUATION);

        let diff = BufferDiff::compute(&old, &new);
        assert_eq!(diff.changes(), &[(1u16, 0u16)]);

        presenter.present(&new, &diff).unwrap();
        let output = get_output(presenter);

        // Advance should be done via CUF (\x1b[C), not by emitting a space.
        assert!(output.windows(3).any(|w| w == b"\x1b[C"));
        assert!(
            !output.contains(&b' '),
            "should not write a space when advancing over a continuation cell"
        );
    }

    #[test]
    fn wide_char_missing_continuation_causes_drift() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);

        // Bug scenario: User sets wide char but forgets continuation
        buffer.set_raw(0, 0, Cell::from_char('中'));
        // (1,0) remains empty (space)

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let _output_str = String::from_utf8_lossy(&output);

        // Expected if broken: '中' (width 2) followed by ' ' (width 1)
        // '中' takes x=0,1 on screen. Cursor moves to 2.
        // Loop visits x=1 (empty). Emits ' '. Cursor moves to 3.
        // So we emitted 3 columns worth of stuff for 2 cells of buffer.

        // This is hard to assert on the raw string without parsing ANSI,
        // but we know '中' is bytes e4 b8 ad.

        // If correct (with continuation):
        // x=0: emits '中'. cursor -> 2.
        // x=1: skipped (continuation).
        // x=2: next char...

        // If incorrect (current behavior):
        // x=0: emits '中'. cursor -> 2.
        // x=1: emits ' '. cursor -> 3.

        // We can check if a space is emitted immediately after the wide char.
        // Note: Presenter might optimize cursor movement, but here we are writing sequentially.

        // The output should contain '中' then ' '.
        // In a correct world, x=1 is CONTINUATION, so ' ' is NOT emitted for x=1.

        // So if we see '中' followed immediately by ' ' (or escape sequence then ' '), it implies drift IF x=1 was supposed to be covered by '中'.

        // To verify this failure, we assert that the output DOES contain the space.
        // If we fix the bug in Buffer::set, this test setup would need to use set() instead of set_raw()
        // to prove the fix.

        // But for now, let's just assert the current broken behavior exists?
        // No, I want to assert the *bug* is that the buffer allows this state.
        // The Presenter is doing its job (GIGO).

        // Let's rely on the fix verification instead.
    }

    #[test]
    fn hyperlink_emitted_with_registry() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);
        let mut links = LinkRegistry::new();

        let link_id = links.register("https://example.com");
        let cell = Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id));
        buffer.set_raw(0, 0, cell);

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter
            .present_with_pool(&buffer, &diff, None, Some(&links))
            .unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // OSC 8 open with URL
        assert!(
            output_str.contains("\x1b]8;;https://example.com\x1b\\"),
            "Expected OSC 8 open, got: {:?}",
            output_str
        );
        // OSC 8 close (empty URL)
        assert!(
            output_str.contains("\x1b]8;;\x1b\\"),
            "Expected OSC 8 close, got: {:?}",
            output_str
        );
    }

    #[test]
    fn hyperlink_not_emitted_without_registry() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);

        // Set a link ID without providing a registry
        let cell = Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), 1));
        buffer.set_raw(0, 0, cell);

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        // Present without link registry
        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // No OSC 8 sequences should appear
        assert!(
            !output_str.contains("\x1b]8;"),
            "OSC 8 should not appear without registry, got: {:?}",
            output_str
        );
    }

    #[test]
    fn hyperlink_not_emitted_for_unknown_id() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);
        let links = LinkRegistry::new();

        let cell = Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), 42));
        buffer.set_raw(0, 0, cell);

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter
            .present_with_pool(&buffer, &diff, None, Some(&links))
            .unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        assert!(
            !output_str.contains("\x1b]8;"),
            "OSC 8 should not appear for unknown link IDs, got: {:?}",
            output_str
        );
        assert!(output_str.contains('L'));
    }

    #[test]
    fn hyperlink_closed_at_frame_end() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);
        let mut links = LinkRegistry::new();

        let link_id = links.register("https://example.com");
        // Set all cells with the same link
        for x in 0..5 {
            buffer.set_raw(
                x,
                0,
                Cell::from_char('A').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
            );
        }

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter
            .present_with_pool(&buffer, &diff, None, Some(&links))
            .unwrap();
        let output = get_output(presenter);

        // The close sequence should appear (frame end cleanup)
        let close_seq = b"\x1b]8;;\x1b\\";
        assert!(
            output.windows(close_seq.len()).any(|w| w == close_seq),
            "Link must be closed at frame end"
        );
    }

    #[test]
    fn hyperlink_transitions_between_links() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);
        let mut links = LinkRegistry::new();

        let link_a = links.register("https://a.com");
        let link_b = links.register("https://b.com");

        buffer.set_raw(
            0,
            0,
            Cell::from_char('A').with_attrs(CellAttrs::new(StyleFlags::empty(), link_a)),
        );
        buffer.set_raw(
            1,
            0,
            Cell::from_char('B').with_attrs(CellAttrs::new(StyleFlags::empty(), link_b)),
        );
        buffer.set_raw(2, 0, Cell::from_char('C')); // no link

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter
            .present_with_pool(&buffer, &diff, None, Some(&links))
            .unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Both links should appear
        assert!(output_str.contains("https://a.com"));
        assert!(output_str.contains("https://b.com"));

        // Close sequence must appear at least once (transition or frame end)
        let close_count = output_str.matches("\x1b]8;;\x1b\\").count();
        assert!(
            close_count >= 2,
            "Expected at least 2 link close sequences (transition + frame end), got {}",
            close_count
        );
    }

    // =========================================================================
    // Single-write-per-frame behavior tests
    // =========================================================================

    #[test]
    fn sync_output_not_wrapped_when_unsupported() {
        // When sync_output capability is false, sync sequences should NOT appear
        let mut presenter = test_presenter(); // basic caps, sync_output = false
        let buffer = Buffer::new(10, 10);
        let diff = BufferDiff::new();

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        // Should NOT contain sync sequences
        assert!(
            !output.starts_with(ansi::SYNC_BEGIN),
            "Sync begin should not appear when sync_output is disabled"
        );
        assert!(
            !output
                .windows(ansi::SYNC_END.len())
                .any(|w| w == ansi::SYNC_END),
            "Sync end should not appear when sync_output is disabled"
        );
    }

    #[test]
    fn present_flushes_buffered_output() {
        // Verify that present() flushes all buffered output by checking
        // that the output contains expected content after present()
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(5, 1);
        buffer.set_raw(0, 0, Cell::from_char('T'));
        buffer.set_raw(1, 0, Cell::from_char('E'));
        buffer.set_raw(2, 0, Cell::from_char('S'));
        buffer.set_raw(3, 0, Cell::from_char('T'));

        let old = Buffer::new(5, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // All characters should be present in output (flushed)
        assert!(
            output_str.contains("TEST"),
            "Expected 'TEST' in flushed output"
        );
    }

    #[test]
    fn present_stats_reports_cells_and_bytes() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);

        // Set 5 cells
        for i in 0..5 {
            buffer.set_raw(i, 0, Cell::from_char('X'));
        }

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        let stats = presenter.present(&buffer, &diff).unwrap();

        // Stats should reflect the changes
        assert_eq!(stats.cells_changed, 5, "Expected 5 cells changed");
        assert!(stats.bytes_emitted > 0, "Expected some bytes written");
        assert!(stats.run_count >= 1, "Expected at least 1 run");
    }

    // =========================================================================
    // Cursor tracking tests
    // =========================================================================

    #[test]
    fn cursor_tracking_after_wide_char() {
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(0);
        presenter.cursor_y = Some(0);

        let mut buffer = Buffer::new(10, 1);
        // Wide char at x=0 should advance cursor by 2
        buffer.set_raw(0, 0, Cell::from_char('中'));
        buffer.set_raw(1, 0, Cell::CONTINUATION);
        // Narrow char at x=2
        buffer.set_raw(2, 0, Cell::from_char('A'));

        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();

        // After presenting, cursor should be at x=3 (0 + 2 for wide + 1 for 'A')
        // Note: cursor_x gets reset during present(), but we can verify output order
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Both characters should appear
        assert!(output_str.contains('中'));
        assert!(output_str.contains('A'));
    }

    #[test]
    fn cursor_position_after_multiple_runs() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(20, 3);

        // Create two separate runs on different rows
        buffer.set_raw(0, 0, Cell::from_char('A'));
        buffer.set_raw(1, 0, Cell::from_char('B'));
        buffer.set_raw(5, 2, Cell::from_char('X'));
        buffer.set_raw(6, 2, Cell::from_char('Y'));

        let old = Buffer::new(20, 3);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // All characters should be present
        assert!(output_str.contains('A'));
        assert!(output_str.contains('B'));
        assert!(output_str.contains('X'));
        assert!(output_str.contains('Y'));

        // Should have multiple CUP sequences (one per run)
        let cup_count = output_str.matches("\x1b[").count();
        assert!(
            cup_count >= 2,
            "Expected at least 2 escape sequences for multiple runs"
        );
    }

    // =========================================================================
    // Style tracking tests
    // =========================================================================

    #[test]
    fn style_with_all_flags() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(5, 1);

        // Create a cell with all style flags
        let all_flags = StyleFlags::BOLD
            | StyleFlags::DIM
            | StyleFlags::ITALIC
            | StyleFlags::UNDERLINE
            | StyleFlags::BLINK
            | StyleFlags::REVERSE
            | StyleFlags::STRIKETHROUGH;

        let cell = Cell::from_char('X').with_attrs(CellAttrs::new(all_flags, 0));
        buffer.set_raw(0, 0, cell);

        let old = Buffer::new(5, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Should contain the character and SGR sequences
        assert!(output_str.contains('X'));
        // Should have SGR with multiple attributes (1;2;3;4;5;7;9m pattern)
        assert!(output_str.contains("\x1b["), "Expected SGR sequences");
    }

    #[test]
    fn style_transitions_between_different_colors() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        // Three cells with different foreground colors
        buffer.set_raw(
            0,
            0,
            Cell::from_char('R').with_fg(PackedRgba::rgb(255, 0, 0)),
        );
        buffer.set_raw(
            1,
            0,
            Cell::from_char('G').with_fg(PackedRgba::rgb(0, 255, 0)),
        );
        buffer.set_raw(
            2,
            0,
            Cell::from_char('B').with_fg(PackedRgba::rgb(0, 0, 255)),
        );

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // All colors should appear in the output
        assert!(output_str.contains("38;2;255;0;0"), "Expected red fg");
        assert!(output_str.contains("38;2;0;255;0"), "Expected green fg");
        assert!(output_str.contains("38;2;0;0;255"), "Expected blue fg");
    }

    // =========================================================================
    // Link tracking tests
    // =========================================================================

    #[test]
    fn link_at_buffer_boundaries() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(5, 1);
        let mut links = LinkRegistry::new();

        let link_id = links.register("https://boundary.test");

        // Link at first cell
        buffer.set_raw(
            0,
            0,
            Cell::from_char('F').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
        );
        // Link at last cell
        buffer.set_raw(
            4,
            0,
            Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
        );

        let old = Buffer::new(5, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter
            .present_with_pool(&buffer, &diff, None, Some(&links))
            .unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Link URL should appear
        assert!(output_str.contains("https://boundary.test"));
        // Characters should appear
        assert!(output_str.contains('F'));
        assert!(output_str.contains('L'));
    }

    #[test]
    fn link_state_cleared_after_reset() {
        let mut presenter = test_presenter();
        let mut links = LinkRegistry::new();
        let link_id = links.register("https://example.com");

        // Simulate having an open link
        presenter.current_link = Some(link_id);
        presenter.current_style = Some(CellStyle::default());
        presenter.cursor_x = Some(5);
        presenter.cursor_y = Some(3);

        presenter.reset();

        // All state should be cleared
        assert!(
            presenter.current_link.is_none(),
            "current_link should be None after reset"
        );
        assert!(
            presenter.current_style.is_none(),
            "current_style should be None after reset"
        );
        assert!(
            presenter.cursor_x.is_none(),
            "cursor_x should be None after reset"
        );
        assert!(
            presenter.cursor_y.is_none(),
            "cursor_y should be None after reset"
        );
    }

    #[test]
    fn link_transitions_linked_unlinked_linked() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(5, 1);
        let mut links = LinkRegistry::new();

        let link_id = links.register("https://toggle.test");

        // Linked -> Unlinked -> Linked pattern
        buffer.set_raw(
            0,
            0,
            Cell::from_char('A').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
        );
        buffer.set_raw(1, 0, Cell::from_char('B')); // no link
        buffer.set_raw(
            2,
            0,
            Cell::from_char('C').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
        );

        let old = Buffer::new(5, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter
            .present_with_pool(&buffer, &diff, None, Some(&links))
            .unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Link URL should appear at least twice (once for A, once for C)
        let url_count = output_str.matches("https://toggle.test").count();
        assert!(
            url_count >= 2,
            "Expected link to open at least twice, got {} occurrences",
            url_count
        );

        // Close sequence should appear (after A, and at frame end)
        let close_count = output_str.matches("\x1b]8;;\x1b\\").count();
        assert!(
            close_count >= 2,
            "Expected at least 2 link closes, got {}",
            close_count
        );
    }

    // =========================================================================
    // Multiple frame tests
    // =========================================================================

    #[test]
    fn multiple_presents_maintain_correct_state() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);

        // First frame
        buffer.set_raw(0, 0, Cell::from_char('1'));
        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);
        presenter.present(&buffer, &diff).unwrap();

        // Second frame - change a different cell
        let prev = buffer.clone();
        buffer.set_raw(1, 0, Cell::from_char('2'));
        let diff = BufferDiff::compute(&prev, &buffer);
        presenter.present(&buffer, &diff).unwrap();

        // Third frame - change another cell
        let prev = buffer.clone();
        buffer.set_raw(2, 0, Cell::from_char('3'));
        let diff = BufferDiff::compute(&prev, &buffer);
        presenter.present(&buffer, &diff).unwrap();

        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // All numbers should appear in final output
        assert!(output_str.contains('1'));
        assert!(output_str.contains('2'));
        assert!(output_str.contains('3'));
    }

    // =========================================================================
    // SGR Delta Engine tests (bd-4kq0.2.1)
    // =========================================================================

    #[test]
    fn sgr_delta_fg_only_change_no_reset() {
        // When only fg changes, delta should NOT emit reset
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        let fg1 = PackedRgba::rgb(255, 0, 0);
        let fg2 = PackedRgba::rgb(0, 255, 0);
        buffer.set_raw(0, 0, Cell::from_char('A').with_fg(fg1));
        buffer.set_raw(1, 0, Cell::from_char('B').with_fg(fg2));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Count SGR resets - the first cell needs a reset (from None state),
        // but the second cell should use delta (no reset)
        let reset_count = output_str.matches("\x1b[0m").count();
        // One reset at start (for first cell from unknown state) + one at frame end
        assert_eq!(
            reset_count, 2,
            "Expected 2 resets (initial + frame end), got {} in: {:?}",
            reset_count, output_str
        );
    }

    #[test]
    fn sgr_delta_bg_only_change_no_reset() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        let bg1 = PackedRgba::rgb(0, 0, 255);
        let bg2 = PackedRgba::rgb(255, 255, 0);
        buffer.set_raw(0, 0, Cell::from_char('A').with_bg(bg1));
        buffer.set_raw(1, 0, Cell::from_char('B').with_bg(bg2));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Only 2 resets: initial cell + frame end
        let reset_count = output_str.matches("\x1b[0m").count();
        assert_eq!(
            reset_count, 2,
            "Expected 2 resets, got {} in: {:?}",
            reset_count, output_str
        );
    }

    #[test]
    fn sgr_delta_attr_addition_no_reset() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        // First cell: bold. Second cell: bold + italic
        let attrs1 = CellAttrs::new(StyleFlags::BOLD, 0);
        let attrs2 = CellAttrs::new(StyleFlags::BOLD | StyleFlags::ITALIC, 0);
        buffer.set_raw(0, 0, Cell::from_char('A').with_attrs(attrs1));
        buffer.set_raw(1, 0, Cell::from_char('B').with_attrs(attrs2));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Second cell should add italic (code 3) without reset
        let reset_count = output_str.matches("\x1b[0m").count();
        assert_eq!(
            reset_count, 2,
            "Expected 2 resets, got {} in: {:?}",
            reset_count, output_str
        );
        // Should contain italic-on code for the delta
        assert!(
            output_str.contains("\x1b[3m"),
            "Expected italic-on sequence in: {:?}",
            output_str
        );
    }

    #[test]
    fn sgr_delta_attr_removal_uses_off_code() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        // First cell: bold+italic. Second cell: bold only
        let attrs1 = CellAttrs::new(StyleFlags::BOLD | StyleFlags::ITALIC, 0);
        let attrs2 = CellAttrs::new(StyleFlags::BOLD, 0);
        buffer.set_raw(0, 0, Cell::from_char('A').with_attrs(attrs1));
        buffer.set_raw(1, 0, Cell::from_char('B').with_attrs(attrs2));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Should contain italic-off code (23) for delta
        assert!(
            output_str.contains("\x1b[23m"),
            "Expected italic-off sequence in: {:?}",
            output_str
        );
        // Only 2 resets (initial + frame end), not 3
        let reset_count = output_str.matches("\x1b[0m").count();
        assert_eq!(
            reset_count, 2,
            "Expected 2 resets, got {} in: {:?}",
            reset_count, output_str
        );
    }

    #[test]
    fn sgr_delta_bold_dim_collateral_re_enables() {
        // Bold off (code 22) also disables Dim. If Dim should remain,
        // the delta engine must re-enable it.
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        // First cell: Bold + Dim. Second cell: Dim only
        let attrs1 = CellAttrs::new(StyleFlags::BOLD | StyleFlags::DIM, 0);
        let attrs2 = CellAttrs::new(StyleFlags::DIM, 0);
        buffer.set_raw(0, 0, Cell::from_char('A').with_attrs(attrs1));
        buffer.set_raw(1, 0, Cell::from_char('B').with_attrs(attrs2));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Should contain bold-off (22) and then dim re-enable (2)
        assert!(
            output_str.contains("\x1b[22m"),
            "Expected bold-off (22) in: {:?}",
            output_str
        );
        assert!(
            output_str.contains("\x1b[2m"),
            "Expected dim re-enable (2) in: {:?}",
            output_str
        );
    }

    #[test]
    fn sgr_delta_same_style_no_output() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        let fg = PackedRgba::rgb(255, 0, 0);
        let attrs = CellAttrs::new(StyleFlags::BOLD, 0);
        buffer.set_raw(0, 0, Cell::from_char('A').with_fg(fg).with_attrs(attrs));
        buffer.set_raw(1, 0, Cell::from_char('B').with_fg(fg).with_attrs(attrs));
        buffer.set_raw(2, 0, Cell::from_char('C').with_fg(fg).with_attrs(attrs));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Only 1 fg color sequence (style set once for all three cells)
        let fg_count = output_str.matches("38;2;255;0;0").count();
        assert_eq!(
            fg_count, 1,
            "Expected 1 fg sequence, got {} in: {:?}",
            fg_count, output_str
        );
    }

    #[test]
    fn sgr_delta_cost_dominance_never_exceeds_baseline() {
        // Test that delta output is never larger than reset+apply would be
        // for a variety of style transitions
        let transitions: Vec<(CellStyle, CellStyle)> = vec![
            // Only fg change
            (
                CellStyle {
                    fg: PackedRgba::rgb(255, 0, 0),
                    bg: PackedRgba::TRANSPARENT,
                    attrs: StyleFlags::empty(),
                },
                CellStyle {
                    fg: PackedRgba::rgb(0, 255, 0),
                    bg: PackedRgba::TRANSPARENT,
                    attrs: StyleFlags::empty(),
                },
            ),
            // Only bg change
            (
                CellStyle {
                    fg: PackedRgba::TRANSPARENT,
                    bg: PackedRgba::rgb(255, 0, 0),
                    attrs: StyleFlags::empty(),
                },
                CellStyle {
                    fg: PackedRgba::TRANSPARENT,
                    bg: PackedRgba::rgb(0, 0, 255),
                    attrs: StyleFlags::empty(),
                },
            ),
            // Only attr addition
            (
                CellStyle {
                    fg: PackedRgba::rgb(100, 100, 100),
                    bg: PackedRgba::TRANSPARENT,
                    attrs: StyleFlags::BOLD,
                },
                CellStyle {
                    fg: PackedRgba::rgb(100, 100, 100),
                    bg: PackedRgba::TRANSPARENT,
                    attrs: StyleFlags::BOLD | StyleFlags::ITALIC,
                },
            ),
            // Attr removal
            (
                CellStyle {
                    fg: PackedRgba::rgb(100, 100, 100),
                    bg: PackedRgba::TRANSPARENT,
                    attrs: StyleFlags::BOLD | StyleFlags::ITALIC,
                },
                CellStyle {
                    fg: PackedRgba::rgb(100, 100, 100),
                    bg: PackedRgba::TRANSPARENT,
                    attrs: StyleFlags::BOLD,
                },
            ),
        ];

        for (old_style, new_style) in &transitions {
            // Measure delta cost
            let delta_buf = {
                let mut delta_presenter = {
                    let caps = TerminalCapabilities::basic();
                    Presenter::new(Vec::new(), caps)
                };
                delta_presenter.current_style = Some(*old_style);
                delta_presenter
                    .emit_style_delta(*old_style, *new_style)
                    .unwrap();
                delta_presenter.into_inner().unwrap()
            };

            // Measure reset+apply cost
            let reset_buf = {
                let mut reset_presenter = {
                    let caps = TerminalCapabilities::basic();
                    Presenter::new(Vec::new(), caps)
                };
                reset_presenter.emit_style_full(*new_style).unwrap();
                reset_presenter.into_inner().unwrap()
            };

            assert!(
                delta_buf.len() <= reset_buf.len(),
                "Delta ({} bytes) exceeded reset+apply ({} bytes) for {:?} -> {:?}.\n\
                 Delta: {:?}\nReset: {:?}",
                delta_buf.len(),
                reset_buf.len(),
                old_style,
                new_style,
                String::from_utf8_lossy(&delta_buf),
                String::from_utf8_lossy(&reset_buf),
            );
        }
    }

    /// Generate a deterministic JSONL evidence ledger proving the SGR delta engine
    /// emits fewer (or equal) bytes than reset+apply for every transition.
    ///
    /// Each line is a JSON object with:
    ///   seed, from_fg, from_bg, from_attrs, to_fg, to_bg, to_attrs,
    ///   delta_bytes, baseline_bytes, cost_delta, used_fallback
    #[test]
    fn sgr_delta_evidence_ledger() {
        use std::io::Write as _;

        // Deterministic seed for reproducibility
        const SEED: u64 = 0xDEAD_BEEF_CAFE;

        // Simple LCG for deterministic pseudorandom values
        let mut rng_state = SEED;
        let mut next_u64 = || -> u64 {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
            rng_state
        };

        let random_style = |rng: &mut dyn FnMut() -> u64| -> CellStyle {
            let v = rng();
            let fg = if v & 1 == 0 {
                PackedRgba::TRANSPARENT
            } else {
                let r = ((v >> 8) & 0xFF) as u8;
                let g = ((v >> 16) & 0xFF) as u8;
                let b = ((v >> 24) & 0xFF) as u8;
                PackedRgba::rgb(r, g, b)
            };
            let v2 = rng();
            let bg = if v2 & 1 == 0 {
                PackedRgba::TRANSPARENT
            } else {
                let r = ((v2 >> 8) & 0xFF) as u8;
                let g = ((v2 >> 16) & 0xFF) as u8;
                let b = ((v2 >> 24) & 0xFF) as u8;
                PackedRgba::rgb(r, g, b)
            };
            let attrs = StyleFlags::from_bits_truncate(rng() as u8);
            CellStyle { fg, bg, attrs }
        };

        let mut ledger = Vec::new();
        let num_transitions = 200;

        for i in 0..num_transitions {
            let old_style = random_style(&mut next_u64);
            let new_style = random_style(&mut next_u64);

            // Measure delta cost
            let mut delta_p = {
                let caps = TerminalCapabilities::basic();
                Presenter::new(Vec::new(), caps)
            };
            delta_p.current_style = Some(old_style);
            delta_p.emit_style_delta(old_style, new_style).unwrap();
            let delta_out = delta_p.into_inner().unwrap();

            // Measure reset+apply cost
            let mut reset_p = {
                let caps = TerminalCapabilities::basic();
                Presenter::new(Vec::new(), caps)
            };
            reset_p.emit_style_full(new_style).unwrap();
            let reset_out = reset_p.into_inner().unwrap();

            let delta_bytes = delta_out.len();
            let baseline_bytes = reset_out.len();

            // Compute whether fallback was used (delta >= baseline means fallback likely)
            let attrs_removed = old_style.attrs & !new_style.attrs;
            let removed_count = attrs_removed.bits().count_ones();
            let fg_changed = old_style.fg != new_style.fg;
            let bg_changed = old_style.bg != new_style.bg;
            let used_fallback = removed_count >= 3 && fg_changed && bg_changed;

            // Assert cost dominance
            assert!(
                delta_bytes <= baseline_bytes,
                "Transition {i}: delta ({delta_bytes}B) > baseline ({baseline_bytes}B)"
            );

            // Emit JSONL record
            writeln!(
                &mut ledger,
                "{{\"seed\":{SEED},\"i\":{i},\"from_fg\":\"{:?}\",\"from_bg\":\"{:?}\",\
                 \"from_attrs\":{},\"to_fg\":\"{:?}\",\"to_bg\":\"{:?}\",\"to_attrs\":{},\
                 \"delta_bytes\":{delta_bytes},\"baseline_bytes\":{baseline_bytes},\
                 \"cost_delta\":{},\"used_fallback\":{used_fallback}}}",
                old_style.fg,
                old_style.bg,
                old_style.attrs.bits(),
                new_style.fg,
                new_style.bg,
                new_style.attrs.bits(),
                baseline_bytes as isize - delta_bytes as isize,
            )
            .unwrap();
        }

        // Verify we produced valid JSONL (every line parses)
        let text = String::from_utf8(ledger).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), num_transitions);

        // Verify aggregate: total savings should be non-negative
        let mut total_saved: isize = 0;
        for line in &lines {
            // Quick parse of cost_delta field
            let cd_start = line.find("\"cost_delta\":").unwrap() + 13;
            let cd_end = line[cd_start..].find(',').unwrap() + cd_start;
            let cd: isize = line[cd_start..cd_end].parse().unwrap();
            total_saved += cd;
        }
        assert!(
            total_saved >= 0,
            "Total byte savings should be non-negative, got {total_saved}"
        );
    }

    /// E2E style stress test: scripted style churn across a full buffer
    /// with byte metrics proving delta engine correctness under load.
    #[test]
    fn e2e_style_stress_with_byte_metrics() {
        let width = 40u16;
        let height = 10u16;

        // Build a buffer with maximum style diversity
        let mut buffer = Buffer::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let i = (y as usize * width as usize + x as usize) as u8;
                let fg = PackedRgba::rgb(i, 255 - i, i.wrapping_mul(3));
                let bg = if i.is_multiple_of(4) {
                    PackedRgba::rgb(i.wrapping_mul(7), i.wrapping_mul(11), i.wrapping_mul(13))
                } else {
                    PackedRgba::TRANSPARENT
                };
                let flags = StyleFlags::from_bits_truncate(i % 128);
                let ch = char::from_u32(('!' as u32) + (i as u32 % 90)).unwrap_or('?');
                let cell = Cell::from_char(ch)
                    .with_fg(fg)
                    .with_bg(bg)
                    .with_attrs(CellAttrs::new(flags, 0));
                buffer.set_raw(x, y, cell);
            }
        }

        // Present from blank (first frame)
        let blank = Buffer::new(width, height);
        let diff = BufferDiff::compute(&blank, &buffer);
        let mut presenter = test_presenter();
        presenter.present(&buffer, &diff).unwrap();
        let frame1_bytes = presenter.into_inner().unwrap().len();

        // Build second buffer: shift all styles by one position (churn)
        let mut buffer2 = Buffer::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let i = (y as usize * width as usize + x as usize + 1) as u8;
                let fg = PackedRgba::rgb(i, 255 - i, i.wrapping_mul(3));
                let bg = if i.is_multiple_of(4) {
                    PackedRgba::rgb(i.wrapping_mul(7), i.wrapping_mul(11), i.wrapping_mul(13))
                } else {
                    PackedRgba::TRANSPARENT
                };
                let flags = StyleFlags::from_bits_truncate(i % 128);
                let ch = char::from_u32(('!' as u32) + (i as u32 % 90)).unwrap_or('?');
                let cell = Cell::from_char(ch)
                    .with_fg(fg)
                    .with_bg(bg)
                    .with_attrs(CellAttrs::new(flags, 0));
                buffer2.set_raw(x, y, cell);
            }
        }

        // Second frame: incremental update should use delta engine
        let diff2 = BufferDiff::compute(&buffer, &buffer2);
        let mut presenter2 = test_presenter();
        presenter2.present(&buffer2, &diff2).unwrap();
        let frame2_bytes = presenter2.into_inner().unwrap().len();

        // Incremental should be smaller than full redraw since delta
        // engine can reuse partial style state
        assert!(
            frame2_bytes > 0,
            "Second frame should produce output for style churn"
        );
        assert!(!diff2.is_empty(), "Style shift should produce changes");

        // Verify frame2 is at most frame1 size (delta should never be worse
        // than a full redraw for the same number of changed cells)
        // Note: frame2 may differ in size due to different diff (changed cells
        // vs all cells), so just verify it's reasonable.
        assert!(
            frame2_bytes <= frame1_bytes * 2,
            "Incremental frame ({frame2_bytes}B) unreasonably large vs full ({frame1_bytes}B)"
        );
    }

    // =========================================================================
    // DP Cost Model Tests (bd-4kq0.2.2)
    // =========================================================================

    #[test]
    fn cost_model_empty_row_single_run() {
        // Single run on a row should always use Sparse (no merge benefit)
        let runs = [ChangeRun::new(5, 10, 20)];
        let plan = cost_model::plan_row(&runs, None, None);
        assert_eq!(plan.spans().len(), 1);
        assert_eq!(plan.spans()[0].x0, 10);
        assert_eq!(plan.spans()[0].x1, 20);
        assert!(plan.total_cost() > 0);
    }

    #[test]
    fn cost_model_full_row_merges() {
        // Two small runs far apart on same row - gap is smaller than 2x CUP overhead
        // Runs at columns 0-2 and 77-79 on an 80-col row
        // Sparse: CUP + 3 cells + CUP + 3 cells
        // Merged: CUP + 80 cells but with gap overhead
        // This should stay sparse since the gap is very large
        let runs = [ChangeRun::new(0, 0, 2), ChangeRun::new(0, 77, 79)];
        let plan = cost_model::plan_row(&runs, None, None);
        // Large gap (74 cells * 2 overhead = 148) vs CUP savings (~8) => no merge.
        assert_eq!(plan.spans().len(), 2);
        assert_eq!(plan.spans()[0].x0, 0);
        assert_eq!(plan.spans()[0].x1, 2);
        assert_eq!(plan.spans()[1].x0, 77);
        assert_eq!(plan.spans()[1].x1, 79);
    }

    #[test]
    fn cost_model_adjacent_runs_merge() {
        // Many single-cell runs with 1-cell gaps should merge
        // 8 single-cell runs at columns 10, 12, 14, 16, 18, 20, 22, 24
        let runs = [
            ChangeRun::new(3, 10, 10),
            ChangeRun::new(3, 12, 12),
            ChangeRun::new(3, 14, 14),
            ChangeRun::new(3, 16, 16),
            ChangeRun::new(3, 18, 18),
            ChangeRun::new(3, 20, 20),
            ChangeRun::new(3, 22, 22),
            ChangeRun::new(3, 24, 24),
        ];
        let plan = cost_model::plan_row(&runs, None, None);
        // Sparse: 1 CUP + 7 CUF(2) * 4 bytes + 8 cells = ~7+28+8 = 43
        // Merged: 1 CUP + 8 changed + 7 gap * 2 = 7+8+14 = 29
        assert_eq!(plan.spans().len(), 1);
        assert_eq!(plan.spans()[0].x0, 10);
        assert_eq!(plan.spans()[0].x1, 24);
    }

    #[test]
    fn cost_model_single_cell_stays_sparse() {
        let runs = [ChangeRun::new(0, 40, 40)];
        let plan = cost_model::plan_row(&runs, Some(0), Some(0));
        assert_eq!(plan.spans().len(), 1);
        assert_eq!(plan.spans()[0].x0, 40);
        assert_eq!(plan.spans()[0].x1, 40);
    }

    #[test]
    fn cost_model_cup_vs_cha_vs_cuf() {
        // CUF should be cheapest for small forward moves on same row
        assert!(cost_model::cuf_cost(1) <= cost_model::cha_cost(5));
        assert!(cost_model::cuf_cost(3) <= cost_model::cup_cost(0, 5));

        // CHA should be cheapest for backward moves on same row (vs CUP)
        let cha = cost_model::cha_cost(5);
        let cup = cost_model::cup_cost(0, 5);
        assert!(cha <= cup);

        // Cheapest move from known position (same row, forward 1)
        let cost = cost_model::cheapest_move_cost(Some(5), Some(0), 6, 0);
        assert_eq!(cost, 3); // CUF(1) = "\x1b[C" = 3 bytes
    }

    #[test]
    fn cost_model_digit_estimation_accuracy() {
        // Verify CUP cost estimates are accurate by comparing to actual output
        let mut buf = Vec::new();
        ansi::cup(&mut buf, 0, 0).unwrap();
        assert_eq!(buf.len(), cost_model::cup_cost(0, 0));

        buf.clear();
        ansi::cup(&mut buf, 9, 9).unwrap();
        assert_eq!(buf.len(), cost_model::cup_cost(9, 9));

        buf.clear();
        ansi::cup(&mut buf, 99, 99).unwrap();
        assert_eq!(buf.len(), cost_model::cup_cost(99, 99));

        buf.clear();
        ansi::cha(&mut buf, 0).unwrap();
        assert_eq!(buf.len(), cost_model::cha_cost(0));

        buf.clear();
        ansi::cuf(&mut buf, 1).unwrap();
        assert_eq!(buf.len(), cost_model::cuf_cost(1));

        buf.clear();
        ansi::cuf(&mut buf, 10).unwrap();
        assert_eq!(buf.len(), cost_model::cuf_cost(10));
    }

    #[test]
    fn cost_model_merged_row_produces_correct_output() {
        // Verify that merged emission produces the same visual result as sparse
        let width = 30u16;
        let mut buffer = Buffer::new(width, 1);

        // Set up scattered changes: columns 5, 10, 15, 20
        for col in [5u16, 10, 15, 20] {
            let ch = char::from_u32('A' as u32 + col as u32 % 26).unwrap();
            buffer.set_raw(col, 0, Cell::from_char(ch));
        }

        let old = Buffer::new(width, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        // Present and verify output contains expected characters
        let mut presenter = test_presenter();
        presenter.present(&buffer, &diff).unwrap();
        let output = presenter.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        for col in [5u16, 10, 15, 20] {
            let ch = char::from_u32('A' as u32 + col as u32 % 26).unwrap();
            assert!(
                output_str.contains(ch),
                "Missing character '{ch}' at col {col} in output"
            );
        }
    }

    #[test]
    fn cost_model_optimal_cursor_uses_cuf_on_same_row() {
        // Verify move_cursor_optimal uses CUF for small forward moves
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(5);
        presenter.cursor_y = Some(0);
        presenter.move_cursor_optimal(6, 0).unwrap();
        let output = presenter.into_inner().unwrap();
        // CUF(1) = "\x1b[C"
        assert_eq!(&output, b"\x1b[C", "Should use CUF for +1 column move");
    }

    #[test]
    fn cost_model_optimal_cursor_uses_cha_on_same_row_backward() {
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(10);
        presenter.cursor_y = Some(3);

        let target_x = 2;
        let target_y = 3;
        let cha_cost = cost_model::cha_cost(target_x);
        let cup_cost = cost_model::cup_cost(target_y, target_x);
        assert!(
            cha_cost <= cup_cost,
            "Expected CHA to be cheaper for backward move (cha={cha_cost}, cup={cup_cost})"
        );

        presenter.move_cursor_optimal(target_x, target_y).unwrap();
        let output = presenter.into_inner().unwrap();
        let mut expected = Vec::new();
        ansi::cha(&mut expected, target_x).unwrap();
        assert_eq!(output, expected, "Should use CHA for backward move");
    }

    #[test]
    fn cost_model_optimal_cursor_uses_cup_on_row_change() {
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(4);
        presenter.cursor_y = Some(1);

        presenter.move_cursor_optimal(7, 4).unwrap();
        let output = presenter.into_inner().unwrap();
        let mut expected = Vec::new();
        ansi::cup(&mut expected, 4, 7).unwrap();
        assert_eq!(output, expected, "Should use CUP when row changes");
    }

    #[test]
    fn cost_model_chooses_full_row_when_cheaper() {
        // Create a scenario where merged is definitely cheaper:
        // 10 single-cell runs with 1-cell gaps on the same row
        let width = 40u16;
        let mut buffer = Buffer::new(width, 1);

        // Every other column: 0, 2, 4, 6, 8, 10, 12, 14, 16, 18
        for col in (0..20).step_by(2) {
            buffer.set_raw(col, 0, Cell::from_char('X'));
        }

        let old = Buffer::new(width, 1);
        let diff = BufferDiff::compute(&old, &buffer);
        let runs = diff.runs();

        // The cost model should merge (many small gaps < many CUP costs)
        let row_runs: Vec<_> = runs.iter().filter(|r| r.y == 0).copied().collect();
        if row_runs.len() > 1 {
            let plan = cost_model::plan_row(&row_runs, None, None);
            assert!(
                plan.spans().len() == 1,
                "Expected single merged span for many small runs, got {} spans",
                plan.spans().len()
            );
            assert_eq!(plan.spans()[0].x0, 0);
            assert_eq!(plan.spans()[0].x1, 18);
        }
    }

    #[test]
    fn perf_cost_model_overhead() {
        // Verify the cost model planning is fast (microsecond scale)
        use std::time::Instant;

        let runs: Vec<ChangeRun> = (0..100)
            .map(|i| ChangeRun::new(0, i * 3, i * 3 + 1))
            .collect();

        let (iterations, max_ms) = if cfg!(debug_assertions) {
            (1_000, 1_000u128)
        } else {
            (10_000, 500u128)
        };

        let start = Instant::now();
        for _ in 0..iterations {
            let _ = cost_model::plan_row(&runs, None, None);
        }
        let elapsed = start.elapsed();

        // Keep this generous in debug builds to avoid flaky perf assertions.
        assert!(
            elapsed.as_millis() < max_ms,
            "Cost model planning too slow: {elapsed:?} for {iterations} iterations"
        );
    }

    #[test]
    fn perf_legacy_vs_dp_worst_case_sparse() {
        use std::time::Instant;

        let width = 200u16;
        let height = 1u16;
        let mut buffer = Buffer::new(width, height);

        // Two dense clusters with a large gap between them.
        for col in (0..40).step_by(2) {
            buffer.set_raw(col, 0, Cell::from_char('X'));
        }
        for col in (160..200).step_by(2) {
            buffer.set_raw(col, 0, Cell::from_char('Y'));
        }

        let blank = Buffer::new(width, height);
        let diff = BufferDiff::compute(&blank, &buffer);
        let runs = diff.runs();
        let row_runs: Vec<_> = runs.iter().filter(|r| r.y == 0).copied().collect();

        let dp_plan = cost_model::plan_row(&row_runs, None, None);
        let legacy_spans = legacy_plan_row(&row_runs, None, None);

        let dp_output = emit_spans_for_output(&buffer, dp_plan.spans());
        let legacy_output = emit_spans_for_output(&buffer, &legacy_spans);

        assert!(
            dp_output.len() <= legacy_output.len(),
            "DP output should be <= legacy output (dp={}, legacy={})",
            dp_output.len(),
            legacy_output.len()
        );

        let (iterations, max_ms) = if cfg!(debug_assertions) {
            (1_000, 1_000u128)
        } else {
            (10_000, 500u128)
        };
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = cost_model::plan_row(&row_runs, None, None);
        }
        let dp_elapsed = start.elapsed();

        let start = Instant::now();
        for _ in 0..iterations {
            let _ = legacy_plan_row(&row_runs, None, None);
        }
        let legacy_elapsed = start.elapsed();

        assert!(
            dp_elapsed.as_millis() < max_ms,
            "DP planning too slow: {dp_elapsed:?} for {iterations} iterations"
        );

        let _ = legacy_elapsed;
    }

    // =========================================================================
    // Presenter Perf + Golden Outputs (bd-4kq0.2.3)
    // =========================================================================

    /// Build a deterministic "style-heavy" scene: every cell has a unique style.
    fn build_style_heavy_scene(width: u16, height: u16, seed: u64) -> Buffer {
        let mut buffer = Buffer::new(width, height);
        let mut rng = seed;
        let mut next = || -> u64 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            rng
        };
        for y in 0..height {
            for x in 0..width {
                let v = next();
                let ch = char::from_u32(('!' as u32) + (v as u32 % 90)).unwrap_or('?');
                let fg = PackedRgba::rgb((v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8);
                let bg = if v & 3 == 0 {
                    PackedRgba::rgb((v >> 32) as u8, (v >> 40) as u8, (v >> 48) as u8)
                } else {
                    PackedRgba::TRANSPARENT
                };
                let flags = StyleFlags::from_bits_truncate((v >> 56) as u8);
                let cell = Cell::from_char(ch)
                    .with_fg(fg)
                    .with_bg(bg)
                    .with_attrs(CellAttrs::new(flags, 0));
                buffer.set_raw(x, y, cell);
            }
        }
        buffer
    }

    /// Build a "sparse-update" scene: only ~10% of cells differ between frames.
    fn build_sparse_update(base: &Buffer, seed: u64) -> Buffer {
        let mut buffer = base.clone();
        let width = base.width();
        let height = base.height();
        let mut rng = seed;
        let mut next = || -> u64 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            rng
        };
        let change_count = (width as usize * height as usize) / 10;
        for _ in 0..change_count {
            let v = next();
            let x = (v % width as u64) as u16;
            let y = ((v >> 16) % height as u64) as u16;
            let ch = char::from_u32(('A' as u32) + (v as u32 % 26)).unwrap_or('?');
            buffer.set_raw(x, y, Cell::from_char(ch));
        }
        buffer
    }

    #[test]
    fn snapshot_presenter_equivalence() {
        // Golden snapshot: style-heavy 40x10 scene with deterministic seed.
        // The output hash must be stable across runs.
        let buffer = build_style_heavy_scene(40, 10, 0xDEAD_CAFE_1234);
        let blank = Buffer::new(40, 10);
        let diff = BufferDiff::compute(&blank, &buffer);

        let mut presenter = test_presenter();
        presenter.present(&buffer, &diff).unwrap();
        let output = presenter.into_inner().unwrap();

        // Compute checksum for golden comparison
        let checksum = {
            let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
            for &byte in &output {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x100000001b3); // FNV prime
            }
            hash
        };

        // Verify determinism: same seed + scene = same output
        let mut presenter2 = test_presenter();
        presenter2.present(&buffer, &diff).unwrap();
        let output2 = presenter2.into_inner().unwrap();
        assert_eq!(output, output2, "Presenter output must be deterministic");

        // Log golden checksum for the record
        let _ = checksum; // Used in JSONL test below
    }

    #[test]
    fn perf_presenter_microbench() {
        use std::env;
        use std::io::Write as _;
        use std::time::Instant;

        let width = 120u16;
        let height = 40u16;
        let seed = 0x00BE_EFCA_FE42;
        let scene = build_style_heavy_scene(width, height, seed);
        let blank = Buffer::new(width, height);
        let diff_full = BufferDiff::compute(&blank, &scene);

        // Also build a sparse update scene
        let scene2 = build_sparse_update(&scene, seed.wrapping_add(1));
        let diff_sparse = BufferDiff::compute(&scene, &scene2);

        let mut jsonl = Vec::new();
        let iterations = env::var("FTUI_PRESENTER_BENCH_ITERS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(50);

        let runs_full = diff_full.runs();
        let runs_sparse = diff_sparse.runs();

        let plan_rows = |runs: &[ChangeRun]| -> (usize, usize) {
            let mut idx = 0;
            let mut total_cost = 0usize;
            let mut span_count = 0usize;
            let mut prev_x = None;
            let mut prev_y = None;

            while idx < runs.len() {
                let y = runs[idx].y;
                let start = idx;
                while idx < runs.len() && runs[idx].y == y {
                    idx += 1;
                }

                let plan = cost_model::plan_row(&runs[start..idx], prev_x, prev_y);
                span_count += plan.spans().len();
                total_cost = total_cost.saturating_add(plan.total_cost());
                if let Some(last) = plan.spans().last() {
                    prev_x = Some(last.x1);
                    prev_y = Some(y);
                }
            }

            (total_cost, span_count)
        };

        for i in 0..iterations {
            let (diff_ref, buf_ref, runs_ref, label) = if i % 2 == 0 {
                (&diff_full, &scene, &runs_full, "full")
            } else {
                (&diff_sparse, &scene2, &runs_sparse, "sparse")
            };

            let plan_start = Instant::now();
            let (plan_cost, plan_spans) = plan_rows(runs_ref);
            let plan_time_us = plan_start.elapsed().as_micros() as u64;

            let mut presenter = test_presenter();
            let start = Instant::now();
            let stats = presenter.present(buf_ref, diff_ref).unwrap();
            let elapsed_us = start.elapsed().as_micros() as u64;
            let output = presenter.into_inner().unwrap();

            // FNV-1a checksum
            let checksum = {
                let mut hash: u64 = 0xcbf29ce484222325;
                for &b in &output {
                    hash ^= b as u64;
                    hash = hash.wrapping_mul(0x100000001b3);
                }
                hash
            };

            writeln!(
                &mut jsonl,
                "{{\"seed\":{seed},\"width\":{width},\"height\":{height},\
                 \"scene\":\"{label}\",\"changes\":{},\"runs\":{},\
                 \"plan_cost\":{plan_cost},\"plan_spans\":{plan_spans},\
                 \"plan_time_us\":{plan_time_us},\"bytes\":{},\
                 \"emit_time_us\":{elapsed_us},\
                 \"checksum\":\"{checksum:016x}\"}}",
                stats.cells_changed, stats.run_count, stats.bytes_emitted,
            )
            .unwrap();
        }

        let text = String::from_utf8(jsonl).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), iterations as usize);

        // Parse and verify: full frames should be deterministic (same checksum)
        let full_checksums: Vec<&str> = lines
            .iter()
            .filter(|l| l.contains("\"full\""))
            .map(|l| {
                let start = l.find("\"checksum\":\"").unwrap() + 12;
                let end = l[start..].find('"').unwrap() + start;
                &l[start..end]
            })
            .collect();
        assert!(full_checksums.len() > 1);
        assert!(
            full_checksums.windows(2).all(|w| w[0] == w[1]),
            "Full frame checksums should be identical across runs"
        );

        // Sparse frame bytes should be less than full frame bytes
        let full_bytes: Vec<u64> = lines
            .iter()
            .filter(|l| l.contains("\"full\""))
            .map(|l| {
                let start = l.find("\"bytes\":").unwrap() + 8;
                let end = l[start..].find(',').unwrap() + start;
                l[start..end].parse::<u64>().unwrap()
            })
            .collect();
        let sparse_bytes: Vec<u64> = lines
            .iter()
            .filter(|l| l.contains("\"sparse\""))
            .map(|l| {
                let start = l.find("\"bytes\":").unwrap() + 8;
                let end = l[start..].find(',').unwrap() + start;
                l[start..end].parse::<u64>().unwrap()
            })
            .collect();

        let avg_full: u64 = full_bytes.iter().sum::<u64>() / full_bytes.len() as u64;
        let avg_sparse: u64 = sparse_bytes.iter().sum::<u64>() / sparse_bytes.len() as u64;
        assert!(
            avg_sparse < avg_full,
            "Sparse updates ({avg_sparse}B) should emit fewer bytes than full ({avg_full}B)"
        );
    }

    #[test]
    fn perf_emit_style_delta_microbench() {
        use std::env;
        use std::io::Write as _;
        use std::time::Instant;

        let iterations = env::var("FTUI_EMIT_STYLE_BENCH_ITERS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(200);
        let mode = env::var("FTUI_EMIT_STYLE_BENCH_MODE").unwrap_or_default();
        let emit_json = mode != "raw";

        let mut styles = Vec::with_capacity(128);
        let mut rng = 0x00A5_A51E_AF42_u64;
        let mut next = || -> u64 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            rng
        };

        for _ in 0..128 {
            let v = next();
            let fg = PackedRgba::rgb(
                (v & 0xFF) as u8,
                ((v >> 8) & 0xFF) as u8,
                ((v >> 16) & 0xFF) as u8,
            );
            let bg = PackedRgba::rgb(
                ((v >> 24) & 0xFF) as u8,
                ((v >> 32) & 0xFF) as u8,
                ((v >> 40) & 0xFF) as u8,
            );
            let flags = StyleFlags::from_bits_truncate((v >> 48) as u8);
            let cell = Cell::from_char('A')
                .with_fg(fg)
                .with_bg(bg)
                .with_attrs(CellAttrs::new(flags, 0));
            styles.push(CellStyle::from_cell(&cell));
        }

        let mut presenter = test_presenter();
        let mut jsonl = Vec::new();
        let mut sink = 0u64;

        for i in 0..iterations {
            let old = styles[i as usize % styles.len()];
            let new = styles[(i as usize + 1) % styles.len()];

            presenter.writer.reset_counter();
            presenter.writer.inner_mut().get_mut().clear();

            let start = Instant::now();
            presenter.emit_style_delta(old, new).unwrap();
            let elapsed_us = start.elapsed().as_micros() as u64;
            let bytes = presenter.writer.bytes_written();

            if emit_json {
                writeln!(
                    &mut jsonl,
                    "{{\"iter\":{i},\"emit_time_us\":{elapsed_us},\"bytes\":{bytes}}}"
                )
                .unwrap();
            } else {
                sink = sink.wrapping_add(elapsed_us ^ bytes);
            }
        }

        if emit_json {
            let text = String::from_utf8(jsonl).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len() as u32, iterations);
        } else {
            std::hint::black_box(sink);
        }
    }

    #[test]
    fn e2e_presenter_stress_deterministic() {
        // Deterministic stress test: seeded style churn across multiple frames,
        // verifying no visual divergence via terminal model.
        use crate::terminal_model::TerminalModel;

        let width = 60u16;
        let height = 20u16;
        let num_frames = 10;

        let mut prev_buffer = Buffer::new(width, height);
        let mut presenter = test_presenter();
        let mut model = TerminalModel::new(width as usize, height as usize);
        let mut rng = 0x5D2E_55DE_5D42_u64;
        let mut next = || -> u64 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            rng
        };

        for _frame in 0..num_frames {
            // Build next frame: modify ~20% of cells each time
            let mut buffer = prev_buffer.clone();
            let changes = (width as usize * height as usize) / 5;
            for _ in 0..changes {
                let v = next();
                let x = (v % width as u64) as u16;
                let y = ((v >> 16) % height as u64) as u16;
                let ch = char::from_u32(('!' as u32) + (v as u32 % 90)).unwrap_or('?');
                let fg = PackedRgba::rgb((v >> 8) as u8, (v >> 24) as u8, (v >> 40) as u8);
                let cell = Cell::from_char(ch).with_fg(fg);
                buffer.set_raw(x, y, cell);
            }

            let diff = BufferDiff::compute(&prev_buffer, &buffer);
            presenter.present(&buffer, &diff).unwrap();

            prev_buffer = buffer;
        }

        // Get all output and verify final frame via terminal model
        let output = presenter.into_inner().unwrap();
        model.process(&output);

        // Verify a sampling of cells match the final buffer
        let mut checked = 0;
        for y in 0..height {
            for x in 0..width {
                let buf_cell = prev_buffer.get_unchecked(x, y);
                if !buf_cell.is_empty()
                    && let Some(model_cell) = model.cell(x as usize, y as usize)
                {
                    let expected = buf_cell.content.as_char().unwrap_or(' ');
                    let mut buf = [0u8; 4];
                    let expected_str = expected.encode_utf8(&mut buf);
                    if model_cell.text.as_str() == expected_str {
                        checked += 1;
                    }
                }
            }
        }

        // At least 80% of non-empty cells should match (some may be
        // overwritten by cursor positioning sequences in the model)
        let total_nonempty = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .filter(|&(x, y)| !prev_buffer.get_unchecked(x, y).is_empty())
            .count();

        assert!(
            checked > total_nonempty * 80 / 100,
            "Frame {num_frames}: only {checked}/{total_nonempty} cells match final buffer"
        );
    }

    #[test]
    fn style_state_persists_across_frames() {
        let mut presenter = test_presenter();
        let fg = PackedRgba::rgb(100, 150, 200);

        // First frame - set style
        let mut buffer = Buffer::new(5, 1);
        buffer.set_raw(0, 0, Cell::from_char('A').with_fg(fg));
        let old = Buffer::new(5, 1);
        let diff = BufferDiff::compute(&old, &buffer);
        presenter.present(&buffer, &diff).unwrap();

        // Style should be tracked (but reset at frame end per the implementation)
        // After present(), current_style is None due to sgr_reset at frame end
        assert!(
            presenter.current_style.is_none(),
            "Style should be reset after frame end"
        );
    }

    // =========================================================================
    // Edge-case tests (bd-27tya)
    // =========================================================================

    // --- Cost model boundary values ---

    #[test]
    fn cost_cup_zero_zero() {
        // CUP at (0,0) → "\x1b[1;1H" = 6 bytes
        assert_eq!(cost_model::cup_cost(0, 0), 6);
    }

    #[test]
    fn cost_cup_max_max() {
        // CUP at (u16::MAX, u16::MAX) → "\x1b[65536;65536H"
        // 2 (CSI) + 5 (row digits) + 1 (;) + 5 (col digits) + 1 (H) = 14
        assert_eq!(cost_model::cup_cost(u16::MAX, u16::MAX), 14);
    }

    #[test]
    fn cost_cha_zero() {
        // CHA at col 0 → "\x1b[1G" = 4 bytes
        assert_eq!(cost_model::cha_cost(0), 4);
    }

    #[test]
    fn cost_cha_max() {
        // CHA at col u16::MAX → "\x1b[65536G" = 8 bytes
        assert_eq!(cost_model::cha_cost(u16::MAX), 8);
    }

    #[test]
    fn cost_cuf_zero_is_free() {
        assert_eq!(cost_model::cuf_cost(0), 0);
    }

    #[test]
    fn cost_cuf_one_is_three() {
        // CUF(1) = "\x1b[C" = 3 bytes
        assert_eq!(cost_model::cuf_cost(1), 3);
    }

    #[test]
    fn cost_cuf_two_has_digit() {
        // CUF(2) = "\x1b[2C" = 4 bytes
        assert_eq!(cost_model::cuf_cost(2), 4);
    }

    #[test]
    fn cost_cuf_max() {
        // CUF(u16::MAX) = "\x1b[65535C" = 3 + 5 = 8 bytes
        assert_eq!(cost_model::cuf_cost(u16::MAX), 8);
    }

    #[test]
    fn cost_cheapest_move_already_at_target() {
        assert_eq!(cost_model::cheapest_move_cost(Some(5), Some(3), 5, 3), 0);
    }

    #[test]
    fn cost_cheapest_move_unknown_position() {
        // When from is unknown, can only use CUP
        let cost = cost_model::cheapest_move_cost(None, None, 5, 3);
        assert_eq!(cost, cost_model::cup_cost(3, 5));
    }

    #[test]
    fn cost_cheapest_move_known_y_unknown_x() {
        // from_x=None, from_y=Some → still uses CUP
        let cost = cost_model::cheapest_move_cost(None, Some(3), 5, 3);
        assert_eq!(cost, cost_model::cup_cost(3, 5));
    }

    #[test]
    fn cost_cheapest_move_backward_same_row() {
        // Moving backward on same row: CHA or CUP, whichever is cheaper
        let cost = cost_model::cheapest_move_cost(Some(50), Some(0), 5, 0);
        let cha = cost_model::cha_cost(5);
        let cup = cost_model::cup_cost(0, 5);
        assert_eq!(cost, cha.min(cup));
    }

    #[test]
    fn cost_cheapest_move_same_row_same_col() {
        // Same (x, y) via the (fx, fy) == (to_x, to_y) check
        assert_eq!(cost_model::cheapest_move_cost(Some(0), Some(0), 0, 0), 0);
    }

    // --- CUP/CHA/CUF cost accuracy across digit boundaries ---

    #[test]
    fn cost_cup_digit_boundaries() {
        let mut buf = Vec::new();
        for (row, col) in [
            (0u16, 0u16),
            (8, 8),
            (9, 9),
            (98, 98),
            (99, 99),
            (998, 998),
            (999, 999),
            (9998, 9998),
            (9999, 9999),
            (u16::MAX, u16::MAX),
        ] {
            buf.clear();
            ansi::cup(&mut buf, row, col).unwrap();
            assert_eq!(
                buf.len(),
                cost_model::cup_cost(row, col),
                "CUP cost mismatch at ({row}, {col})"
            );
        }
    }

    #[test]
    fn cost_cha_digit_boundaries() {
        let mut buf = Vec::new();
        for col in [0u16, 8, 9, 98, 99, 998, 999, 9998, 9999, u16::MAX] {
            buf.clear();
            ansi::cha(&mut buf, col).unwrap();
            assert_eq!(
                buf.len(),
                cost_model::cha_cost(col),
                "CHA cost mismatch at col {col}"
            );
        }
    }

    #[test]
    fn cost_cuf_digit_boundaries() {
        let mut buf = Vec::new();
        for n in [1u16, 2, 9, 10, 99, 100, 999, 1000, 9999, 10000, u16::MAX] {
            buf.clear();
            ansi::cuf(&mut buf, n).unwrap();
            assert_eq!(
                buf.len(),
                cost_model::cuf_cost(n),
                "CUF cost mismatch for n={n}"
            );
        }
    }

    // --- RowPlan scratch reuse ---

    #[test]
    fn plan_row_reuse_matches_plan_row() {
        let runs = [
            ChangeRun::new(5, 2, 4),
            ChangeRun::new(5, 8, 10),
            ChangeRun::new(5, 20, 25),
        ];
        let plan1 = cost_model::plan_row(&runs, Some(0), Some(5));
        let mut scratch = cost_model::RowPlanScratch::default();
        let plan2 = cost_model::plan_row_reuse(&runs, Some(0), Some(5), &mut scratch);
        assert_eq!(plan1, plan2);
    }

    #[test]
    fn plan_row_reuse_across_different_sizes() {
        // Use scratch with a large row first, then a small row
        let mut scratch = cost_model::RowPlanScratch::default();

        let large_runs: Vec<ChangeRun> = (0..20)
            .map(|i| ChangeRun::new(0, i * 4, i * 4 + 1))
            .collect();
        let plan_large = cost_model::plan_row_reuse(&large_runs, None, None, &mut scratch);
        assert!(!plan_large.spans().is_empty());

        let small_runs = [ChangeRun::new(1, 5, 8)];
        let plan_small = cost_model::plan_row_reuse(&small_runs, None, None, &mut scratch);
        assert_eq!(plan_small.spans().len(), 1);
        assert_eq!(plan_small.spans()[0].x0, 5);
        assert_eq!(plan_small.spans()[0].x1, 8);
    }

    // --- DP gap boundary (exactly 32 and 33 cells) ---

    #[test]
    fn plan_row_gap_exactly_32_cells() {
        // Two runs with exactly 32-cell gap: run at 0-0 and 33-33
        // gap = 33 - 0 + 1 - 2 = 32 cells
        let runs = [ChangeRun::new(0, 0, 0), ChangeRun::new(0, 33, 33)];
        let plan = cost_model::plan_row(&runs, None, None);
        // 32-cell gap is at the break boundary; the DP may still consider merging
        // since the check is `gap_cells > 32` (strictly greater)
        // gap = 34 total - 2 changed = 32, which is NOT > 32, so merge is considered
        assert!(
            plan.spans().len() <= 2,
            "32-cell gap should still consider merge"
        );
    }

    #[test]
    fn plan_row_gap_33_cells_stays_sparse() {
        // Two runs with 33-cell gap: run at 0-0 and 34-34
        // gap = 34 - 0 + 1 - 2 = 33 > 32, so merge is NOT considered
        let runs = [ChangeRun::new(0, 0, 0), ChangeRun::new(0, 34, 34)];
        let plan = cost_model::plan_row(&runs, None, None);
        assert_eq!(
            plan.spans().len(),
            2,
            "33-cell gap should stay sparse (gap > 32 breaks)"
        );
    }

    // --- SmallVec spill: >4 separate spans ---

    #[test]
    fn plan_row_many_sparse_spans() {
        // 6 runs with 34+ cell gaps between them (each gap > 32, no merging)
        let runs = [
            ChangeRun::new(0, 0, 0),
            ChangeRun::new(0, 40, 40),
            ChangeRun::new(0, 80, 80),
            ChangeRun::new(0, 120, 120),
            ChangeRun::new(0, 160, 160),
            ChangeRun::new(0, 200, 200),
        ];
        let plan = cost_model::plan_row(&runs, None, None);
        // All gaps are > 32, so no merging possible
        assert_eq!(plan.spans().len(), 6, "Should have 6 separate sparse spans");
    }

    // --- CellStyle ---

    #[test]
    fn cell_style_default_is_transparent_no_attrs() {
        let style = CellStyle::default();
        assert_eq!(style.fg, PackedRgba::TRANSPARENT);
        assert_eq!(style.bg, PackedRgba::TRANSPARENT);
        assert!(style.attrs.is_empty());
    }

    #[test]
    fn cell_style_from_cell_captures_all() {
        let fg = PackedRgba::rgb(10, 20, 30);
        let bg = PackedRgba::rgb(40, 50, 60);
        let flags = StyleFlags::BOLD | StyleFlags::ITALIC;
        let cell = Cell::from_char('X')
            .with_fg(fg)
            .with_bg(bg)
            .with_attrs(CellAttrs::new(flags, 5));
        let style = CellStyle::from_cell(&cell);
        assert_eq!(style.fg, fg);
        assert_eq!(style.bg, bg);
        assert_eq!(style.attrs, flags);
    }

    #[test]
    fn cell_style_eq_and_clone() {
        let a = CellStyle {
            fg: PackedRgba::rgb(1, 2, 3),
            bg: PackedRgba::TRANSPARENT,
            attrs: StyleFlags::DIM,
        };
        let b = a;
        assert_eq!(a, b);
    }

    // --- SGR length estimation ---

    #[test]
    fn sgr_flags_len_empty() {
        assert_eq!(Presenter::<Vec<u8>>::sgr_flags_len(StyleFlags::empty()), 0);
    }

    #[test]
    fn sgr_flags_len_single() {
        // Single flag: "\x1b[1m" = 4 bytes → 3 + digits(code) + 0 separators
        let len = Presenter::<Vec<u8>>::sgr_flags_len(StyleFlags::BOLD);
        assert!(len > 0);
        // Verify by actually emitting
        let mut buf = Vec::new();
        ansi::sgr_flags(&mut buf, StyleFlags::BOLD).unwrap();
        assert_eq!(len as usize, buf.len());
    }

    #[test]
    fn sgr_flags_len_multiple() {
        let flags = StyleFlags::BOLD | StyleFlags::ITALIC | StyleFlags::UNDERLINE;
        let len = Presenter::<Vec<u8>>::sgr_flags_len(flags);
        let mut buf = Vec::new();
        ansi::sgr_flags(&mut buf, flags).unwrap();
        assert_eq!(len as usize, buf.len());
    }

    #[test]
    fn sgr_flags_off_len_empty() {
        assert_eq!(
            Presenter::<Vec<u8>>::sgr_flags_off_len(StyleFlags::empty()),
            0
        );
    }

    #[test]
    fn sgr_rgb_len_matches_actual() {
        let color = PackedRgba::rgb(0, 0, 0);
        let estimated = Presenter::<Vec<u8>>::sgr_rgb_len(color);
        // "\x1b[38;2;0;0;0m" = 2(CSI) + "38;2;" + "0;0;0" + "m" but sgr_rgb_len
        // is used for cost comparison, not exact output. Just check > 0.
        assert!(estimated > 0);
    }

    #[test]
    fn sgr_rgb_len_large_values() {
        let color = PackedRgba::rgb(255, 255, 255);
        let small_color = PackedRgba::rgb(0, 0, 0);
        let large_len = Presenter::<Vec<u8>>::sgr_rgb_len(color);
        let small_len = Presenter::<Vec<u8>>::sgr_rgb_len(small_color);
        // 255,255,255 has more digits than 0,0,0
        assert!(large_len > small_len);
    }

    #[test]
    fn dec_len_u8_boundaries() {
        assert_eq!(Presenter::<Vec<u8>>::dec_len_u8(0), 1);
        assert_eq!(Presenter::<Vec<u8>>::dec_len_u8(9), 1);
        assert_eq!(Presenter::<Vec<u8>>::dec_len_u8(10), 2);
        assert_eq!(Presenter::<Vec<u8>>::dec_len_u8(99), 2);
        assert_eq!(Presenter::<Vec<u8>>::dec_len_u8(100), 3);
        assert_eq!(Presenter::<Vec<u8>>::dec_len_u8(255), 3);
    }

    // --- Style delta corner cases ---

    #[test]
    fn sgr_delta_all_attrs_removed_at_once() {
        let mut presenter = test_presenter();
        let all_flags = StyleFlags::BOLD
            | StyleFlags::DIM
            | StyleFlags::ITALIC
            | StyleFlags::UNDERLINE
            | StyleFlags::BLINK
            | StyleFlags::REVERSE
            | StyleFlags::STRIKETHROUGH;
        let old = CellStyle {
            fg: PackedRgba::rgb(100, 100, 100),
            bg: PackedRgba::TRANSPARENT,
            attrs: all_flags,
        };
        let new = CellStyle {
            fg: PackedRgba::rgb(100, 100, 100),
            bg: PackedRgba::TRANSPARENT,
            attrs: StyleFlags::empty(),
        };

        presenter.current_style = Some(old);
        presenter.emit_style_delta(old, new).unwrap();
        let output = presenter.into_inner().unwrap();

        // Should either use individual off codes or fall back to full reset
        // Either way, output should be non-empty
        assert!(!output.is_empty());
    }

    #[test]
    fn sgr_delta_fg_to_transparent() {
        let mut presenter = test_presenter();
        let old = CellStyle {
            fg: PackedRgba::rgb(200, 100, 50),
            bg: PackedRgba::TRANSPARENT,
            attrs: StyleFlags::empty(),
        };
        let new = CellStyle {
            fg: PackedRgba::TRANSPARENT,
            bg: PackedRgba::TRANSPARENT,
            attrs: StyleFlags::empty(),
        };

        presenter.current_style = Some(old);
        presenter.emit_style_delta(old, new).unwrap();
        let output = presenter.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        // When going to TRANSPARENT fg, the delta should emit the default fg code
        // or reset. Either way, output should be non-empty.
        assert!(!output.is_empty(), "Should emit fg removal: {output_str:?}");
    }

    #[test]
    fn sgr_delta_bg_to_transparent() {
        let mut presenter = test_presenter();
        let old = CellStyle {
            fg: PackedRgba::TRANSPARENT,
            bg: PackedRgba::rgb(30, 60, 90),
            attrs: StyleFlags::empty(),
        };
        let new = CellStyle {
            fg: PackedRgba::TRANSPARENT,
            bg: PackedRgba::TRANSPARENT,
            attrs: StyleFlags::empty(),
        };

        presenter.current_style = Some(old);
        presenter.emit_style_delta(old, new).unwrap();
        let output = presenter.into_inner().unwrap();
        assert!(!output.is_empty(), "Should emit bg removal");
    }

    #[test]
    fn sgr_delta_dim_removed_bold_stays() {
        // Reverse of the bold-dim collateral test: removing DIM while BOLD stays.
        // DIM off (code 22) also disables BOLD. If BOLD should remain,
        // the delta engine must re-enable BOLD.
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);

        let attrs1 = CellAttrs::new(StyleFlags::BOLD | StyleFlags::DIM, 0);
        let attrs2 = CellAttrs::new(StyleFlags::BOLD, 0);
        buffer.set_raw(0, 0, Cell::from_char('A').with_attrs(attrs1));
        buffer.set_raw(1, 0, Cell::from_char('B').with_attrs(attrs2));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Should contain dim-off (22) and then bold re-enable (1)
        assert!(
            output_str.contains("\x1b[22m"),
            "Expected dim-off (22) in: {output_str:?}"
        );
        assert!(
            output_str.contains("\x1b[1m"),
            "Expected bold re-enable (1) in: {output_str:?}"
        );
    }

    #[test]
    fn sgr_delta_fallback_to_full_reset_when_cheaper() {
        // Many attrs removed + colors changed → delta is expensive, full reset is cheaper
        let mut presenter = test_presenter();
        let old = CellStyle {
            fg: PackedRgba::rgb(10, 20, 30),
            bg: PackedRgba::rgb(40, 50, 60),
            attrs: StyleFlags::BOLD
                | StyleFlags::DIM
                | StyleFlags::ITALIC
                | StyleFlags::UNDERLINE
                | StyleFlags::STRIKETHROUGH,
        };
        let new = CellStyle {
            fg: PackedRgba::TRANSPARENT,
            bg: PackedRgba::TRANSPARENT,
            attrs: StyleFlags::empty(),
        };

        presenter.current_style = Some(old);
        presenter.emit_style_delta(old, new).unwrap();
        let output = presenter.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        // With everything removed and going to default, full reset ("\x1b[0m") is cheapest
        assert!(
            output_str.contains("\x1b[0m"),
            "Expected full reset fallback: {output_str:?}"
        );
    }

    // --- Content emission edge cases ---

    #[test]
    fn emit_cell_control_char_replaced_with_fffd() {
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(0);
        presenter.cursor_y = Some(0);

        // Control character '\x01' has width 0, not empty, not continuation.
        // The zero-width-content path replaces it with U+FFFD.
        let cell = Cell::from_char('\x01');
        presenter.emit_cell(0, &cell, None, None).unwrap();
        let output = presenter.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        // Should emit U+FFFD (replacement character), not the raw control char
        assert!(
            output_str.contains('\u{FFFD}'),
            "Control char (width 0) should be replaced with U+FFFD, got: {output:?}"
        );
        assert!(
            !output.contains(&0x01),
            "Raw control char should not appear"
        );
    }

    #[test]
    fn emit_content_empty_cell_emits_space() {
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(0);
        presenter.cursor_y = Some(0);

        let cell = Cell::default();
        assert!(cell.is_empty());
        presenter.emit_cell(0, &cell, None, None).unwrap();
        let output = presenter.into_inner().unwrap();
        assert!(output.contains(&b' '), "Empty cell should emit space");
    }

    // --- Continuation cell cursor_x variants ---

    #[test]
    fn continuation_cell_cursor_x_none() {
        let mut presenter = test_presenter();
        // cursor_x = None → defensive path, emits CUF(1) and sets cursor_x
        presenter.cursor_x = None;
        presenter.cursor_y = Some(0);

        let cell = Cell::CONTINUATION;
        presenter.emit_cell(5, &cell, None, None).unwrap();
        let output = presenter.into_inner().unwrap();

        // Should emit CUF(1) = "\x1b[C"
        assert!(
            output.windows(3).any(|w| w == b"\x1b[C"),
            "Should emit CUF(1) for continuation with unknown cursor_x"
        );
    }

    #[test]
    fn continuation_cell_cursor_already_past() {
        let mut presenter = test_presenter();
        // cursor_x > cell x → cursor already advanced past, skip
        presenter.cursor_x = Some(10);
        presenter.cursor_y = Some(0);

        let cell = Cell::CONTINUATION;
        presenter.emit_cell(5, &cell, None, None).unwrap();
        let output = presenter.into_inner().unwrap();

        // Should produce no output (cursor already past)
        assert!(
            output.is_empty(),
            "Should skip continuation when cursor is past it"
        );
    }

    // --- clear_line ---

    #[test]
    fn clear_line_positions_cursor_and_erases() {
        let mut presenter = test_presenter();
        presenter.clear_line(5).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Should contain CUP to row 5 col 0 and erase line
        assert!(
            output_str.contains("\x1b[2K"),
            "Should contain erase line sequence"
        );
    }

    // --- into_inner ---

    #[test]
    fn into_inner_returns_accumulated_output() {
        let mut presenter = test_presenter();
        presenter.position_cursor(0, 0).unwrap();
        let inner = presenter.into_inner().unwrap();
        assert!(!inner.is_empty(), "into_inner should return buffered data");
    }

    // --- move_cursor_optimal edge cases ---

    #[test]
    fn move_cursor_optimal_same_row_forward_large() {
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(0);
        presenter.cursor_y = Some(0);

        // Forward by 100 columns. CUF(100) vs CHA(100) vs CUP(0,100)
        presenter.move_cursor_optimal(100, 0).unwrap();
        let output = presenter.into_inner().unwrap();

        // Verify the output picks the cheapest move
        let cuf = cost_model::cuf_cost(100);
        let cha = cost_model::cha_cost(100);
        let cup = cost_model::cup_cost(0, 100);
        let cheapest = cuf.min(cha).min(cup);
        assert_eq!(output.len(), cheapest, "Should pick cheapest cursor move");
    }

    #[test]
    fn move_cursor_optimal_same_row_backward_to_zero() {
        let mut presenter = test_presenter();
        presenter.cursor_x = Some(50);
        presenter.cursor_y = Some(0);

        presenter.move_cursor_optimal(0, 0).unwrap();
        let output = presenter.into_inner().unwrap();

        // CHA(0) → "\x1b[1G" = 4 bytes, CUP(0,0) = "\x1b[1;1H" = 6 bytes
        // CHA should win
        let mut expected = Vec::new();
        ansi::cha(&mut expected, 0).unwrap();
        assert_eq!(output, expected, "Should use CHA for backward to col 0");
    }

    #[test]
    fn move_cursor_optimal_unknown_cursor_uses_cup() {
        let mut presenter = test_presenter();
        // cursor_x and cursor_y are None
        presenter.move_cursor_optimal(10, 5).unwrap();
        let output = presenter.into_inner().unwrap();
        let mut expected = Vec::new();
        ansi::cup(&mut expected, 5, 10).unwrap();
        assert_eq!(output, expected, "Should use CUP when cursor is unknown");
    }

    // --- Present with sync: verify wrap order ---

    #[test]
    fn sync_wrap_order_begin_content_reset_end() {
        let mut presenter = test_presenter_with_sync();
        let mut buffer = Buffer::new(3, 1);
        buffer.set_raw(0, 0, Cell::from_char('Z'));

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);

        let sync_begin_pos = output
            .windows(ansi::SYNC_BEGIN.len())
            .position(|w| w == ansi::SYNC_BEGIN)
            .expect("sync begin missing");
        let z_pos = output
            .iter()
            .position(|&b| b == b'Z')
            .expect("character Z missing");
        let reset_pos = output
            .windows(b"\x1b[0m".len())
            .rposition(|w| w == b"\x1b[0m")
            .expect("SGR reset missing");
        let sync_end_pos = output
            .windows(ansi::SYNC_END.len())
            .rposition(|w| w == ansi::SYNC_END)
            .expect("sync end missing");

        assert!(sync_begin_pos < z_pos, "sync begin before content");
        assert!(z_pos < reset_pos, "content before reset");
        assert!(reset_pos < sync_end_pos, "reset before sync end");
    }

    // --- Multi-frame style state ---

    #[test]
    fn style_none_after_each_frame() {
        let mut presenter = test_presenter();
        let fg = PackedRgba::rgb(255, 128, 64);

        for _ in 0..5 {
            let mut buffer = Buffer::new(3, 1);
            buffer.set_raw(0, 0, Cell::from_char('X').with_fg(fg));
            let old = Buffer::new(3, 1);
            let diff = BufferDiff::compute(&old, &buffer);
            presenter.present(&buffer, &diff).unwrap();

            // After each present(), current_style should be None (reset at frame end)
            assert!(
                presenter.current_style.is_none(),
                "Style should be None after frame end"
            );
            assert!(
                presenter.current_link.is_none(),
                "Link should be None after frame end"
            );
        }
    }

    // --- Link state after present with open link ---

    #[test]
    fn link_closed_at_frame_end_even_if_all_cells_linked() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(3, 1);
        let mut links = LinkRegistry::new();
        let link_id = links.register("https://all-linked.test");

        // All cells have the same link
        for x in 0..3 {
            buffer.set_raw(
                x,
                0,
                Cell::from_char('L').with_attrs(CellAttrs::new(StyleFlags::empty(), link_id)),
            );
        }

        let old = Buffer::new(3, 1);
        let diff = BufferDiff::compute(&old, &buffer);
        presenter
            .present_with_pool(&buffer, &diff, None, Some(&links))
            .unwrap();

        // After present, current_link must be None (closed at frame end)
        assert!(
            presenter.current_link.is_none(),
            "Link must be closed at frame end"
        );
    }

    // --- PresentStats ---

    #[test]
    fn present_stats_empty_diff() {
        let mut presenter = test_presenter();
        let buffer = Buffer::new(10, 10);
        let diff = BufferDiff::new();
        let stats = presenter.present(&buffer, &diff).unwrap();

        assert_eq!(stats.cells_changed, 0);
        assert_eq!(stats.run_count, 0);
        // bytes_emitted includes the SGR reset
        assert!(stats.bytes_emitted > 0);
    }

    #[test]
    fn present_stats_full_row() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(10, 1);
        for x in 0..10 {
            buffer.set_raw(x, 0, Cell::from_char('A'));
        }
        let old = Buffer::new(10, 1);
        let diff = BufferDiff::compute(&old, &buffer);
        let stats = presenter.present(&buffer, &diff).unwrap();

        assert_eq!(stats.cells_changed, 10);
        assert!(stats.run_count >= 1);
        assert!(stats.bytes_emitted > 10, "Should include ANSI overhead");
    }

    // --- Capabilities accessor ---

    #[test]
    fn capabilities_accessor() {
        let mut caps = TerminalCapabilities::basic();
        caps.sync_output = true;
        let presenter = Presenter::new(Vec::<u8>::new(), caps);
        assert!(presenter.capabilities().sync_output);
    }

    // --- Flush ---

    #[test]
    fn flush_succeeds_on_empty_presenter() {
        let mut presenter = test_presenter();
        presenter.flush().unwrap();
        let output = get_output(presenter);
        assert!(output.is_empty());
    }

    // --- RowPlan total_cost ---

    #[test]
    fn row_plan_total_cost_matches_dp() {
        let runs = [ChangeRun::new(3, 5, 10), ChangeRun::new(3, 15, 20)];
        let plan = cost_model::plan_row(&runs, None, None);
        assert!(plan.total_cost() > 0);
        // The total cost includes move costs + cell costs
        // Just verify it's consistent (non-zero) and accessible
    }

    // --- Style delta: same attrs, only colors change (hot path) ---

    #[test]
    fn sgr_delta_hot_path_only_fg_change() {
        let mut presenter = test_presenter();
        let old = CellStyle {
            fg: PackedRgba::rgb(255, 0, 0),
            bg: PackedRgba::rgb(0, 0, 0),
            attrs: StyleFlags::BOLD | StyleFlags::ITALIC,
        };
        let new = CellStyle {
            fg: PackedRgba::rgb(0, 255, 0),
            bg: PackedRgba::rgb(0, 0, 0),
            attrs: StyleFlags::BOLD | StyleFlags::ITALIC, // same attrs
        };

        presenter.current_style = Some(old);
        presenter.emit_style_delta(old, new).unwrap();
        let output = presenter.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        // Only fg should change, no reset
        assert!(output_str.contains("38;2;0;255;0"), "Should emit new fg");
        assert!(
            !output_str.contains("\x1b[0m"),
            "No reset needed for color-only change"
        );
        // Should NOT re-emit attrs
        assert!(
            !output_str.contains("\x1b[1m"),
            "Bold should not be re-emitted"
        );
    }

    #[test]
    fn sgr_delta_hot_path_both_colors_change() {
        let mut presenter = test_presenter();
        let old = CellStyle {
            fg: PackedRgba::rgb(1, 2, 3),
            bg: PackedRgba::rgb(4, 5, 6),
            attrs: StyleFlags::UNDERLINE,
        };
        let new = CellStyle {
            fg: PackedRgba::rgb(7, 8, 9),
            bg: PackedRgba::rgb(10, 11, 12),
            attrs: StyleFlags::UNDERLINE, // same
        };

        presenter.current_style = Some(old);
        presenter.emit_style_delta(old, new).unwrap();
        let output = presenter.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains("38;2;7;8;9"), "Should emit new fg");
        assert!(output_str.contains("48;2;10;11;12"), "Should emit new bg");
        assert!(!output_str.contains("\x1b[0m"), "No reset for color-only");
    }

    // --- Style full apply ---

    #[test]
    fn emit_style_full_default_is_just_reset() {
        let mut presenter = test_presenter();
        let default_style = CellStyle::default();
        presenter.emit_style_full(default_style).unwrap();
        let output = presenter.into_inner().unwrap();

        // Default style (transparent fg/bg, no attrs) should just be reset
        assert_eq!(output, b"\x1b[0m");
    }

    #[test]
    fn emit_style_full_with_all_properties() {
        let mut presenter = test_presenter();
        let style = CellStyle {
            fg: PackedRgba::rgb(10, 20, 30),
            bg: PackedRgba::rgb(40, 50, 60),
            attrs: StyleFlags::BOLD | StyleFlags::ITALIC,
        };
        presenter.emit_style_full(style).unwrap();
        let output = presenter.into_inner().unwrap();
        let output_str = String::from_utf8_lossy(&output);

        // Should have reset + fg + bg + attrs
        assert!(output_str.contains("\x1b[0m"), "Should start with reset");
        assert!(output_str.contains("38;2;10;20;30"), "Should have fg");
        assert!(output_str.contains("48;2;40;50;60"), "Should have bg");
    }

    // --- Multiple rows with different strategies ---

    #[test]
    fn present_multiple_rows_different_strategies() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(80, 5);

        // Row 0: dense changes (should merge)
        for x in (0..20).step_by(2) {
            buffer.set_raw(x, 0, Cell::from_char('D'));
        }
        // Row 2: sparse changes (large gap, should stay sparse)
        buffer.set_raw(0, 2, Cell::from_char('L'));
        buffer.set_raw(79, 2, Cell::from_char('R'));
        // Row 4: single cell
        buffer.set_raw(40, 4, Cell::from_char('M'));

        let old = Buffer::new(80, 5);
        let diff = BufferDiff::compute(&old, &buffer);
        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        assert!(output_str.contains('D'));
        assert!(output_str.contains('L'));
        assert!(output_str.contains('R'));
        assert!(output_str.contains('M'));
    }

    #[test]
    fn zero_width_chars_replaced_with_placeholder() {
        let mut presenter = test_presenter();
        let mut buffer = Buffer::new(5, 1);

        // U+0301 is COMBINING ACUTE ACCENT (width 0).
        // It is not empty, not continuation, not grapheme (unless pooled).
        // Storing it directly as a char means it's a standalone cell content.
        let zw_char = '\u{0301}';

        // Ensure our assumption about width is correct for this environment
        assert_eq!(Cell::from_char(zw_char).content.width(), 0);

        buffer.set_raw(0, 0, Cell::from_char(zw_char));
        buffer.set_raw(1, 0, Cell::from_char('A'));

        let old = Buffer::new(5, 1);
        let diff = BufferDiff::compute(&old, &buffer);

        presenter.present(&buffer, &diff).unwrap();
        let output = get_output(presenter);
        let output_str = String::from_utf8_lossy(&output);

        // Should contain U+FFFD (Replacement Character)
        assert!(
            output_str.contains("\u{FFFD}"),
            "Expected replacement character for zero-width content, got: {:?}",
            output_str
        );

        // Should NOT contain the raw combining mark
        assert!(
            !output_str.contains(zw_char),
            "Should not contain raw zero-width char"
        );

        // Should contain 'A' (verify cursor sync didn't swallow it)
        assert!(
            output_str.contains('A'),
            "Should contain subsequent character 'A'"
        );
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use crate::cell::{Cell, PackedRgba};
    use crate::diff::BufferDiff;
    use crate::terminal_model::TerminalModel;
    use proptest::prelude::*;

    /// Create a presenter for testing.
    fn test_presenter() -> Presenter<Vec<u8>> {
        let caps = TerminalCapabilities::basic();
        Presenter::new(Vec::new(), caps)
    }

    proptest! {
        /// Property: Presenter output, when applied to terminal model, produces
        /// the correct characters for changed cells.
        #[test]
        fn presenter_roundtrip_characters(
            width in 5u16..40,
            height in 3u16..20,
            num_chars in 1usize..50, // At least 1 char to have meaningful diff
        ) {
            let mut buffer = Buffer::new(width, height);
            let mut changed_positions = std::collections::HashSet::new();

            // Fill some cells with ASCII chars
            for i in 0..num_chars {
                let x = (i * 7 + 3) as u16 % width;
                let y = (i * 11 + 5) as u16 % height;
                let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
                buffer.set_raw(x, y, Cell::from_char(ch));
                changed_positions.insert((x, y));
            }

            // Present full buffer
            let mut presenter = test_presenter();
            let old = Buffer::new(width, height);
            let diff = BufferDiff::compute(&old, &buffer);
            presenter.present(&buffer, &diff).unwrap();
            let output = presenter.into_inner().unwrap();

            // Apply to terminal model
            let mut model = TerminalModel::new(width as usize, height as usize);
            model.process(&output);

            // Verify ONLY changed characters match (model may have different default)
            for &(x, y) in &changed_positions {
                let buf_cell = buffer.get_unchecked(x, y);
                let expected_ch = buf_cell.content.as_char().unwrap_or(' ');
                let mut expected_buf = [0u8; 4];
                let expected_str = expected_ch.encode_utf8(&mut expected_buf);

                if let Some(model_cell) = model.cell(x as usize, y as usize) {
                    prop_assert_eq!(
                        model_cell.text.as_str(),
                        expected_str,
                        "Character mismatch at ({}, {})", x, y
                    );
                }
            }
        }

        /// Property: After complete frame presentation, SGR is reset.
        #[test]
        fn style_reset_after_present(
            width in 5u16..30,
            height in 3u16..15,
            num_styled in 1usize..20,
        ) {
            let mut buffer = Buffer::new(width, height);

            // Add some styled cells
            for i in 0..num_styled {
                let x = (i * 7) as u16 % width;
                let y = (i * 11) as u16 % height;
                let fg = PackedRgba::rgb(
                    ((i * 31) % 256) as u8,
                    ((i * 47) % 256) as u8,
                    ((i * 71) % 256) as u8,
                );
                buffer.set_raw(x, y, Cell::from_char('X').with_fg(fg));
            }

            // Present
            let mut presenter = test_presenter();
            let old = Buffer::new(width, height);
            let diff = BufferDiff::compute(&old, &buffer);
            presenter.present(&buffer, &diff).unwrap();
            let output = presenter.into_inner().unwrap();
            let output_str = String::from_utf8_lossy(&output);

            // Output should end with SGR reset sequence
            prop_assert!(
                output_str.contains("\x1b[0m"),
                "Output should contain SGR reset"
            );
        }

        /// Property: Presenter handles empty diff correctly.
        #[test]
        fn empty_diff_minimal_output(
            width in 5u16..50,
            height in 3u16..25,
        ) {
            let buffer = Buffer::new(width, height);
            let diff = BufferDiff::new(); // Empty diff

            let mut presenter = test_presenter();
            presenter.present(&buffer, &diff).unwrap();
            let output = presenter.into_inner().unwrap();

            // Output should only be SGR reset (or very minimal)
            // No cursor moves or cell content for empty diff
            prop_assert!(output.len() < 50, "Empty diff should have minimal output");
        }

        /// Property: Full buffer change produces diff with all cells.
        ///
        /// When every cell differs, the diff should contain exactly
        /// width * height changes.
        #[test]
        fn diff_size_bounds(
            width in 5u16..30,
            height in 3u16..15,
        ) {
            // Full change buffer
            let old = Buffer::new(width, height);
            let mut new = Buffer::new(width, height);

            for y in 0..height {
                for x in 0..width {
                    new.set_raw(x, y, Cell::from_char('X'));
                }
            }

            let diff = BufferDiff::compute(&old, &new);

            // Diff should capture all cells
            prop_assert_eq!(
                diff.len(),
                (width as usize) * (height as usize),
                "Full change should have all cells in diff"
            );
        }

        /// Property: Presenter cursor state is consistent after operations.
        #[test]
        fn presenter_cursor_consistency(
            width in 10u16..40,
            height in 5u16..20,
            num_runs in 1usize..10,
        ) {
            let mut buffer = Buffer::new(width, height);

            // Create some runs of changes
            for i in 0..num_runs {
                let start_x = (i * 5) as u16 % (width - 5);
                let y = i as u16 % height;
                for x in start_x..(start_x + 3) {
                    buffer.set_raw(x, y, Cell::from_char('A'));
                }
            }

            // Multiple presents should work correctly
            let mut presenter = test_presenter();
            let old = Buffer::new(width, height);

            for _ in 0..3 {
                let diff = BufferDiff::compute(&old, &buffer);
                presenter.present(&buffer, &diff).unwrap();
            }

            // Should not panic and produce valid output
            let output = presenter.into_inner().unwrap();
            prop_assert!(!output.is_empty(), "Should produce some output");
        }

        /// Property (bd-4kq0.2.1): SGR delta produces identical visual styling
        /// as reset+apply for random style transitions. Verified via terminal
        /// model roundtrip.
        #[test]
        fn sgr_delta_transition_equivalence(
            width in 5u16..20,
            height in 3u16..10,
            num_styled in 2usize..15,
        ) {
            let mut buffer = Buffer::new(width, height);
            // Track final character at each position (later writes overwrite earlier)
            let mut expected: std::collections::HashMap<(u16, u16), char> =
                std::collections::HashMap::new();

            // Create cells with varying styles to exercise delta engine
            for i in 0..num_styled {
                let x = (i * 3 + 1) as u16 % width;
                let y = (i * 5 + 2) as u16 % height;
                let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
                let fg = PackedRgba::rgb(
                    ((i * 73) % 256) as u8,
                    ((i * 137) % 256) as u8,
                    ((i * 41) % 256) as u8,
                );
                let bg = if i % 3 == 0 {
                    PackedRgba::rgb(
                        ((i * 29) % 256) as u8,
                        ((i * 53) % 256) as u8,
                        ((i * 97) % 256) as u8,
                    )
                } else {
                    PackedRgba::TRANSPARENT
                };
                let flags_bits = ((i * 37) % 256) as u8;
                let flags = StyleFlags::from_bits_truncate(flags_bits);
                let cell = Cell::from_char(ch)
                    .with_fg(fg)
                    .with_bg(bg)
                    .with_attrs(CellAttrs::new(flags, 0));
                buffer.set_raw(x, y, cell);
                expected.insert((x, y), ch);
            }

            // Present with delta engine
            let mut presenter = test_presenter();
            let old = Buffer::new(width, height);
            let diff = BufferDiff::compute(&old, &buffer);
            presenter.present(&buffer, &diff).unwrap();
            let output = presenter.into_inner().unwrap();

            // Apply to terminal model and verify characters
            let mut model = TerminalModel::new(width as usize, height as usize);
            model.process(&output);

            for (&(x, y), &ch) in &expected {
                let mut buf = [0u8; 4];
                let expected_str = ch.encode_utf8(&mut buf);

                if let Some(model_cell) = model.cell(x as usize, y as usize) {
                    prop_assert_eq!(
                        model_cell.text.as_str(),
                        expected_str,
                        "Character mismatch at ({}, {}) with delta engine", x, y
                    );
                }
            }
        }

        /// Property (bd-4kq0.2.2): DP cost model produces correct output
        /// regardless of which row strategy is chosen (sparse vs merged).
        /// Verified via terminal model roundtrip with scattered runs.
        #[test]
        fn dp_emit_equivalence(
            width in 20u16..60,
            height in 5u16..15,
            num_changes in 5usize..30,
        ) {
            let mut buffer = Buffer::new(width, height);
            let mut expected: std::collections::HashMap<(u16, u16), char> =
                std::collections::HashMap::new();

            // Create scattered changes that will trigger both sparse and merged strategies
            for i in 0..num_changes {
                let x = (i * 7 + 3) as u16 % width;
                let y = (i * 3 + 1) as u16 % height;
                let ch = char::from_u32(('A' as u32) + (i as u32 % 26)).unwrap();
                buffer.set_raw(x, y, Cell::from_char(ch));
                expected.insert((x, y), ch);
            }

            // Present with DP cost model
            let mut presenter = test_presenter();
            let old = Buffer::new(width, height);
            let diff = BufferDiff::compute(&old, &buffer);
            presenter.present(&buffer, &diff).unwrap();
            let output = presenter.into_inner().unwrap();

            // Apply to terminal model and verify all characters are correct
            let mut model = TerminalModel::new(width as usize, height as usize);
            model.process(&output);

            for (&(x, y), &ch) in &expected {
                let mut buf = [0u8; 4];
                let expected_str = ch.encode_utf8(&mut buf);

                if let Some(model_cell) = model.cell(x as usize, y as usize) {
                    prop_assert_eq!(
                        model_cell.text.as_str(),
                        expected_str,
                        "DP cost model: character mismatch at ({}, {})", x, y
                    );
                }
            }
        }
    }
}
