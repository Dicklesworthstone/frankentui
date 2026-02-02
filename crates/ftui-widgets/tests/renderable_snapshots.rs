#![forbid(unsafe_code)]

//! Renderable Snapshot Tests
//!
//! Exercises the 12 rich renderables through the Frame pipeline,
//! verifying buffer output, measurement invariants, and composition.

use ftui_core::geometry::{Rect, Sides};
use ftui_layout::Constraint;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::Alignment;
use ftui_widgets::borders::BorderType;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a single row from the frame buffer as a String.
fn row_text(frame: &Frame, y: u16, width: u16) -> String {
    let mut out = String::new();
    for x in 0..width {
        if let Some(cell) = frame.buffer.get(x, y) {
            if let Some(ch) = cell.content.as_char() {
                out.push(ch);
            } else {
                out.push(' ');
            }
        }
    }
    out.trim_end().to_string()
}

/// Check that the frame buffer is non-empty (at least one non-space cell).
fn has_content(frame: &Frame, area: Rect) -> bool {
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = frame.buffer.get(x, y)
                && cell.content.as_char().is_some_and(|c| c != ' ')
            {
                return true;
            }
        }
    }
    false
}

// ===========================================================================
// Align
// ===========================================================================

mod align_tests {
    use super::*;
    use ftui_widgets::align::{Align, VerticalAlignment};
    use ftui_widgets::paragraph::Paragraph;

    #[test]
    fn align_center_centers_child() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);
        let area = Rect::new(0, 0, 20, 5);

        let widget = Align::new(Paragraph::new("Hi"))
            .horizontal(Alignment::Center)
            .vertical(VerticalAlignment::Middle)
            .child_width(2)
            .child_height(1);

        widget.render(area, &mut frame);

        // "Hi" should appear somewhere in the middle, not at (0,0)
        let c00 = frame.buffer.get(0, 0).unwrap().content.as_char();
        assert_ne!(c00, Some('H'), "should not be at top-left when centered");

        // Find "H" in the buffer
        let mut found = false;
        for y in 0..5 {
            for x in 0..20 {
                if frame.buffer.get(x, y).unwrap().content.as_char() == Some('H') {
                    assert!(x > 0, "H should be offset from left edge");
                    assert!(y > 0, "H should be offset from top edge");
                    found = true;
                }
            }
        }
        assert!(found, "should find 'H' somewhere in the buffer");
    }

    #[test]
    fn align_right_bottom() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 4, &mut pool);
        let area = Rect::new(0, 0, 10, 4);

        let widget = Align::new(Paragraph::new("X"))
            .horizontal(Alignment::Right)
            .vertical(VerticalAlignment::Bottom)
            .child_width(1)
            .child_height(1);

        widget.render(area, &mut frame);

        // X should be at bottom-right corner
        let cell = frame.buffer.get(9, 3).unwrap();
        assert_eq!(cell.content.as_char(), Some('X'));
    }

    #[test]
    fn align_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Align::new(Paragraph::new("test"))
            .render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Columns
// ===========================================================================

mod columns_tests {
    use super::*;
    use ftui_widgets::columns::Columns;
    use ftui_widgets::paragraph::Paragraph;

    #[test]
    fn columns_two_equal_splits_area() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let area = Rect::new(0, 0, 20, 1);

        let widget = Columns::new()
            .column(Paragraph::new("L"), Constraint::Ratio(1, 2))
            .column(Paragraph::new("R"), Constraint::Ratio(1, 2));

        widget.render(area, &mut frame);

        // Left column should have L, right column should have R
        let left = frame.buffer.get(0, 0).unwrap().content.as_char();
        assert_eq!(left, Some('L'));

        // R should be in the second half
        let mut found_r = false;
        for x in 5..20 {
            if frame.buffer.get(x, 0).unwrap().content.as_char() == Some('R') {
                found_r = true;
                break;
            }
        }
        assert!(found_r, "R should appear in second half of columns");
    }

    #[test]
    fn columns_with_gap() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(21, 1, &mut pool);
        let area = Rect::new(0, 0, 21, 1);

        let widget = Columns::new()
            .gap(1)
            .column(Paragraph::new("A"), Constraint::Ratio(1, 2))
            .column(Paragraph::new("B"), Constraint::Ratio(1, 2));

        widget.render(area, &mut frame);

        let a = frame.buffer.get(0, 0).unwrap().content.as_char();
        assert_eq!(a, Some('A'));
        assert!(has_content(&frame, area));
    }

    #[test]
    fn columns_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Columns::new()
            .add(Paragraph::new("x"))
            .render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Emoji
// ===========================================================================

mod emoji_tests {
    use super::*;
    use ftui_widgets::emoji::Emoji;

    #[test]
    fn emoji_renders_text() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let area = Rect::new(0, 0, 10, 1);

        let widget = Emoji::new("OK");
        widget.render(area, &mut frame);

        let c0 = frame.buffer.get(0, 0).unwrap().content.as_char();
        let c1 = frame.buffer.get(1, 0).unwrap().content.as_char();
        assert_eq!(c0, Some('O'));
        assert_eq!(c1, Some('K'));
    }

    #[test]
    fn emoji_width_matches_text() {
        let e = Emoji::new("Hi");
        assert_eq!(e.width(), 2);
    }

    #[test]
    fn emoji_fallback() {
        let e = Emoji::new("🚀").with_fallback("rocket");
        assert_eq!(e.text(), "🚀");
        assert_eq!(e.fallback(), Some("rocket"));
    }

    #[test]
    fn emoji_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Emoji::new("test").render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Group
// ===========================================================================

mod group_tests {
    use super::*;
    use ftui_widgets::group::Group;
    use ftui_widgets::paragraph::Paragraph;

    #[test]
    fn group_renders_all_children_in_order() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        let area = Rect::new(0, 0, 5, 1);

        // Second child overwrites first since they render to same area
        let widget = Group::new()
            .push(Paragraph::new("AAAAA"))
            .push(Paragraph::new("B"));

        widget.render(area, &mut frame);

        // First cell should be B (overwrites A), rest should be A
        let c0 = frame.buffer.get(0, 0).unwrap().content.as_char();
        let c1 = frame.buffer.get(1, 0).unwrap().content.as_char();
        assert_eq!(c0, Some('B'));
        assert_eq!(c1, Some('A'));
    }

    #[test]
    fn group_empty_renders_nothing() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);
        let area = Rect::new(0, 0, 5, 1);

        let widget = Group::new();
        assert!(widget.is_empty());
        widget.render(area, &mut frame);

        // Should be all spaces/empty
        assert!(!has_content(&frame, area));
    }

    #[test]
    fn group_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Group::new()
            .push(Paragraph::new("x"))
            .render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// JsonView
// ===========================================================================

mod json_view_tests {
    use super::*;
    use ftui_widgets::json_view::JsonView;

    #[test]
    fn json_view_renders_simple_object() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        let area = Rect::new(0, 0, 30, 5);

        let widget = JsonView::new(r#"{"key": "value"}"#);
        widget.render(area, &mut frame);

        assert!(has_content(&frame, area), "JSON should render content");

        // Should contain the key somewhere
        let line0 = row_text(&frame, 0, 30);
        let line1 = row_text(&frame, 1, 30);
        let all = format!("{line0} {line1}");
        assert!(
            all.contains("key") || all.contains("{"),
            "should contain JSON structure, got: {all:?}"
        );
    }

    #[test]
    fn json_view_invalid_json_shows_error() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 3, &mut pool);
        let area = Rect::new(0, 0, 30, 3);

        let widget = JsonView::new("not valid json {{{");
        widget.render(area, &mut frame);

        // Should still render something (error display), not crash
        assert!(has_content(&frame, area), "invalid JSON should still render");
    }

    #[test]
    fn json_view_formatted_lines() {
        let jv = JsonView::new(r#"{"a": 1}"#);
        let lines = jv.formatted_lines();
        assert!(!lines.is_empty(), "formatted_lines should produce output");
    }

    #[test]
    fn json_view_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        JsonView::new("{}").render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Padding
// ===========================================================================

mod padding_tests {
    use super::*;
    use ftui_widgets::padding::Padding;
    use ftui_widgets::paragraph::Paragraph;

    #[test]
    fn padding_offsets_child_content() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);

        let widget = Padding::new(
            Paragraph::new("X"),
            Sides::new(2, 2, 1, 1), // top, right, bottom, left
        );

        widget.render(area, &mut frame);

        // (0,0) should NOT have X due to padding
        let c00 = frame.buffer.get(0, 0).unwrap().content.as_char();
        assert_ne!(c00, Some('X'), "padding should offset content from top-left");

        // X should appear at offset (left=1, top=2)
        let cx = frame.buffer.get(1, 2).unwrap().content.as_char();
        assert_eq!(cx, Some('X'), "X should appear at padded offset");
    }

    #[test]
    fn padding_inner_area_calculation() {
        let padding = Padding::new(
            Paragraph::new("test"),
            Sides::new(1, 1, 1, 1),
        );
        let inner = padding.inner_area(Rect::new(0, 0, 10, 10));
        assert_eq!(inner.x, 1);
        assert_eq!(inner.y, 1);
        assert_eq!(inner.width, 8);
        assert_eq!(inner.height, 8);
    }

    #[test]
    fn padding_larger_than_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(3, 3, &mut pool);

        // Padding larger than the area
        let widget = Padding::new(
            Paragraph::new("X"),
            Sides::new(5, 5, 5, 5),
        );
        widget.render(Rect::new(0, 0, 3, 3), &mut frame);
        // Should not panic, child just gets zero area
    }

    #[test]
    fn padding_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Padding::new(Paragraph::new("x"), Sides::new(0, 0, 0, 0))
            .render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Panel
// ===========================================================================

mod panel_tests {
    use super::*;
    use ftui_widgets::borders::Borders;
    use ftui_widgets::panel::Panel;
    use ftui_widgets::paragraph::Paragraph;

    #[test]
    fn panel_renders_border_and_child() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);

        let widget = Panel::new(Paragraph::new("Hi"))
            .borders(Borders::ALL)
            .border_type(BorderType::Ascii);

        widget.render(area, &mut frame);

        // Top-left corner should be border char
        let tl = frame.buffer.get(0, 0).unwrap().content.as_char();
        assert_eq!(tl, Some('+'), "top-left should be ASCII border corner");

        // Content "Hi" should be inside the border
        let c1 = frame.buffer.get(1, 1).unwrap().content.as_char();
        assert_eq!(c1, Some('H'), "child content should render inside border");
    }

    #[test]
    fn panel_with_title() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);
        let area = Rect::new(0, 0, 20, 5);

        let widget = Panel::new(Paragraph::new("body"))
            .borders(Borders::ALL)
            .border_type(BorderType::Ascii)
            .title("Title");

        widget.render(area, &mut frame);

        // Title should appear on the top border row
        let top_row = row_text(&frame, 0, 20);
        assert!(top_row.contains("Title"), "top border should contain title, got: {top_row:?}");
    }

    #[test]
    fn panel_inner_area() {
        let panel = Panel::new(Paragraph::new(""))
            .borders(Borders::ALL);
        let inner = panel.inner(Rect::new(0, 0, 10, 10));
        // Borders take 1 cell each side
        assert_eq!(inner.x, 1);
        assert_eq!(inner.y, 1);
        assert_eq!(inner.width, 8);
        assert_eq!(inner.height, 8);
    }

    #[test]
    fn panel_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Panel::new(Paragraph::new("x"))
            .borders(Borders::ALL)
            .render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Pretty
// ===========================================================================

mod pretty_tests {
    use super::*;
    use ftui_widgets::pretty::Pretty;

    #[test]
    fn pretty_renders_debug_output() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        let area = Rect::new(0, 0, 30, 5);

        let data = vec![1, 2, 3];
        let widget = Pretty::new(&data);
        widget.render(area, &mut frame);

        assert!(has_content(&frame, area), "Pretty should render debug output");
    }

    #[test]
    fn pretty_compact_mode() {
        let data = vec![1, 2, 3];
        let p = Pretty::new(&data).with_compact(true);
        let text = p.formatted_text();
        // Compact mode uses {:?} which is single-line
        assert!(!text.contains('\n'), "compact mode should be single line, got: {text:?}");
    }

    #[test]
    fn pretty_expanded_mode() {
        let data = vec![1, 2, 3];
        let p = Pretty::new(&data).with_compact(false);
        let text = p.formatted_text();
        // Pretty mode uses {:#?} which is multi-line
        assert!(text.contains('\n'), "expanded mode should be multi-line");
    }

    #[test]
    fn pretty_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Pretty::new(&42).render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Rule
// ===========================================================================

mod rule_tests {
    use super::*;
    use ftui_widgets::rule::Rule;

    #[test]
    fn rule_fills_width_with_line_char() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 1, &mut pool);
        let area = Rect::new(0, 0, 10, 1);

        let widget = Rule::new().border_type(BorderType::Ascii);
        widget.render(area, &mut frame);

        // All cells should be the horizontal border char '-'
        for x in 0..10 {
            let ch = frame.buffer.get(x, 0).unwrap().content.as_char();
            assert_eq!(ch, Some('-'), "cell {x} should be '-'");
        }
    }

    #[test]
    fn rule_with_title_contains_text() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 1, &mut pool);
        let area = Rect::new(0, 0, 20, 1);

        let widget = Rule::new()
            .title("Section")
            .border_type(BorderType::Ascii);
        widget.render(area, &mut frame);

        let text = row_text(&frame, 0, 20);
        assert!(text.contains("Section"), "rule should contain title, got: {text:?}");
    }

    #[test]
    fn rule_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Rule::new().render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Tree
// ===========================================================================

mod tree_tests {
    use super::*;
    use ftui_widgets::tree::{Tree, TreeGuides, TreeNode};

    #[test]
    fn tree_renders_root_and_children() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        let area = Rect::new(0, 0, 30, 5);

        let root = TreeNode::new("root")
            .child(TreeNode::new("child1"))
            .child(TreeNode::new("child2"));
        let widget = Tree::new(root).with_guides(TreeGuides::Ascii);

        widget.render(area, &mut frame);

        assert!(has_content(&frame, area), "tree should render content");

        // Look for root label
        let line0 = row_text(&frame, 0, 30);
        assert!(line0.contains("root"), "first line should contain root label, got: {line0:?}");
    }

    #[test]
    fn tree_hide_root() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        let area = Rect::new(0, 0, 30, 5);

        let root = TreeNode::new("root")
            .child(TreeNode::new("child1"));
        let widget = Tree::new(root)
            .with_show_root(false)
            .with_guides(TreeGuides::Ascii);

        widget.render(area, &mut frame);

        let line0 = row_text(&frame, 0, 30);
        assert!(!line0.contains("root"), "root should be hidden, got: {line0:?}");
        assert!(line0.contains("child1"), "child should be visible, got: {line0:?}");
    }

    #[test]
    fn tree_collapsed_node_hides_children() {
        let root = TreeNode::new("root")
            .child(
                TreeNode::new("parent")
                    .with_expanded(false)
                    .child(TreeNode::new("hidden_child")),
            );
        // Collapsed parent: visible_count should not include hidden_child
        let count = root.visible_count();
        // root(1) + parent(1) = 2 (hidden_child not counted)
        assert_eq!(count, 2, "collapsed node should hide children");
    }

    #[test]
    fn tree_node_toggle() {
        let mut node = TreeNode::new("test").with_expanded(true);
        assert!(node.is_expanded());
        node.toggle_expanded();
        assert!(!node.is_expanded());
    }

    #[test]
    fn tree_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Tree::new(TreeNode::new("r"))
            .render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Table
// ===========================================================================

mod table_tests {
    use super::*;
    use ftui_widgets::table::{Row, Table, TableState};

    #[test]
    fn table_renders_rows() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 4, &mut pool);
        let area = Rect::new(0, 0, 20, 4);

        let widget = Table::new(
            [
                Row::new(["Name", "Age"]),
                Row::new(["Alice", "30"]),
                Row::new(["Bob", "25"]),
            ],
            [Constraint::Fixed(10), Constraint::Fixed(10)],
        );

        widget.render(area, &mut frame);

        assert!(has_content(&frame, area), "table should render content");

        let line0 = row_text(&frame, 0, 20);
        assert!(line0.contains("Name"), "first row should contain 'Name', got: {line0:?}");
    }

    #[test]
    fn table_with_header() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);
        let area = Rect::new(0, 0, 20, 5);

        let widget = Table::new(
            [Row::new(["Alice", "30"])],
            [Constraint::Fixed(10), Constraint::Fixed(10)],
        )
        .header(Row::new(["Name", "Age"]));

        widget.render(area, &mut frame);

        let header_row = row_text(&frame, 0, 20);
        assert!(
            header_row.contains("Name"),
            "header row should contain 'Name', got: {header_row:?}"
        );
    }

    #[test]
    fn table_stateful_selection() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 4, &mut pool);
        let area = Rect::new(0, 0, 20, 4);

        let widget = Table::new(
            [
                Row::new(["Alice"]),
                Row::new(["Bob"]),
                Row::new(["Charlie"]),
            ],
            [Constraint::Fixed(20)],
        )
        .highlight_style(Style::new().bold());

        let mut state = TableState::default();
        state.select(Some(1));

        ftui_widgets::StatefulWidget::render(&widget, area, &mut frame, &mut state);

        assert!(has_content(&frame, area), "table with selection should render");
    }

    #[test]
    fn table_zero_area_no_panic() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        Table::new(
            [Row::new(["x"])],
            [Constraint::Fixed(1)],
        )
        .render(Rect::new(0, 0, 0, 0), &mut frame);
    }
}

// ===========================================================================
// Cross-renderable composition
// ===========================================================================

mod composition_tests {
    use super::*;
    use ftui_widgets::padding::Padding;
    use ftui_widgets::panel::Panel;
    use ftui_widgets::borders::Borders;
    use ftui_widgets::paragraph::Paragraph;

    #[test]
    fn panel_containing_padded_paragraph() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 7, &mut pool);
        let area = Rect::new(0, 0, 20, 7);

        let inner = Padding::new(
            Paragraph::new("Hello"),
            Sides::new(1, 0, 0, 1), // top=1, right=0, bottom=0, left=1
        );
        let widget = Panel::new(inner)
            .borders(Borders::ALL)
            .border_type(BorderType::Ascii);

        widget.render(area, &mut frame);

        // Border at (0,0)
        let tl = frame.buffer.get(0, 0).unwrap().content.as_char();
        assert_eq!(tl, Some('+'));

        // Content should be offset by border(1) + padding(left=1, top=1)
        // So "H" at x=2, y=2
        let ch = frame.buffer.get(2, 2).unwrap().content.as_char();
        assert_eq!(ch, Some('H'), "content should be at border+padding offset");
    }

    #[test]
    fn group_of_rule_and_paragraph() {
        use ftui_widgets::group::Group;
        use ftui_widgets::rule::Rule;

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(15, 3, &mut pool);
        let area = Rect::new(0, 0, 15, 3);

        // Group renders both to same area; paragraph overwrites rule at row 0
        let widget = Group::new()
            .push(Rule::new().border_type(BorderType::Ascii))
            .push(Paragraph::new("Text"));

        widget.render(area, &mut frame);

        // Paragraph overwrites at (0,0)
        let c0 = frame.buffer.get(0, 0).unwrap().content.as_char();
        assert_eq!(c0, Some('T'));
    }

    #[test]
    fn columns_of_panels() {
        use ftui_widgets::columns::Columns;

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        let area = Rect::new(0, 0, 30, 5);

        let left = Panel::new(Paragraph::new("L"))
            .borders(Borders::ALL)
            .border_type(BorderType::Ascii);
        let right = Panel::new(Paragraph::new("R"))
            .borders(Borders::ALL)
            .border_type(BorderType::Ascii);

        let widget = Columns::new()
            .column(left, Constraint::Ratio(1, 2))
            .column(right, Constraint::Ratio(1, 2));

        widget.render(area, &mut frame);

        // Both columns should have border corners
        let left_corner = frame.buffer.get(0, 0).unwrap().content.as_char();
        assert_eq!(left_corner, Some('+'), "left panel should have border");

        // Right panel corner should be somewhere in second half
        let mut found_right_corner = false;
        for x in 10..30 {
            if frame.buffer.get(x, 0).unwrap().content.as_char() == Some('+') {
                found_right_corner = true;
                break;
            }
        }
        assert!(found_right_corner, "right panel border should appear in second half");
    }
}

// ===========================================================================
// Zero-area safety: all renderables survive empty rects
// ===========================================================================

#[test]
fn all_renderables_survive_zero_area() {
    use ftui_widgets::align::Align;
    use ftui_widgets::columns::Columns;
    use ftui_widgets::emoji::Emoji;
    use ftui_widgets::group::Group;
    use ftui_widgets::json_view::JsonView;
    use ftui_widgets::padding::Padding;
    use ftui_widgets::panel::Panel;
    use ftui_widgets::pretty::Pretty;
    use ftui_widgets::rule::Rule;
    use ftui_widgets::tree::{Tree, TreeNode};
    use ftui_widgets::paragraph::Paragraph;
    use ftui_widgets::table::{Row, Table};

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    let zero = Rect::new(0, 0, 0, 0);

    Align::new(Paragraph::new("")).render(zero, &mut frame);
    Columns::new().render(zero, &mut frame);
    Emoji::new("").render(zero, &mut frame);
    Group::new().render(zero, &mut frame);
    JsonView::new("{}").render(zero, &mut frame);
    Padding::new(Paragraph::new(""), Sides::new(0, 0, 0, 0)).render(zero, &mut frame);
    Panel::new(Paragraph::new("")).render(zero, &mut frame);
    Pretty::new(&0).render(zero, &mut frame);
    Rule::new().render(zero, &mut frame);
    Tree::new(TreeNode::new("r")).render(zero, &mut frame);
    Table::new([Row::new(["x"])], [Constraint::Fixed(1)]).render(zero, &mut frame);
}
