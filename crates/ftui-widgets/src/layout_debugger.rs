#![forbid(unsafe_code)]

//! Layout constraint debugger utilities.
//!
//! Provides a lightweight recorder and renderer for layout constraint
//! diagnostics. This is intended for developer tooling and can be kept
//! disabled in production to avoid overhead.

use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::Draw;

#[cfg(feature = "tracing")]
use tracing::{debug, warn};

/// Constraint bounds for a widget's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutConstraints {
    /// Minimum allowed width.
    pub min_width: u16,
    /// Maximum allowed width (0 = unconstrained).
    pub max_width: u16,
    /// Minimum allowed height.
    pub min_height: u16,
    /// Maximum allowed height (0 = unconstrained).
    pub max_height: u16,
}

impl LayoutConstraints {
    /// Create constraints with the given bounds.
    pub fn new(min_width: u16, max_width: u16, min_height: u16, max_height: u16) -> Self {
        Self {
            min_width,
            max_width,
            min_height,
            max_height,
        }
    }

    /// Create unconstrained (all-zero) bounds.
    pub fn unconstrained() -> Self {
        Self {
            min_width: 0,
            max_width: 0,
            min_height: 0,
            max_height: 0,
        }
    }

    fn width_overflow(&self, width: u16) -> bool {
        self.max_width != 0 && width > self.max_width
    }

    fn height_overflow(&self, height: u16) -> bool {
        self.max_height != 0 && height > self.max_height
    }

    fn width_underflow(&self, width: u16) -> bool {
        width < self.min_width
    }

    fn height_underflow(&self, height: u16) -> bool {
        height < self.min_height
    }
}

/// Layout record for a single widget.
#[derive(Debug, Clone)]
pub struct LayoutRecord {
    /// Name of the widget this record describes.
    pub widget_name: String,
    /// Area originally requested by the widget.
    pub area_requested: Rect,
    /// Area actually received after layout.
    pub area_received: Rect,
    /// Constraint bounds applied during layout.
    pub constraints: LayoutConstraints,
    /// Child layout records for nested widgets.
    pub children: Vec<LayoutRecord>,
}

impl LayoutRecord {
    /// Create a new layout record for the given widget.
    pub fn new(
        name: impl Into<String>,
        area_requested: Rect,
        area_received: Rect,
        constraints: LayoutConstraints,
    ) -> Self {
        Self {
            widget_name: name.into(),
            area_requested,
            area_received,
            constraints,
            children: Vec::new(),
        }
    }

    /// Add a child record to this layout record.
    #[must_use]
    pub fn with_child(mut self, child: LayoutRecord) -> Self {
        self.children.push(child);
        self
    }

    fn overflow(&self) -> bool {
        self.constraints.width_overflow(self.area_received.width)
            || self.constraints.height_overflow(self.area_received.height)
    }

    fn underflow(&self) -> bool {
        self.constraints.width_underflow(self.area_received.width)
            || self.constraints.height_underflow(self.area_received.height)
    }
}

/// Layout debugger that records constraint data and renders diagnostics.
#[derive(Debug, Default)]
pub struct LayoutDebugger {
    enabled: bool,
    records: Vec<LayoutRecord>,
}

impl LayoutDebugger {
    /// Create a new disabled layout debugger.
    pub fn new() -> Self {
        Self {
            enabled: false,
            records: Vec::new(),
        }
    }

    /// Enable or disable the debugger.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Returns whether the debugger is enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Clear all recorded layout data.
    pub fn clear(&mut self) {
        self.records.clear();
    }

    /// Record a layout computation result.
    pub fn record(&mut self, record: LayoutRecord) {
        if !self.enabled {
            return;
        }
        #[cfg(feature = "tracing")]
        {
            if record.overflow() || record.underflow() {
                warn!(
                    widget = record.widget_name.as_str(),
                    requested = ?record.area_requested,
                    received = ?record.area_received,
                    "Layout constraint violation"
                );
            }
            debug!(
                widget = record.widget_name.as_str(),
                constraints = ?record.constraints,
                result = ?record.area_received,
                "Layout computed"
            );
        }
        self.records.push(record);
    }

    /// Get the recorded layout data.
    pub fn records(&self) -> &[LayoutRecord] {
        &self.records
    }

    /// Render a simple tree view of layout records into the buffer.
    pub fn render_debug(&self, area: Rect, buf: &mut Buffer) {
        if !self.enabled {
            return;
        }
        let mut y = area.y;
        for record in &self.records {
            y = self.render_record(record, 0, area, y, buf);
            if y >= area.bottom() {
                break;
            }
        }
    }

    /// Export recorded layout data as Graphviz DOT.
    pub fn export_dot(&self) -> String {
        let mut out = String::from("digraph Layout {\n  node [shape=box];\n");
        let mut next_id = 0usize;
        for record in &self.records {
            next_id = write_dot_record(&mut out, record, next_id, None);
        }
        out.push_str("}\n");
        out
    }

    fn render_record(
        &self,
        record: &LayoutRecord,
        depth: usize,
        area: Rect,
        y: u16,
        buf: &mut Buffer,
    ) -> u16 {
        if y >= area.bottom() {
            return y;
        }

        let indent = " ".repeat(depth * 2);
        let line = format!(
            "{}{} req={}x{} got={}x{} min={}x{} max={}x{}",
            indent,
            record.widget_name,
            record.area_requested.width,
            record.area_requested.height,
            record.area_received.width,
            record.area_received.height,
            record.constraints.min_width,
            record.constraints.min_height,
            record.constraints.max_width,
            record.constraints.max_height,
        );

        let color = if record.overflow() {
            PackedRgba::rgb(240, 80, 80)
        } else if record.underflow() {
            PackedRgba::rgb(240, 200, 80)
        } else {
            PackedRgba::rgb(200, 200, 200)
        };

        let cell = Cell::from_char(' ').with_fg(color);
        let _ = buf.print_text_clipped(area.x, y, &line, cell, area.right());

        let mut next_y = y.saturating_add(1);
        for child in &record.children {
            next_y = self.render_record(child, depth + 1, area, next_y, buf);
            if next_y >= area.bottom() {
                break;
            }
        }
        next_y
    }
}

fn write_dot_record(
    out: &mut String,
    record: &LayoutRecord,
    id: usize,
    parent: Option<usize>,
) -> usize {
    let safe_name = record.widget_name.replace('"', "'");
    let label = format!(
        "{}\\nreq={}x{} got={}x{}",
        safe_name,
        record.area_requested.width,
        record.area_requested.height,
        record.area_received.width,
        record.area_received.height
    );
    out.push_str(&format!("  n{} [label=\"{}\"];\n", id, label));
    if let Some(parent_id) = parent {
        out.push_str(&format!("  n{} -> n{};\n", parent_id, id));
    }

    let mut next_id = id + 1;
    for child in &record.children {
        next_id = write_dot_record(out, child, next_id, Some(id));
    }
    next_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_dot_contains_nodes_and_edges() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        let record = LayoutRecord::new(
            "Root",
            Rect::new(0, 0, 10, 4),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::new(5, 12, 2, 6),
        )
        .with_child(LayoutRecord::new(
            "Child",
            Rect::new(0, 0, 5, 2),
            Rect::new(0, 0, 5, 2),
            LayoutConstraints::unconstrained(),
        ));
        dbg.record(record);

        let dot = dbg.export_dot();
        assert!(dot.contains("Root"));
        assert!(dot.contains("Child"));
        assert!(dot.contains("->"));
    }

    #[test]
    fn render_debug_writes_lines() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        dbg.record(LayoutRecord::new(
            "Root",
            Rect::new(0, 0, 10, 4),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::new(9, 0, 0, 0),
        ));

        let mut buf = Buffer::new(30, 4);
        dbg.render_debug(Rect::new(0, 0, 30, 4), &mut buf);

        let cell = buf.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('R'));
    }

    #[test]
    fn disabled_debugger_is_noop() {
        let mut dbg = LayoutDebugger::new();
        dbg.record(LayoutRecord::new(
            "Root",
            Rect::new(0, 0, 10, 4),
            Rect::new(0, 0, 8, 4),
            LayoutConstraints::unconstrained(),
        ));
        assert!(dbg.records().is_empty());
    }

    // --- LayoutConstraints ---

    #[test]
    fn constraints_new_and_fields() {
        let c = LayoutConstraints::new(5, 80, 3, 24);
        assert_eq!(c.min_width, 5);
        assert_eq!(c.max_width, 80);
        assert_eq!(c.min_height, 3);
        assert_eq!(c.max_height, 24);
    }

    #[test]
    fn constraints_unconstrained_all_zero() {
        let c = LayoutConstraints::unconstrained();
        assert_eq!(c.min_width, 0);
        assert_eq!(c.max_width, 0);
        assert_eq!(c.min_height, 0);
        assert_eq!(c.max_height, 0);
    }

    #[test]
    fn constraints_width_overflow() {
        let c = LayoutConstraints::new(0, 10, 0, 0);
        assert!(!c.width_overflow(10)); // at max = ok
        assert!(c.width_overflow(11)); // over max = overflow
        assert!(!c.width_overflow(5)); // under max = ok
    }

    #[test]
    fn constraints_width_overflow_unconstrained() {
        let c = LayoutConstraints::new(0, 0, 0, 0); // max_width=0 = unconstrained
        assert!(!c.width_overflow(9999)); // never overflows
    }

    #[test]
    fn constraints_height_overflow() {
        let c = LayoutConstraints::new(0, 0, 0, 10);
        assert!(!c.height_overflow(10));
        assert!(c.height_overflow(11));
    }

    #[test]
    fn constraints_width_underflow() {
        let c = LayoutConstraints::new(5, 0, 0, 0);
        assert!(!c.width_underflow(5)); // at min = ok
        assert!(c.width_underflow(4)); // below min = underflow
        assert!(!c.width_underflow(10)); // above min = ok
    }

    #[test]
    fn constraints_height_underflow() {
        let c = LayoutConstraints::new(0, 0, 3, 0);
        assert!(!c.height_underflow(3));
        assert!(c.height_underflow(2));
    }

    // --- LayoutRecord ---

    #[test]
    fn record_new_and_fields() {
        let r = LayoutRecord::new(
            "MyWidget",
            Rect::new(0, 0, 20, 10),
            Rect::new(0, 0, 15, 8),
            LayoutConstraints::new(5, 25, 3, 12),
        );
        assert_eq!(r.widget_name, "MyWidget");
        assert_eq!(r.area_requested.width, 20);
        assert_eq!(r.area_received.width, 15);
        assert!(r.children.is_empty());
    }

    #[test]
    fn record_with_child_appends() {
        let parent = LayoutRecord::new(
            "Parent",
            Rect::new(0, 0, 20, 10),
            Rect::new(0, 0, 20, 10),
            LayoutConstraints::unconstrained(),
        )
        .with_child(LayoutRecord::new(
            "Child1",
            Rect::new(0, 0, 10, 5),
            Rect::new(0, 0, 10, 5),
            LayoutConstraints::unconstrained(),
        ))
        .with_child(LayoutRecord::new(
            "Child2",
            Rect::new(10, 0, 10, 5),
            Rect::new(10, 0, 10, 5),
            LayoutConstraints::unconstrained(),
        ));
        assert_eq!(parent.children.len(), 2);
        assert_eq!(parent.children[0].widget_name, "Child1");
        assert_eq!(parent.children[1].widget_name, "Child2");
    }

    #[test]
    fn record_overflow_detected() {
        // width overflow
        let r = LayoutRecord::new(
            "Widget",
            Rect::new(0, 0, 20, 10),
            Rect::new(0, 0, 20, 10),
            LayoutConstraints::new(0, 15, 0, 0), // max_width=15, received=20
        );
        assert!(r.overflow());
    }

    #[test]
    fn record_underflow_detected() {
        let r = LayoutRecord::new(
            "Widget",
            Rect::new(0, 0, 20, 10),
            Rect::new(0, 0, 3, 10),
            LayoutConstraints::new(5, 0, 0, 0), // min_width=5, received=3
        );
        assert!(r.underflow());
    }

    #[test]
    fn record_no_violation() {
        let r = LayoutRecord::new(
            "Widget",
            Rect::new(0, 0, 10, 5),
            Rect::new(0, 0, 10, 5),
            LayoutConstraints::new(5, 15, 3, 8),
        );
        assert!(!r.overflow());
        assert!(!r.underflow());
    }

    // --- LayoutDebugger ---

    #[test]
    fn debugger_default_disabled() {
        let dbg = LayoutDebugger::new();
        assert!(!dbg.enabled());
        assert!(dbg.records().is_empty());
    }

    #[test]
    fn debugger_enable_disable() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        assert!(dbg.enabled());
        dbg.set_enabled(false);
        assert!(!dbg.enabled());
    }

    #[test]
    fn debugger_clear() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        dbg.record(LayoutRecord::new(
            "Widget",
            Rect::new(0, 0, 10, 5),
            Rect::new(0, 0, 10, 5),
            LayoutConstraints::unconstrained(),
        ));
        assert_eq!(dbg.records().len(), 1);
        dbg.clear();
        assert!(dbg.records().is_empty());
    }

    #[test]
    fn debugger_records_multiple() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        for i in 0..5 {
            dbg.record(LayoutRecord::new(
                format!("W{i}"),
                Rect::new(0, 0, 10, 5),
                Rect::new(0, 0, 10, 5),
                LayoutConstraints::unconstrained(),
            ));
        }
        assert_eq!(dbg.records().len(), 5);
    }

    // --- export_dot edge cases ---

    #[test]
    fn export_dot_empty() {
        let dbg = LayoutDebugger::new();
        let dot = dbg.export_dot();
        assert!(dot.starts_with("digraph Layout"));
        assert!(dot.ends_with(
            "}
"
        ));
        assert!(!dot.contains("n0"));
    }

    #[test]
    fn export_dot_escapes_quotes() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        // Name containing a double-quote character
        let name = String::from("Wid") + &String::from('"') + "get";
        dbg.record(LayoutRecord::new(
            &name,
            Rect::new(0, 0, 10, 5),
            Rect::new(0, 0, 10, 5),
            LayoutConstraints::unconstrained(),
        ));
        let dot = dbg.export_dot();
        // Double quotes should be replaced with single quotes
        assert!(dot.contains("Wid'get"));
    }

    #[test]
    fn export_dot_nested_children() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        let root = LayoutRecord::new(
            "Root",
            Rect::new(0, 0, 40, 20),
            Rect::new(0, 0, 40, 20),
            LayoutConstraints::unconstrained(),
        )
        .with_child(
            LayoutRecord::new(
                "Mid",
                Rect::new(0, 0, 20, 10),
                Rect::new(0, 0, 20, 10),
                LayoutConstraints::unconstrained(),
            )
            .with_child(LayoutRecord::new(
                "Leaf",
                Rect::new(0, 0, 10, 5),
                Rect::new(0, 0, 10, 5),
                LayoutConstraints::unconstrained(),
            )),
        );
        dbg.record(root);
        let dot = dbg.export_dot();
        assert!(dot.contains("Root"));
        assert!(dot.contains("Mid"));
        assert!(dot.contains("Leaf"));
        // Should have edges: n0->n1, n1->n2
        assert!(dot.contains("n0 -> n1"));
        assert!(dot.contains("n1 -> n2"));
    }

    // --- render_debug edge cases ---

    #[test]
    fn render_debug_disabled_noop() {
        let dbg = LayoutDebugger::new(); // disabled
        let mut buf = Buffer::new(30, 4);
        let blank_cell = *buf.get(0, 0).unwrap();
        dbg.render_debug(Rect::new(0, 0, 30, 4), &mut buf);
        assert_eq!(*buf.get(0, 0).unwrap(), blank_cell);
    }

    #[test]
    fn render_debug_overflow_uses_red_color() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        dbg.record(LayoutRecord::new(
            "Over",
            Rect::new(0, 0, 20, 10),
            Rect::new(0, 0, 20, 10),
            LayoutConstraints::new(0, 10, 0, 0), // width overflows
        ));
        let mut buf = Buffer::new(60, 4);
        dbg.render_debug(Rect::new(0, 0, 60, 4), &mut buf);
        let cell = buf.get(0, 0).unwrap();
        // Should be red-ish (240, 80, 80)
        assert_eq!(cell.fg, PackedRgba::rgb(240, 80, 80));
    }

    #[test]
    fn render_debug_underflow_uses_yellow_color() {
        let mut dbg = LayoutDebugger::new();
        dbg.set_enabled(true);
        dbg.record(LayoutRecord::new(
            "Under",
            Rect::new(0, 0, 20, 10),
            Rect::new(0, 0, 3, 10),
            LayoutConstraints::new(5, 0, 0, 0), // width underflows
        ));
        let mut buf = Buffer::new(60, 4);
        dbg.render_debug(Rect::new(0, 0, 60, 4), &mut buf);
        let cell = buf.get(0, 0).unwrap();
        // Should be yellow-ish (240, 200, 80)
        assert_eq!(cell.fg, PackedRgba::rgb(240, 200, 80));
    }
}
