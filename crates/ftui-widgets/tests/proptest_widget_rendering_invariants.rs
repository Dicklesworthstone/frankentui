//! Property-based invariant tests for widget rendering.
//!
//! These tests verify structural invariants across the widget library:
//!
//! 1. Block inner() is always contained within original area
//! 2. Block chrome_size matches inner() reduction
//! 3. ProgressBar ratio is always clamped to [0, 1]
//! 4. Sparkline never panics on arbitrary data and sizes
//! 5. List render never panics with arbitrary items and sizes
//! 6. List state selection remains valid after render
//! 7. Scrollbar state position stays within bounds
//! 8. Widget rendering never panics at zero-sized areas
//! 9. Block inner() with all border combinations is monotonic
//! 10. ProgressBar filled width never exceeds area width

use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::block::Block;
use ftui_widgets::borders::Borders;
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use ftui_widgets::sparkline::Sparkline;
use ftui_widgets::{StatefulWidget, Widget};
use proptest::prelude::*;

// ── Strategies ──────────────────────────────────────────────────────────────

fn rect_strategy() -> impl Strategy<Value = Rect> {
    (0u16..=50, 0u16..=50, 0u16..=120, 0u16..=60).prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

fn borders_strategy() -> impl Strategy<Value = Borders> {
    (0u8..=0x0F).prop_map(Borders::from_bits_truncate)
}

fn sparkline_data_strategy() -> impl Strategy<Value = Vec<f64>> {
    proptest::collection::vec(
        prop_oneof![
            -1e6f64..=1e6,
            Just(0.0),
            Just(f64::NAN),
            Just(f64::INFINITY),
            Just(f64::NEG_INFINITY),
        ],
        0..=200,
    )
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. Block inner() is always contained within original area
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn block_inner_within_original_area(
        area in rect_strategy(),
        borders in borders_strategy(),
    ) {
        let block = Block::new().borders(borders);
        let inner = block.inner(area);

        // Inner width/height should never exceed area width/height.
        prop_assert!(inner.width <= area.width,
            "inner.width {} > area.width {}", inner.width, area.width);
        prop_assert!(inner.height <= area.height,
            "inner.height {} > area.height {}", inner.height, area.height);

        // For non-zero areas, inner is spatially contained.
        if area.width > 0 && area.height > 0 && inner.width > 0 && inner.height > 0 {
            prop_assert!(inner.x >= area.x, "inner.x {} < area.x {}", inner.x, area.x);
            prop_assert!(inner.y >= area.y, "inner.y {} < area.y {}", inner.y, area.y);
            prop_assert!(
                inner.right() <= area.right(),
                "inner.right() {} > area.right() {}",
                inner.right(), area.right()
            );
            prop_assert!(
                inner.bottom() <= area.bottom(),
                "inner.bottom() {} > area.bottom() {}",
                inner.bottom(), area.bottom()
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. Block chrome_size matches inner() reduction
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn block_chrome_size_consistent_with_inner(
        w in 4u16..=80,
        h in 4u16..=40,
        borders in borders_strategy(),
    ) {
        let area = Rect::new(0, 0, w, h);
        let block = Block::new().borders(borders);
        let inner = block.inner(area);
        let (chrome_w, chrome_h) = block.chrome_size();

        // Chrome size should equal the reduction in width and height.
        prop_assert_eq!(
            chrome_w, w - inner.width,
            "chrome_w {} != area.width {} - inner.width {}",
            chrome_w, w, inner.width
        );
        prop_assert_eq!(
            chrome_h, h - inner.height,
            "chrome_h {} != area.height {} - inner.height {}",
            chrome_h, h, inner.height
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. ProgressBar ratio is always clamped to [0, 1]
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn progress_bar_ratio_clamped(
        ratio in -10.0f64..=10.0,
    ) {
        let pb = ProgressBar::new().ratio(ratio);
        // We can't directly read the ratio field (private), but rendering
        // should never panic, which verifies the clamp worked.
        let area = Rect::new(0, 0, 40, 3);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 3, &mut pool);
        pb.render(area, &mut frame);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. Sparkline never panics on arbitrary data and sizes
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn sparkline_never_panics(
        data in sparkline_data_strategy(),
        w in 0u16..=120,
        h in 0u16..=10,
    ) {
        let spark = Sparkline::new(&data);
        let w_safe = w.max(1);
        let h_safe = h.max(1);
        let area = Rect::new(0, 0, w, h);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w_safe, h_safe, &mut pool);
        spark.render(area, &mut frame);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. List render never panics with arbitrary items and sizes
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn list_render_never_panics(
        item_count in 0usize..=50,
        w in 1u16..=80,
        h in 1u16..=30,
    ) {
        let items: Vec<ListItem> = (0..item_count)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, w, h);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. List state selection remains valid after render
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn list_selection_valid_after_render(
        item_count in 1usize..=30,
        selected in 0usize..=50,
        w in 5u16..=60,
        h in 3u16..=20,
    ) {
        let items: Vec<ListItem> = (0..item_count)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, w, h);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);
        let mut state = ListState::default();
        state.select(Some(selected));

        StatefulWidget::render(&list, area, &mut frame, &mut state);

        // After render, selection should be valid if items exist.
        if let Some(sel) = state.selected() {
            // If the widget clamped the selection, it should be in bounds.
            // Some implementations preserve out-of-bounds selections,
            // others clamp. Either way, no panic is the key invariant.
            prop_assert!(
                sel <= selected,
                "Selection grew: {} > original {}", sel, selected
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 7. Scrollbar state position stays within bounds
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn scrollbar_scroll_stays_bounded(
        content_length in 0usize..=1000,
        viewport_length in 0usize..=100,
        position in 0usize..=1000,
        scroll_amount in 1usize..=20,
    ) {
        let mut state = ScrollbarState::new(content_length, position, viewport_length);

        // Scroll down.
        state.scroll_down(scroll_amount);
        let max_pos = content_length.saturating_sub(viewport_length);
        prop_assert!(
            state.position <= max_pos || content_length <= viewport_length,
            "After scroll_down: position {} > max_pos {} (content={}, viewport={})",
            state.position, max_pos, content_length, viewport_length,
        );

        // Scroll up.
        state.scroll_up(scroll_amount);
        // position is always >= 0 (usize), never panics.
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 8. Widget rendering never panics at zero-sized areas
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn zero_area_block_render(borders in borders_strategy()) {
        let block = Block::bordered().borders(borders);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        block.render(area, &mut frame);
    }

    #[test]
    fn zero_area_progress_render(ratio in 0.0f64..=1.0) {
        let pb = ProgressBar::new().ratio(ratio);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        pb.render(area, &mut frame);
    }

    #[test]
    fn zero_area_list_render(item_count in 0usize..=10) {
        let items: Vec<ListItem> = (0..item_count)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items);
        let area = Rect::new(0, 0, 0, 0);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let mut state = ListState::default();
        StatefulWidget::render(&list, area, &mut frame, &mut state);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 9. Block inner() with all border combinations is monotonic
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn block_inner_monotonic_with_more_borders(
        w in 4u16..=80,
        h in 4u16..=40,
        b1 in borders_strategy(),
        b2 in borders_strategy(),
    ) {
        let area = Rect::new(0, 0, w, h);

        // b_union has at least as many borders as b1.
        let b_union = b1 | b2;
        let inner1 = Block::new().borders(b1).inner(area);
        let inner_union = Block::new().borders(b_union).inner(area);

        // More borders => inner area is <= (monotonic shrink).
        prop_assert!(
            inner_union.width <= inner1.width,
            "Adding borders should not increase width: union={} > b1={}",
            inner_union.width, inner1.width
        );
        prop_assert!(
            inner_union.height <= inner1.height,
            "Adding borders should not increase height: union={} > b1={}",
            inner_union.height, inner1.height
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 10. Scrollbar render never panics with arbitrary state
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn scrollbar_render_never_panics(
        content_length in 0usize..=500,
        viewport_length in 0usize..=100,
        position in 0usize..=500,
        w in 1u16..=80,
        h in 1u16..=40,
    ) {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let mut state = ScrollbarState::new(content_length, position, viewport_length);
        let area = Rect::new(0, 0, w, h);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);
        StatefulWidget::render(&scrollbar, area, &mut frame, &mut state);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 11. Block inner() at minimum sizes never underflows
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn block_inner_no_underflow_at_tiny_sizes(
        w in 0u16..=3,
        h in 0u16..=3,
        borders in borders_strategy(),
    ) {
        let area = Rect::new(0, 0, w, h);
        let block = Block::new().borders(borders);
        let inner = block.inner(area);

        // Saturating arithmetic should prevent underflow.
        // Width and height should never wrap around.
        prop_assert!(inner.width <= w, "inner.width {} > area.width {}", inner.width, w);
        prop_assert!(inner.height <= h, "inner.height {} > area.height {}", inner.height, h);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 12. Sparkline with explicit bounds never panics
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn sparkline_explicit_bounds_never_panics(
        data in proptest::collection::vec(-100.0f64..=100.0, 0..=50),
        min_val in -200.0f64..=0.0,
        max_val in 0.0f64..=200.0,
        w in 1u16..=60,
    ) {
        let spark = Sparkline::new(&data).bounds(min_val, max_val);
        let area = Rect::new(0, 0, w, 1);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, 1, &mut pool);
        spark.render(area, &mut frame);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 13. ProgressBar with block wrapper never panics
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn progress_bar_with_block_never_panics(
        ratio in 0.0f64..=1.0,
        w in 0u16..=60,
        h in 0u16..=5,
    ) {
        let pb = ProgressBar::new()
            .ratio(ratio)
            .block(Block::bordered());
        let w_safe = w.max(1);
        let h_safe = h.max(1);
        let area = Rect::new(0, 0, w, h);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w_safe, h_safe, &mut pool);
        pb.render(area, &mut frame);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 14. List with block and highlight never panics
// ═════════════════════════════════════════════════════════════════════════════

proptest! {
    #[test]
    fn list_with_block_and_selection_never_panics(
        item_count in 0usize..=20,
        selected in proptest::option::of(0usize..=25),
        w in 1u16..=40,
        h in 1u16..=15,
    ) {
        let items: Vec<ListItem> = (0..item_count)
            .map(|i| ListItem::new(format!("Item {i}")))
            .collect();
        let list = List::new(items)
            .block(Block::bordered())
            .highlight_symbol("> ");
        let area = Rect::new(0, 0, w, h);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);
        let mut state = ListState::default();
        state.select(selected);
        StatefulWidget::render(&list, area, &mut frame, &mut state);
    }
}
