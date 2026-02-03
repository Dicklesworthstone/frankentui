#![forbid(unsafe_code)]

//! Layout constraint debugging utilities.
//!
//! Provides introspection into layout constraint solving:
//! - Recording of constraint solving steps
//! - Detection of overflow/underflow conditions
//! - Export to Graphviz DOT format
//!
//! # Feature Gating
//!
//! This module is always compiled (the types are useful for testing),
//! but recording is a no-op unless explicitly enabled at runtime.
//!
//! # Usage
//!
//! ```ignore
//! use ftui_layout::debug::{LayoutDebugger, LayoutRecord};
//!
//! let debugger = LayoutDebugger::new();
//! debugger.set_enabled(true);
//!
//! // ... perform layout ...
//!
//! for record in debugger.snapshot() {
//!     println!("{}: {:?} -> {:?}", record.name, record.constraints, record.computed_sizes);
//!     if record.has_overflow() {
//!         eprintln!("  WARNING: overflow detected!");
//!     }
//! }
//! ```

use crate::{Alignment, Constraint, Direction, Sides};
use ftui_core::geometry::Rect;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A record of a single layout solve operation.
#[derive(Debug, Clone)]
pub struct LayoutRecord {
    /// User-provided name for identification.
    pub name: String,
    /// The constraints that were solved.
    pub constraints: Vec<Constraint>,
    /// Total available size before solving.
    pub available_size: u16,
    /// Computed sizes for each constraint.
    pub computed_sizes: Vec<u16>,
    /// Layout direction.
    pub direction: Direction,
    /// Alignment mode.
    pub alignment: Alignment,
    /// Margin applied before solving.
    pub margin: Sides,
    /// Gap between items.
    pub gap: u16,
    /// The input area.
    pub input_area: Rect,
    /// The resulting rectangles.
    pub result_rects: Vec<Rect>,
    /// Time taken to solve (if measured).
    pub solve_time: Option<Duration>,
    /// Parent record index (for nested layouts).
    pub parent_index: Option<usize>,
}

impl LayoutRecord {
    /// Create a new layout record.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            constraints: Vec::new(),
            available_size: 0,
            computed_sizes: Vec::new(),
            direction: Direction::default(),
            alignment: Alignment::default(),
            margin: Sides::default(),
            gap: 0,
            input_area: Rect::default(),
            result_rects: Vec::new(),
            solve_time: None,
            parent_index: None,
        }
    }

    /// Check if the total computed size exceeds available space (overflow).
    pub fn has_overflow(&self) -> bool {
        let total_computed: u16 = self.computed_sizes.iter().sum();
        let total_gaps = if self.computed_sizes.len() > 1 {
            self.gap
                .saturating_mul((self.computed_sizes.len() - 1) as u16)
        } else {
            0
        };
        total_computed.saturating_add(total_gaps) > self.available_size
    }

    /// Check if significant space remains unused (underflow).
    ///
    /// Returns true if more than 20% of available space is unused.
    pub fn has_underflow(&self) -> bool {
        let total_computed: u16 = self.computed_sizes.iter().sum();
        let total_gaps = if self.computed_sizes.len() > 1 {
            self.gap
                .saturating_mul((self.computed_sizes.len() - 1) as u16)
        } else {
            0
        };
        let total_used = total_computed.saturating_add(total_gaps);
        let unused = self.available_size.saturating_sub(total_used);
        // Consider underflow if >20% unused
        self.available_size > 0 && (unused as f32 / self.available_size as f32) > 0.2
    }

    /// Percentage of available space used.
    pub fn utilization(&self) -> f32 {
        if self.available_size == 0 {
            return 0.0;
        }
        let total_computed: u16 = self.computed_sizes.iter().sum();
        let total_gaps = if self.computed_sizes.len() > 1 {
            self.gap
                .saturating_mul((self.computed_sizes.len() - 1) as u16)
        } else {
            0
        };
        let total_used = total_computed.saturating_add(total_gaps);
        (total_used as f32 / self.available_size as f32).min(1.0) * 100.0
    }

    /// Format a single constraint for display.
    fn format_constraint(c: &Constraint) -> String {
        match c {
            Constraint::Fixed(n) => format!("Fixed({n})"),
            Constraint::Percentage(p) => format!("Pct({p:.0}%)"),
            Constraint::Min(n) => format!("Min({n})"),
            Constraint::Max(n) => format!("Max({n})"),
            Constraint::Ratio(n, d) => format!("Ratio({n}/{d})"),
            Constraint::Fill => "Fill".to_string(),
            Constraint::FitContent => "FitContent".to_string(),
            Constraint::FitContentBounded { min, max } => format!("FitContent({min}..{max})"),
            Constraint::FitMin => "FitMin".to_string(),
        }
    }

    /// Generate a human-readable summary.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{} ({:?}):", self.name, self.direction);
        let _ = writeln!(
            s,
            "  Input: {}x{} at ({},{})",
            self.input_area.width, self.input_area.height, self.input_area.x, self.input_area.y
        );
        let _ = writeln!(s, "  Available: {} (after margin)", self.available_size);
        let _ = writeln!(s, "  Gap: {}", self.gap);

        for (i, (constraint, size)) in self
            .constraints
            .iter()
            .zip(self.computed_sizes.iter())
            .enumerate()
        {
            let constraint_str = Self::format_constraint(constraint);
            let rect = self.result_rects.get(i);
            let rect_str = rect.map_or_else(
                || "?".to_string(),
                |r| format!("({},{} {}x{})", r.x, r.y, r.width, r.height),
            );
            let _ = writeln!(s, "  [{i}] {constraint_str} -> {size} @ {rect_str}");
        }

        let _ = writeln!(s, "  Utilization: {:.1}%", self.utilization());
        if self.has_overflow() {
            let _ = writeln!(s, "  ⚠ OVERFLOW");
        }
        if self.has_underflow() {
            let _ = writeln!(s, "  ⚠ UNDERFLOW (>20% unused)");
        }
        if let Some(t) = self.solve_time {
            let _ = writeln!(s, "  Solve time: {:?}", t);
        }
        s
    }

    /// Generate a JSONL-formatted record for structured logging.
    ///
    /// Returns a single-line JSON object suitable for appending to a log file.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let constraints_json: Vec<String> = self
            .constraints
            .iter()
            .map(|c| format!("\"{}\"", Self::format_constraint(c)))
            .collect();
        let sizes_json: Vec<String> = self.computed_sizes.iter().map(|s| s.to_string()).collect();
        let solve_time_us = self
            .solve_time
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);

        format!(
            r#"{{"event":"layout_solve","name":"{}","direction":"{:?}","alignment":"{:?}","available_size":{},"gap":{},"margin":{{"top":{},"right":{},"bottom":{},"left":{}}},"constraints":[{}],"computed_sizes":[{}],"utilization":{:.1},"has_overflow":{},"has_underflow":{},"solve_time_us":{}}}"#,
            self.name,
            self.direction,
            self.alignment,
            self.available_size,
            self.gap,
            self.margin.top,
            self.margin.right,
            self.margin.bottom,
            self.margin.left,
            constraints_json.join(","),
            sizes_json.join(","),
            self.utilization(),
            self.has_overflow(),
            self.has_underflow(),
            solve_time_us
        )
    }
}

/// A record of a grid layout solve operation.
#[derive(Debug, Clone)]
pub struct GridLayoutRecord {
    /// User-provided name for identification.
    pub name: String,
    /// Row constraints.
    pub row_constraints: Vec<Constraint>,
    /// Column constraints.
    pub col_constraints: Vec<Constraint>,
    /// Available width.
    pub available_width: u16,
    /// Available height.
    pub available_height: u16,
    /// Computed row heights.
    pub row_heights: Vec<u16>,
    /// Computed column widths.
    pub col_widths: Vec<u16>,
    /// The input area.
    pub input_area: Rect,
    /// Time taken to solve.
    pub solve_time: Option<Duration>,
}

impl GridLayoutRecord {
    /// Create a new grid layout record.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            row_constraints: Vec::new(),
            col_constraints: Vec::new(),
            available_width: 0,
            available_height: 0,
            row_heights: Vec::new(),
            col_widths: Vec::new(),
            input_area: Rect::default(),
            solve_time: None,
        }
    }

    /// Check for row overflow.
    pub fn has_row_overflow(&self) -> bool {
        self.row_heights.iter().sum::<u16>() > self.available_height
    }

    /// Check for column overflow.
    pub fn has_col_overflow(&self) -> bool {
        self.col_widths.iter().sum::<u16>() > self.available_width
    }

    /// Generate a JSONL-formatted record for structured logging.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let row_heights_json: Vec<String> =
            self.row_heights.iter().map(|h| h.to_string()).collect();
        let col_widths_json: Vec<String> = self.col_widths.iter().map(|w| w.to_string()).collect();
        let solve_time_us = self
            .solve_time
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);

        format!(
            r#"{{"event":"grid_layout_solve","name":"{}","available_width":{},"available_height":{},"row_heights":[{}],"col_widths":[{}],"has_row_overflow":{},"has_col_overflow":{},"solve_time_us":{}}}"#,
            self.name,
            self.available_width,
            self.available_height,
            row_heights_json.join(","),
            col_widths_json.join(","),
            self.has_row_overflow(),
            self.has_col_overflow(),
            solve_time_us
        )
    }
}

/// Telemetry hooks for layout debugging observability (bd-32my.5).
///
/// Provides callback-based notifications for layout events, enabling
/// external observability systems to monitor layout performance.
///
/// # Example
///
/// ```
/// use ftui_layout::debug::{LayoutDebugger, LayoutTelemetryHooks, LayoutRecord};
///
/// let hooks = LayoutTelemetryHooks::new()
///     .on_layout_solve(|record| {
///         println!("Layout solved: {} ({:.1}% util)", record.name, record.utilization());
///     })
///     .on_overflow(|record| {
///         eprintln!("OVERFLOW in {}", record.name);
///     });
///
/// let debugger = LayoutDebugger::new();
/// debugger.set_telemetry_hooks(hooks);
/// ```
type LayoutHook = Box<dyn Fn(&LayoutRecord) + Send + Sync>;
type GridHook = Box<dyn Fn(&GridLayoutRecord) + Send + Sync>;

pub struct LayoutTelemetryHooks {
    on_layout_solve: Option<LayoutHook>,
    on_grid_solve: Option<GridHook>,
    on_overflow: Option<LayoutHook>,
    on_underflow: Option<LayoutHook>,
}

impl Default for LayoutTelemetryHooks {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LayoutTelemetryHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutTelemetryHooks")
            .field("on_layout_solve", &self.on_layout_solve.is_some())
            .field("on_grid_solve", &self.on_grid_solve.is_some())
            .field("on_overflow", &self.on_overflow.is_some())
            .field("on_underflow", &self.on_underflow.is_some())
            .finish()
    }
}

impl LayoutTelemetryHooks {
    /// Create a new hooks instance with no callbacks attached.
    #[must_use]
    pub fn new() -> Self {
        Self {
            on_layout_solve: None,
            on_grid_solve: None,
            on_overflow: None,
            on_underflow: None,
        }
    }

    /// Attach a callback for flex layout solve events.
    #[must_use]
    pub fn on_layout_solve<F>(mut self, f: F) -> Self
    where
        F: Fn(&LayoutRecord) + Send + Sync + 'static,
    {
        self.on_layout_solve = Some(Box::new(f));
        self
    }

    /// Attach a callback for grid layout solve events.
    #[must_use]
    pub fn on_grid_solve<F>(mut self, f: F) -> Self
    where
        F: Fn(&GridLayoutRecord) + Send + Sync + 'static,
    {
        self.on_grid_solve = Some(Box::new(f));
        self
    }

    /// Attach a callback for layout overflow detection.
    #[must_use]
    pub fn on_overflow<F>(mut self, f: F) -> Self
    where
        F: Fn(&LayoutRecord) + Send + Sync + 'static,
    {
        self.on_overflow = Some(Box::new(f));
        self
    }

    /// Attach a callback for layout underflow detection.
    #[must_use]
    pub fn on_underflow<F>(mut self, f: F) -> Self
    where
        F: Fn(&LayoutRecord) + Send + Sync + 'static,
    {
        self.on_underflow = Some(Box::new(f));
        self
    }

    /// Fire the layout solve callback if attached.
    pub fn fire_layout_solve(&self, record: &LayoutRecord) {
        if let Some(ref f) = self.on_layout_solve {
            f(record);
        }
    }

    /// Fire the grid solve callback if attached.
    pub fn fire_grid_solve(&self, record: &GridLayoutRecord) {
        if let Some(ref f) = self.on_grid_solve {
            f(record);
        }
    }

    /// Fire the overflow callback if attached.
    pub fn fire_overflow(&self, record: &LayoutRecord) {
        if let Some(ref f) = self.on_overflow {
            f(record);
        }
    }

    /// Fire the underflow callback if attached.
    pub fn fire_underflow(&self, record: &LayoutRecord) {
        if let Some(ref f) = self.on_underflow {
            f(record);
        }
    }
}

/// Layout constraint debugger.
///
/// Collects layout solve records for introspection. Thread-safe via internal
/// synchronization; can be shared across the application.
///
/// Supports optional telemetry hooks for external observability (bd-32my.5).
pub struct LayoutDebugger {
    enabled: AtomicBool,
    records: Mutex<Vec<LayoutRecord>>,
    grid_records: Mutex<Vec<GridLayoutRecord>>,
    telemetry_hooks: Mutex<Option<LayoutTelemetryHooks>>,
}

impl std::fmt::Debug for LayoutDebugger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutDebugger")
            .field("enabled", &self.enabled.load(Ordering::Relaxed))
            .field("records_count", &self.records.lock().map(|r| r.len()).unwrap_or(0))
            .field("grid_records_count", &self.grid_records.lock().map(|r| r.len()).unwrap_or(0))
            .field("has_telemetry_hooks", &self.telemetry_hooks.lock().map(|h| h.is_some()).unwrap_or(false))
            .finish()
    }
}

impl LayoutDebugger {
    /// Create a new debugger wrapped in Arc (disabled by default).
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(false),
            records: Mutex::new(Vec::new()),
            grid_records: Mutex::new(Vec::new()),
            telemetry_hooks: Mutex::new(None),
        })
    }

    /// Attach telemetry hooks for external observability.
    pub fn set_telemetry_hooks(&self, hooks: LayoutTelemetryHooks) {
        if let Ok(mut h) = self.telemetry_hooks.lock() {
            *h = Some(hooks);
        }
    }

    /// Remove telemetry hooks.
    pub fn clear_telemetry_hooks(&self) {
        if let Ok(mut h) = self.telemetry_hooks.lock() {
            *h = None;
        }
    }

    /// Check if debugging is enabled.
    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Enable or disable debugging.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Toggle debugging on/off.
    pub fn toggle(&self) -> bool {
        !self.enabled.fetch_xor(true, Ordering::Relaxed)
    }

    /// Clear all recorded data.
    pub fn clear(&self) {
        if let Ok(mut records) = self.records.lock() {
            records.clear();
        }
        if let Ok(mut grid_records) = self.grid_records.lock() {
            grid_records.clear();
        }
    }

    /// Record a flex layout solve.
    ///
    /// Also fires telemetry hooks if attached:
    /// - `on_layout_solve` for every recorded layout
    /// - `on_overflow` if overflow detected
    /// - `on_underflow` if underflow detected
    pub fn record(&self, record: LayoutRecord) {
        if !self.enabled() {
            return;
        }

        // Fire telemetry hooks before recording
        if let Ok(hooks) = self.telemetry_hooks.lock()
            && let Some(ref h) = *hooks
        {
            h.fire_layout_solve(&record);
            if record.has_overflow() {
                h.fire_overflow(&record);
            }
            if record.has_underflow() {
                h.fire_underflow(&record);
            }
        }

        if let Ok(mut records) = self.records.lock() {
            records.push(record);
        }
    }

    /// Record a grid layout solve.
    ///
    /// Also fires telemetry hooks if attached.
    pub fn record_grid(&self, record: GridLayoutRecord) {
        if !self.enabled() {
            return;
        }

        // Fire telemetry hooks before recording
        if let Ok(hooks) = self.telemetry_hooks.lock()
            && let Some(ref h) = *hooks
        {
            h.fire_grid_solve(&record);
        }

        if let Ok(mut grid_records) = self.grid_records.lock() {
            grid_records.push(record);
        }
    }

    /// Get a snapshot of all flex layout records.
    pub fn snapshot(&self) -> Vec<LayoutRecord> {
        self.records
            .lock()
            .ok()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Get a snapshot of all grid layout records.
    pub fn snapshot_grids(&self) -> Vec<GridLayoutRecord> {
        self.grid_records
            .lock()
            .ok()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Get records with overflow conditions.
    pub fn overflows(&self) -> Vec<LayoutRecord> {
        self.snapshot()
            .into_iter()
            .filter(|r| r.has_overflow())
            .collect()
    }

    /// Get records with underflow conditions.
    pub fn underflows(&self) -> Vec<LayoutRecord> {
        self.snapshot()
            .into_iter()
            .filter(|r| r.has_underflow())
            .collect()
    }

    /// Generate a summary report of all recorded layouts.
    pub fn report(&self) -> String {
        let records = self.snapshot();
        let grid_records = self.snapshot_grids();

        let mut s = String::new();
        let _ = writeln!(
            s,
            "=== Layout Debug Report ({} flex, {} grid) ===",
            records.len(),
            grid_records.len()
        );

        let overflows: Vec<_> = records.iter().filter(|r| r.has_overflow()).collect();
        let underflows: Vec<_> = records.iter().filter(|r| r.has_underflow()).collect();

        if !overflows.is_empty() {
            let _ = writeln!(s, "\n⚠ {} layouts have OVERFLOW:", overflows.len());
            for r in &overflows {
                let _ = writeln!(s, "  - {}", r.name);
            }
        }

        if !underflows.is_empty() {
            let _ = writeln!(s, "\n⚠ {} layouts have UNDERFLOW:", underflows.len());
            for r in &underflows {
                let _ = writeln!(s, "  - {} ({:.1}% utilization)", r.name, r.utilization());
            }
        }

        let _ = writeln!(s, "\n--- Flex Layouts ---");
        for record in &records {
            let _ = write!(s, "\n{}", record.summary());
        }

        if !grid_records.is_empty() {
            let _ = writeln!(s, "\n--- Grid Layouts ---");
            for record in &grid_records {
                let _ = writeln!(s, "\n{} (Grid):", record.name);
                let _ = writeln!(
                    s,
                    "  Input: {}x{}",
                    record.input_area.width, record.input_area.height
                );
                let _ = writeln!(s, "  Rows: {:?}", record.row_heights);
                let _ = writeln!(s, "  Cols: {:?}", record.col_widths);
                if record.has_row_overflow() {
                    let _ = writeln!(s, "  ⚠ ROW OVERFLOW");
                }
                if record.has_col_overflow() {
                    let _ = writeln!(s, "  ⚠ COLUMN OVERFLOW");
                }
            }
        }

        s
    }

    /// Export to Graphviz DOT format for visualization.
    ///
    /// Each layout becomes a node, with edges representing parent-child
    /// relationships (if parent_index is set).
    pub fn export_dot(&self) -> String {
        let records = self.snapshot();

        let mut s = String::new();
        let _ = writeln!(s, "digraph LayoutDebug {{");
        let _ = writeln!(s, "  rankdir=TB;");
        let _ = writeln!(s, "  node [shape=record];");

        for (i, r) in records.iter().enumerate() {
            let color = if r.has_overflow() {
                "red"
            } else if r.has_underflow() {
                "yellow"
            } else {
                "green"
            };

            let label = format!(
                "{}|dir: {:?}|avail: {}|util: {:.0}%",
                r.name,
                r.direction,
                r.available_size,
                r.utilization()
            );

            let _ = writeln!(
                s,
                "  n{} [label=\"{{{}}}\", color=\"{}\"];",
                i, label, color
            );

            if let Some(parent) = r.parent_index {
                let _ = writeln!(s, "  n{} -> n{};", parent, i);
            }
        }

        let _ = writeln!(s, "}}");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_record_overflow_detection() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![60, 60];
        record.gap = 0;

        assert!(record.has_overflow());
    }

    #[test]
    fn layout_record_no_overflow() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![40, 40];
        record.gap = 0;

        assert!(!record.has_overflow());
    }

    #[test]
    fn layout_record_overflow_with_gaps() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![45, 45];
        record.gap = 15; // 45 + 15 + 45 = 105 > 100

        assert!(record.has_overflow());
    }

    #[test]
    fn layout_record_underflow_detection() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![20, 20]; // 40% utilization
        record.gap = 0;

        assert!(record.has_underflow());
    }

    #[test]
    fn layout_record_no_underflow() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![40, 45]; // 85% utilization
        record.gap = 0;

        assert!(!record.has_underflow());
    }

    #[test]
    fn layout_record_utilization() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![25, 25];
        record.gap = 0;

        assert!((record.utilization() - 50.0).abs() < 0.1);
    }

    #[test]
    fn layout_record_utilization_with_gap() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![20, 20];
        record.gap = 10; // 20 + 10 + 20 = 50

        assert!((record.utilization() - 50.0).abs() < 0.1);
    }

    #[test]
    fn layout_record_utilization_clamped() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![150]; // Overflow

        // Should clamp to 100%
        assert!((record.utilization() - 100.0).abs() < 0.1);
    }

    #[test]
    fn layout_record_zero_available() {
        let mut record = LayoutRecord::new("test");
        record.available_size = 0;
        record.computed_sizes = vec![];
        record.gap = 0;

        assert!(!record.has_overflow());
        assert!(!record.has_underflow());
        assert!((record.utilization() - 0.0).abs() < 0.1);
    }

    #[test]
    fn layout_record_summary() {
        let mut record = LayoutRecord::new("main_layout");
        record.constraints = vec![Constraint::Fixed(30), Constraint::Min(10)];
        record.available_size = 100;
        record.computed_sizes = vec![30, 70];
        record.direction = Direction::Horizontal;
        record.input_area = Rect::new(0, 0, 100, 50);
        record.result_rects = vec![Rect::new(0, 0, 30, 50), Rect::new(30, 0, 70, 50)];

        let summary = record.summary();
        assert!(summary.contains("main_layout"));
        assert!(summary.contains("Horizontal"));
        assert!(summary.contains("Fixed(30)"));
        assert!(summary.contains("Min(10)"));
    }

    #[test]
    fn debugger_disabled_by_default() {
        let debugger = LayoutDebugger::new();
        assert!(!debugger.enabled());
    }

    #[test]
    fn debugger_enable_disable() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        assert!(debugger.enabled());
        debugger.set_enabled(false);
        assert!(!debugger.enabled());
    }

    #[test]
    fn debugger_toggle() {
        let debugger = LayoutDebugger::new();
        assert!(!debugger.enabled());
        let result = debugger.toggle();
        assert!(result);
        assert!(debugger.enabled());
        let result = debugger.toggle();
        assert!(!result);
        assert!(!debugger.enabled());
    }

    #[test]
    fn debugger_record_when_disabled() {
        let debugger = LayoutDebugger::new();
        debugger.record(LayoutRecord::new("test"));
        assert!(debugger.snapshot().is_empty());
    }

    #[test]
    fn debugger_record_when_enabled() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new("test"));
        let records = debugger.snapshot();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "test");
    }

    #[test]
    fn debugger_clear() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(LayoutRecord::new("test1"));
        debugger.record(LayoutRecord::new("test2"));
        assert_eq!(debugger.snapshot().len(), 2);

        debugger.clear();
        assert!(debugger.snapshot().is_empty());
    }

    #[test]
    fn debugger_overflows() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let mut overflow_record = LayoutRecord::new("overflow");
        overflow_record.available_size = 100;
        overflow_record.computed_sizes = vec![60, 60];
        debugger.record(overflow_record);

        let mut normal_record = LayoutRecord::new("normal");
        normal_record.available_size = 100;
        normal_record.computed_sizes = vec![30, 30];
        debugger.record(normal_record);

        let overflows = debugger.overflows();
        assert_eq!(overflows.len(), 1);
        assert_eq!(overflows[0].name, "overflow");
    }

    #[test]
    fn debugger_underflows() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let mut underflow_record = LayoutRecord::new("underflow");
        underflow_record.available_size = 100;
        underflow_record.computed_sizes = vec![10, 10]; // 20% utilization
        debugger.record(underflow_record);

        let mut normal_record = LayoutRecord::new("normal");
        normal_record.available_size = 100;
        normal_record.computed_sizes = vec![45, 45]; // 90% utilization
        debugger.record(normal_record);

        let underflows = debugger.underflows();
        assert_eq!(underflows.len(), 1);
        assert_eq!(underflows[0].name, "underflow");
    }

    #[test]
    fn debugger_report() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let mut record = LayoutRecord::new("test_layout");
        record.available_size = 100;
        record.computed_sizes = vec![50, 50];
        record.direction = Direction::Horizontal;
        debugger.record(record);

        let report = debugger.report();
        assert!(report.contains("Layout Debug Report"));
        assert!(report.contains("test_layout"));
    }

    #[test]
    fn debugger_export_dot() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let mut record = LayoutRecord::new("root");
        record.available_size = 100;
        record.computed_sizes = vec![50, 50];
        record.direction = Direction::Vertical;
        debugger.record(record);

        let mut child = LayoutRecord::new("child");
        child.available_size = 50;
        child.computed_sizes = vec![25, 25];
        child.parent_index = Some(0);
        debugger.record(child);

        let dot = debugger.export_dot();
        assert!(dot.contains("digraph LayoutDebug"));
        assert!(dot.contains("root"));
        assert!(dot.contains("child"));
        assert!(dot.contains("n0 -> n1")); // Parent-child edge
    }

    #[test]
    fn debugger_export_dot_colors() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let mut overflow = LayoutRecord::new("overflow");
        overflow.available_size = 100;
        overflow.computed_sizes = vec![120];
        debugger.record(overflow);

        let mut underflow = LayoutRecord::new("underflow");
        underflow.available_size = 100;
        underflow.computed_sizes = vec![10];
        debugger.record(underflow);

        let mut normal = LayoutRecord::new("normal");
        normal.available_size = 100;
        normal.computed_sizes = vec![90];
        debugger.record(normal);

        let dot = debugger.export_dot();
        assert!(dot.contains("color=\"red\"")); // Overflow
        assert!(dot.contains("color=\"yellow\"")); // Underflow
        assert!(dot.contains("color=\"green\"")); // Normal
    }

    #[test]
    fn grid_record_overflow() {
        let mut record = GridLayoutRecord::new("grid");
        record.available_width = 100;
        record.available_height = 100;
        record.row_heights = vec![60, 60];
        record.col_widths = vec![50, 50];

        assert!(record.has_row_overflow());
        assert!(!record.has_col_overflow());
    }

    #[test]
    fn debugger_record_grid() {
        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let mut record = GridLayoutRecord::new("grid");
        record.available_width = 100;
        record.available_height = 100;
        record.row_heights = vec![50, 50];
        record.col_widths = vec![50, 50];
        debugger.record_grid(record);

        let records = debugger.snapshot_grids();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "grid");
    }

    #[test]
    fn format_constraint_all_types() {
        assert_eq!(
            LayoutRecord::format_constraint(&Constraint::Fixed(10)),
            "Fixed(10)"
        );
        assert_eq!(
            LayoutRecord::format_constraint(&Constraint::Percentage(50.0)),
            "Pct(50%)"
        );
        assert_eq!(
            LayoutRecord::format_constraint(&Constraint::Min(5)),
            "Min(5)"
        );
        assert_eq!(
            LayoutRecord::format_constraint(&Constraint::Max(20)),
            "Max(20)"
        );
        assert_eq!(
            LayoutRecord::format_constraint(&Constraint::Ratio(1, 3)),
            "Ratio(1/3)"
        );
    }

    // --- Telemetry tests (bd-32my.5) ---

    #[test]
    fn layout_record_to_jsonl() {
        let mut record = LayoutRecord::new("test_layout");
        record.constraints = vec![Constraint::Fixed(30), Constraint::Min(10)];
        record.available_size = 100;
        record.computed_sizes = vec![30, 70];
        record.direction = Direction::Horizontal;
        record.gap = 2;

        let jsonl = record.to_jsonl();
        assert!(jsonl.contains("\"event\":\"layout_solve\""));
        assert!(jsonl.contains("\"name\":\"test_layout\""));
        assert!(jsonl.contains("\"direction\":\"Horizontal\""));
        assert!(jsonl.contains("\"available_size\":100"));
        assert!(jsonl.contains("\"gap\":2"));
        assert!(jsonl.contains("\"Fixed(30)\""));
        assert!(jsonl.contains("\"Min(10)\""));
        assert!(jsonl.contains("\"computed_sizes\":[30,70]"));
        // Verify it's valid single-line JSON (no newlines)
        assert!(!jsonl.contains('\n'));
    }

    #[test]
    fn grid_record_to_jsonl() {
        let mut record = GridLayoutRecord::new("test_grid");
        record.available_width = 100;
        record.available_height = 50;
        record.row_heights = vec![10, 20, 20];
        record.col_widths = vec![30, 30, 40];

        let jsonl = record.to_jsonl();
        assert!(jsonl.contains("\"event\":\"grid_layout_solve\""));
        assert!(jsonl.contains("\"name\":\"test_grid\""));
        assert!(jsonl.contains("\"available_width\":100"));
        assert!(jsonl.contains("\"available_height\":50"));
        assert!(jsonl.contains("\"row_heights\":[10,20,20]"));
        assert!(jsonl.contains("\"col_widths\":[30,30,40]"));
        assert!(jsonl.contains("\"has_row_overflow\":false"));
        assert!(jsonl.contains("\"has_col_overflow\":false"));
        assert!(!jsonl.contains('\n'));
    }

    #[test]
    fn telemetry_hooks_fire_on_layout_solve() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let hooks = LayoutTelemetryHooks::new()
            .on_layout_solve(move |_record| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            });

        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.set_telemetry_hooks(hooks);

        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![50, 50];
        debugger.record(record);

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn telemetry_hooks_fire_on_overflow() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let overflow_counter = Arc::new(AtomicU32::new(0));
        let overflow_clone = overflow_counter.clone();

        let hooks = LayoutTelemetryHooks::new()
            .on_overflow(move |_record| {
                overflow_clone.fetch_add(1, Ordering::SeqCst);
            });

        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.set_telemetry_hooks(hooks);

        // Record with overflow
        let mut overflow_record = LayoutRecord::new("overflow");
        overflow_record.available_size = 100;
        overflow_record.computed_sizes = vec![60, 60]; // 120 > 100
        debugger.record(overflow_record);

        // Record without overflow
        let mut normal_record = LayoutRecord::new("normal");
        normal_record.available_size = 100;
        normal_record.computed_sizes = vec![30, 30];
        debugger.record(normal_record);

        // Only the overflow record should have triggered the hook
        assert_eq!(overflow_counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn telemetry_hooks_fire_on_underflow() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let underflow_counter = Arc::new(AtomicU32::new(0));
        let underflow_clone = underflow_counter.clone();

        let hooks = LayoutTelemetryHooks::new()
            .on_underflow(move |_record| {
                underflow_clone.fetch_add(1, Ordering::SeqCst);
            });

        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.set_telemetry_hooks(hooks);

        // Record with underflow (< 80% utilization)
        let mut underflow_record = LayoutRecord::new("underflow");
        underflow_record.available_size = 100;
        underflow_record.computed_sizes = vec![10, 10]; // 20% utilization
        debugger.record(underflow_record);

        assert_eq!(underflow_counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn telemetry_hooks_fire_on_grid_solve() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let hooks = LayoutTelemetryHooks::new()
            .on_grid_solve(move |_record| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            });

        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.set_telemetry_hooks(hooks);

        let mut record = GridLayoutRecord::new("grid");
        record.available_width = 100;
        record.available_height = 50;
        record.row_heights = vec![25, 25];
        record.col_widths = vec![50, 50];
        debugger.record_grid(record);

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn telemetry_hooks_not_fired_when_disabled() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let hooks = LayoutTelemetryHooks::new()
            .on_layout_solve(move |_record| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            });

        let debugger = LayoutDebugger::new();
        // Note: NOT enabled
        debugger.set_telemetry_hooks(hooks);

        let mut record = LayoutRecord::new("test");
        record.available_size = 100;
        record.computed_sizes = vec![50, 50];
        debugger.record(record);

        // Hook should not fire because debugger is disabled
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn clear_telemetry_hooks() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let hooks = LayoutTelemetryHooks::new()
            .on_layout_solve(move |_record| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            });

        let debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.set_telemetry_hooks(hooks);

        let mut record1 = LayoutRecord::new("test1");
        record1.available_size = 100;
        record1.computed_sizes = vec![50, 50];
        debugger.record(record1);

        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Clear hooks
        debugger.clear_telemetry_hooks();

        let mut record2 = LayoutRecord::new("test2");
        record2.available_size = 100;
        record2.computed_sizes = vec![50, 50];
        debugger.record(record2);

        // Counter should still be 1 (hooks cleared)
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn layout_record_jsonl_overflow_flags() {
        let mut record = LayoutRecord::new("overflow_test");
        record.available_size = 100;
        record.computed_sizes = vec![60, 60]; // Overflow

        let jsonl = record.to_jsonl();
        assert!(jsonl.contains("\"has_overflow\":true"));
    }

    #[test]
    fn layout_record_jsonl_underflow_flags() {
        let mut record = LayoutRecord::new("underflow_test");
        record.available_size = 100;
        record.computed_sizes = vec![10, 10]; // 20% utilization

        let jsonl = record.to_jsonl();
        assert!(jsonl.contains("\"has_underflow\":true"));
    }
}
