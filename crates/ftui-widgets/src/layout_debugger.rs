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

/// Constraint bounds for a widget's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutConstraints {
    pub min_width: u16,
    pub max_width: u16,
    pub min_height: u16,
    pub max_height: u16,
}

impl LayoutConstraints {
    pub fn new(min_width: u16, max_width: u16, min_height: u16, max_height: u16) -> Self {
        Self {
            min_width,
            max_width,
            min_height,
            max_height,
        }
    }

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
    pub widget_name: String,
    pub area_requested: Rect,
    pub area_received: Rect,
    pub constraints: LayoutConstraints,
    pub children: Vec<LayoutRecord>,
}

impl LayoutRecord {
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

    pub fn with_child(mut self, child: LayoutRecord) -> Self {
        self.children.push(child);
        self
    }

    fn overflow(&self) -> bool {
        self.constraints
            .width_overflow(self.area_received.width)
            || self
                .constraints
                .height_overflow(self.area_received.height)
    }

    fn underflow(&self) -> bool {
        self.constraints
            .width_underflow(self.area_received.width)
            || self
                .constraints
                .height_underflow(self.area_received.height)
    }
}

/// Layout debugger that records constraint data and renders diagnostics.
#[derive(Debug, Default)]
pub struct LayoutDebugger {
    enabled: bool,
    records: Vec<LayoutRecord>,
}

impl LayoutDebugger {
    pub fn new() -> Self {
        Self {
            enabled: false,
            records: Vec::new(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn clear(&mut self) {
        self.records.clear();
    }

    pub fn record(&mut self, record: LayoutRecord) {
        if !self.enabled {
            return;
        }
        self.records.push(record);
    }

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
}
