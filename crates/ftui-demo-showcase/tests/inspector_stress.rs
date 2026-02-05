//! Stress and Performance Regression Tests for UI Inspector (bd-17h9.4)
//!
//! This module provides stress testing for the `InspectorState` and
//! `InspectorOverlay` components:
//!
//! # Coverage
//! - Widget registration throughput (up to 1000 widgets)
//! - Overlay render latency across frame sizes and inspector modes
//! - Hit-region scanning latency (full-screen grid walk)
//! - Deep and wide widget-tree traversal performance
//! - Deterministic render output (hash stability)
//! - Mode-cycling throughput under load
//!
//! # Invariants
//! - Render latency grows linearly in area (hit regions) and widget count (bounds).
//! - Double-render with identical state produces identical buffer output.
//! - Mode cycling is O(1) regardless of registered widget count.
//!
//! # JSONL Logging
//! Tests emit structured logs for CI analysis:
//! ```json
//! {"test":"stress_register_many_widgets","widget_count":1000,"elapsed_us":123}
//! ```
//!
//! Run with: `cargo test -p ftui-demo-showcase inspector_stress -- --nocapture`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitData, HitId, HitRegion};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::Widget;
use ftui_widgets::inspector::{
    DiagnosticEventKind, InspectorMode, InspectorOverlay, InspectorState, WidgetInfo,
};

// =============================================================================
// Test Utilities
// =============================================================================

/// Emit a JSONL log line for CI consumption.
fn log_jsonl(data: &serde_json::Value) {
    eprintln!("{}", serde_json::to_string(data).unwrap());
}

#[allow(dead_code)]
fn is_coverage_run() -> bool {
    std::env::var("LLVM_PROFILE_FILE").is_ok() || std::env::var("CARGO_LLVM_COV").is_ok()
}

fn coverage_budget_ns(default_ns: u64) -> u64 {
    if is_coverage_run() {
        default_ns.saturating_mul(3)
    } else {
        default_ns
    }
}

/// Check if running in debug build (debug_assertions are enabled).
/// Debug builds run significantly slower, so timing budgets need adjustment.
#[allow(dead_code)]
fn is_debug_build() -> bool {
    cfg!(debug_assertions)
}

/// Hash the visible content of a frame buffer for determinism checks.
fn buffer_hash(frame: &Frame, area: Rect) -> u64 {
    let mut hasher = DefaultHasher::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = frame.buffer.get(x, y) {
                if let Some(ch) = cell.content.as_char() {
                    ch.hash(&mut hasher);
                }
                cell.fg.hash(&mut hasher);
                cell.bg.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Build a flat widget tree of `n` widgets spread across `area`.
fn build_flat_widgets(n: usize, area: Rect) -> Vec<WidgetInfo> {
    let row_h = area.height.max(1);
    (0..n)
        .map(|i| {
            let x = area.x + (i as u16 * 3) % area.width;
            let y = area.y + (i as u16) % row_h;
            let w = 10.min(area.width.saturating_sub(x - area.x));
            let h = 3.min(row_h.saturating_sub(y - area.y));
            WidgetInfo::new(format!("W{i}"), Rect::new(x, y, w, h))
                .with_hit_id(HitId::new(i as u32))
                .with_depth((i % 6) as u8)
        })
        .collect()
}

/// Build a deeply nested widget tree of depth `d`.
fn build_deep_tree(depth: usize, area: Rect) -> WidgetInfo {
    let mut leaf = WidgetInfo::new(format!("L{depth}"), area).with_depth(depth as u8);
    for d in (0..depth).rev() {
        let mut parent = WidgetInfo::new(format!("L{d}"), area).with_depth(d as u8);
        parent.add_child(leaf);
        leaf = parent;
    }
    leaf
}

/// Build a wide widget tree: one parent with `n` children.
fn build_wide_tree(n: usize, area: Rect) -> WidgetInfo {
    let mut root = WidgetInfo::new("Root", area).with_depth(0);
    let child_w = area.width / n.max(1) as u16;
    for i in 0..n {
        let x = area.x + i as u16 * child_w;
        let child = WidgetInfo::new(format!("C{i}"), Rect::new(x, area.y, child_w, area.height))
            .with_hit_id(HitId::new(i as u32))
            .with_depth(1);
        root.add_child(child);
    }
    root
}

/// Populate a frame's hit grid with a grid of regions.
fn populate_hit_grid(frame: &mut Frame, area: Rect, cell_size: u16) {
    let mut id_counter: u32 = 1;
    let mut y = area.y;
    while y < area.bottom() {
        let mut x = area.x;
        while x < area.right() {
            let w = cell_size.min(area.right() - x);
            let h = cell_size.min(area.bottom() - y);
            let region = match id_counter % 5 {
                0 => HitRegion::Content,
                1 => HitRegion::Button,
                2 => HitRegion::Link,
                3 => HitRegion::Scrollbar,
                _ => HitRegion::Border,
            };
            frame.register_hit(
                Rect::new(x, y, w, h),
                HitId::new(id_counter),
                region,
                id_counter as HitData,
            );
            id_counter += 1;
            x += cell_size;
        }
        y += cell_size;
    }
}

/// Compute p50/p95/p99 from a sorted slice of nanos.
fn percentiles(sorted: &[u64]) -> (u64, u64, u64) {
    let n = sorted.len();
    if n == 0 {
        return (0, 0, 0);
    }
    let p50 = sorted[n / 2];
    let p95 = sorted[n * 95 / 100];
    let p99 = sorted[n * 99 / 100];
    (p50, p95, p99)
}

// =============================================================================
// Widget Registration Stress
// =============================================================================

#[test]
fn stress_register_many_widgets() {
    let area = Rect::new(0, 0, 200, 50);
    let widgets = build_flat_widgets(1000, area);

    let mut state = InspectorState::new();
    let start = Instant::now();

    for widget in widgets {
        state.register_widget(widget);
    }

    let elapsed = start.elapsed();

    log_jsonl(&serde_json::json!({
        "test": "stress_register_many_widgets",
        "widget_count": 1000,
        "elapsed_us": elapsed.as_micros(),
        "avg_register_ns": elapsed.as_nanos() / 1000,
    }));

    assert_eq!(state.widgets.len(), 1000);

    // Budget: registering 1000 widgets should complete in < 10ms
    assert!(
        elapsed.as_millis() < 10,
        "Widget registration exceeded budget: {:?}",
        elapsed
    );
}

#[test]
fn stress_register_then_clear_cycle() {
    let area = Rect::new(0, 0, 120, 40);
    let mut state = InspectorState::new();

    let start = Instant::now();
    for cycle in 0..50 {
        let widgets = build_flat_widgets(200, area);
        for widget in widgets {
            state.register_widget(widget);
        }
        assert_eq!(
            state.widgets.len(),
            200,
            "cycle {cycle}: registration count"
        );
        state.clear_widgets();
        assert!(state.widgets.is_empty(), "cycle {cycle}: clear failed");
    }
    let elapsed = start.elapsed();

    log_jsonl(&serde_json::json!({
        "test": "stress_register_then_clear_cycle",
        "cycles": 50,
        "widgets_per_cycle": 200,
        "total_elapsed_us": elapsed.as_micros(),
    }));

    // Budget: 50 register-clear cycles should complete in < 100ms
    assert!(
        elapsed.as_millis() < 100,
        "Register/clear cycle exceeded budget: {:?}",
        elapsed
    );
}

// =============================================================================
// Overlay Render Latency
// =============================================================================

#[test]
fn stress_overlay_render_hit_regions_120x40() {
    let area = Rect::new(0, 0, 120, 40);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::HitRegions;

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();
    let mut render_times = Vec::with_capacity(50);

    for _ in 0..50 {
        let mut frame = Frame::with_hit_grid(120, 40, &mut pool);
        populate_hit_grid(&mut frame, area, 5);

        let start = Instant::now();
        overlay.render(area, &mut frame);
        render_times.push(start.elapsed().as_nanos() as u64);
    }

    render_times.sort();
    let avg_ns = render_times.iter().sum::<u64>() / render_times.len() as u64;
    let (p50, p95, p99) = percentiles(&render_times);

    log_jsonl(&serde_json::json!({
        "test": "stress_overlay_render_hit_regions_120x40",
        "render_count": 50,
        "avg_ns": avg_ns,
        "p50_ns": p50,
        "p95_ns": p95,
        "p99_ns": p99,
    }));

    let budget_ns = coverage_budget_ns(5_000_000);
    // Budget: hit-region overlay on 120×40 should be < 5ms (3× under coverage)
    assert!(
        avg_ns < budget_ns,
        "Hit region render exceeded budget ({budget_ns}ns): avg={}ns",
        avg_ns,
    );
}

#[test]
fn stress_overlay_render_widget_bounds_many_widgets() {
    let area = Rect::new(0, 0, 120, 40);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::WidgetBounds;
    state.show_names = true;

    // Register 500 widgets
    for widget in build_flat_widgets(500, area) {
        state.register_widget(widget);
    }

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();
    let mut render_times = Vec::with_capacity(50);

    for _ in 0..50 {
        let mut frame = Frame::with_hit_grid(120, 40, &mut pool);
        let start = Instant::now();
        overlay.render(area, &mut frame);
        render_times.push(start.elapsed().as_nanos() as u64);
    }

    render_times.sort();
    let avg_ns = render_times.iter().sum::<u64>() / render_times.len() as u64;
    let (p50, p95, p99) = percentiles(&render_times);

    log_jsonl(&serde_json::json!({
        "test": "stress_overlay_render_widget_bounds_500",
        "widget_count": 500,
        "render_count": 50,
        "avg_ns": avg_ns,
        "p50_ns": p50,
        "p95_ns": p95,
        "p99_ns": p99,
    }));

    let budget_ns = if is_coverage_run() {
        20_000_000
    } else {
        5_000_000
    };
    // Budget: widget bounds with 500 widgets should be < 5ms (4× under coverage)
    assert!(
        avg_ns < budget_ns,
        "Widget bounds render exceeded budget ({budget_ns}ns): avg={}ns",
        avg_ns,
    );
}

#[test]
fn stress_overlay_render_full_mode_200x50() {
    let area = Rect::new(0, 0, 200, 50);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::Full;
    state.show_detail_panel = true;
    state.show_names = true;
    state.set_hover(Some((50, 25)));

    // Register 300 widgets
    for widget in build_flat_widgets(300, area) {
        state.register_widget(widget);
    }

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();
    let mut render_times = Vec::with_capacity(50);

    for _ in 0..50 {
        let mut frame = Frame::with_hit_grid(200, 50, &mut pool);
        populate_hit_grid(&mut frame, area, 10);

        let start = Instant::now();
        overlay.render(area, &mut frame);
        render_times.push(start.elapsed().as_nanos() as u64);
    }

    render_times.sort();
    let avg_ns = render_times.iter().sum::<u64>() / render_times.len() as u64;
    let (p50, p95, p99) = percentiles(&render_times);

    log_jsonl(&serde_json::json!({
        "test": "stress_overlay_render_full_mode_200x50",
        "widget_count": 300,
        "frame_size": "200x50",
        "render_count": 50,
        "avg_ns": avg_ns,
        "p50_ns": p50,
        "p95_ns": p95,
        "p99_ns": p99,
    }));

    let budget_ns = coverage_budget_ns(10_000_000);
    // Budget: full-mode overlay on 200×50 with 300 widgets should be < 10ms (3× under coverage)
    assert!(
        avg_ns < budget_ns,
        "Full-mode render exceeded budget ({budget_ns}ns): avg={}ns",
        avg_ns,
    );
}

// =============================================================================
// Deep Widget Tree Stress
// =============================================================================

#[test]
fn stress_deep_widget_tree_render() {
    let area = Rect::new(0, 0, 120, 40);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::WidgetBounds;
    state.show_names = true;

    // Build a tree 50 levels deep
    let deep_tree = build_deep_tree(50, area);
    state.register_widget(deep_tree);

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();
    let mut render_times = Vec::with_capacity(50);

    for _ in 0..50 {
        let mut frame = Frame::with_hit_grid(120, 40, &mut pool);
        let start = Instant::now();
        overlay.render(area, &mut frame);
        render_times.push(start.elapsed().as_nanos() as u64);
    }

    render_times.sort();
    let avg_ns = render_times.iter().sum::<u64>() / render_times.len() as u64;
    let (p50, p95, p99) = percentiles(&render_times);

    log_jsonl(&serde_json::json!({
        "test": "stress_deep_widget_tree_render",
        "tree_depth": 50,
        "render_count": 50,
        "avg_ns": avg_ns,
        "p50_ns": p50,
        "p95_ns": p95,
        "p99_ns": p99,
    }));

    let budget_ns = coverage_budget_ns(3_000_000);
    // Budget: rendering 50-deep tree should be < 3ms (3× under coverage)
    assert!(
        avg_ns < budget_ns,
        "Deep tree render exceeded budget ({budget_ns}ns): avg={}ns",
        avg_ns,
    );
}

// =============================================================================
// Wide Widget Tree Stress
// =============================================================================

#[test]
fn stress_wide_widget_tree_render() {
    let area = Rect::new(0, 0, 200, 50);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::WidgetBounds;
    state.show_names = true;

    // Build a tree with 500 children
    let wide_tree = build_wide_tree(500, area);
    state.register_widget(wide_tree);

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();
    let mut render_times = Vec::with_capacity(50);

    for _ in 0..50 {
        let mut frame = Frame::with_hit_grid(200, 50, &mut pool);
        let start = Instant::now();
        overlay.render(area, &mut frame);
        render_times.push(start.elapsed().as_nanos() as u64);
    }

    render_times.sort();
    let avg_ns = render_times.iter().sum::<u64>() / render_times.len() as u64;
    let (p50, p95, p99) = percentiles(&render_times);

    log_jsonl(&serde_json::json!({
        "test": "stress_wide_widget_tree_render",
        "child_count": 500,
        "render_count": 50,
        "avg_ns": avg_ns,
        "p50_ns": p50,
        "p95_ns": p95,
        "p99_ns": p99,
    }));

    // Budget: rendering 500-child tree should be < 5ms
    assert!(
        avg_ns < 5_000_000,
        "Wide tree render exceeded budget: avg={}ns",
        avg_ns
    );
}

// =============================================================================
// Mode Cycling Under Load
// =============================================================================

#[test]
fn stress_mode_cycling_with_many_widgets() {
    let area = Rect::new(0, 0, 120, 40);
    let mut state = InspectorState::new();

    // Register 500 widgets
    for widget in build_flat_widgets(500, area) {
        state.register_widget(widget);
    }

    let cycle_count = if is_coverage_run() { 200 } else { 1000 };
    let start = Instant::now();
    for _ in 0..cycle_count {
        state.mode = state.mode.cycle();
    }
    let elapsed = start.elapsed();

    log_jsonl(&serde_json::json!({
        "test": "stress_mode_cycling_with_many_widgets",
        "widget_count": 500,
        "cycle_count": cycle_count,
        "elapsed_us": elapsed.as_micros(),
    }));

    // Mode cycling is O(1) — state change only, doesn't touch widgets
    assert!(
        elapsed.as_micros() < 1_000,
        "Mode cycling exceeded budget: {:?}",
        elapsed
    );

    // After a multiple of 4 cycles, should be back at Off.
    assert_eq!(state.mode, InspectorMode::Off);
}

// =============================================================================
// Toggle Throughput Under Load
// =============================================================================

#[test]
fn stress_toggle_flags_with_diagnostics() {
    let mut state = InspectorState::new().with_diagnostics();

    // Register widgets so diagnostic entries include widget_count
    let area = Rect::new(0, 0, 100, 30);
    for widget in build_flat_widgets(200, area) {
        state.register_widget(widget);
    }

    let cycles = if is_coverage_run() { 100 } else { 500 };
    let start = Instant::now();
    for _ in 0..cycles {
        state.toggle_hits();
        state.toggle_bounds();
        state.toggle_names();
        state.toggle_times();
        state.toggle_detail_panel();
    }
    let elapsed = start.elapsed();

    let log = state
        .diagnostic_log()
        .expect("diagnostics should be enabled");
    let toggle_events = log.entries_of_kind(DiagnosticEventKind::HitsToggled).len()
        + log
            .entries_of_kind(DiagnosticEventKind::BoundsToggled)
            .len()
        + log.entries_of_kind(DiagnosticEventKind::NamesToggled).len()
        + log.entries_of_kind(DiagnosticEventKind::TimesToggled).len()
        + log
            .entries_of_kind(DiagnosticEventKind::DetailPanelToggled)
            .len();

    log_jsonl(&serde_json::json!({
        "test": "stress_toggle_flags_with_diagnostics",
        "toggle_count": cycles * 5,
        "diagnostic_events": toggle_events,
        "elapsed_us": elapsed.as_micros(),
    }));

    // Budget: toggle burst with diagnostic recording should be < 50ms
    assert!(
        elapsed.as_millis() < 50,
        "Flag toggle throughput exceeded budget: {:?}",
        elapsed
    );
}

// =============================================================================
// Deterministic Rendering
// =============================================================================

#[test]
fn determinism_same_state_same_output() {
    let area = Rect::new(0, 0, 80, 24);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::Full;
    state.show_names = true;
    state.show_detail_panel = true;
    state.set_hover(Some((20, 10)));

    // Register widgets
    for widget in build_flat_widgets(50, area) {
        state.register_widget(widget);
    }

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();

    // Render twice and compare hashes
    let mut frame1 = Frame::with_hit_grid(80, 24, &mut pool);
    populate_hit_grid(&mut frame1, area, 8);
    overlay.render(area, &mut frame1);
    let hash1 = buffer_hash(&frame1, area);

    let mut frame2 = Frame::with_hit_grid(80, 24, &mut pool);
    populate_hit_grid(&mut frame2, area, 8);
    overlay.render(area, &mut frame2);
    let hash2 = buffer_hash(&frame2, area);

    log_jsonl(&serde_json::json!({
        "test": "determinism_same_state_same_output",
        "hash1": hash1,
        "hash2": hash2,
        "match": hash1 == hash2,
    }));

    assert_eq!(
        hash1, hash2,
        "Identical inspector state should produce identical output"
    );
}

#[test]
fn determinism_different_modes_different_output() {
    let area = Rect::new(0, 0, 80, 24);

    // Collect hashes for each active mode
    let mut mode_hashes = Vec::new();
    for mode in [
        InspectorMode::HitRegions,
        InspectorMode::WidgetBounds,
        InspectorMode::Full,
    ] {
        let mut state = InspectorState::new();
        state.mode = mode;
        state.show_names = true;

        for widget in build_flat_widgets(20, area) {
            state.register_widget(widget);
        }

        let overlay = InspectorOverlay::new(&state);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(80, 24, &mut pool);
        populate_hit_grid(&mut frame, area, 10);
        overlay.render(area, &mut frame);

        mode_hashes.push((format!("{mode:?}"), buffer_hash(&frame, area)));
    }

    log_jsonl(&serde_json::json!({
        "test": "determinism_different_modes_different_output",
        "mode_hashes": mode_hashes.iter()
            .map(|(m, h)| serde_json::json!({"mode": m, "hash": h}))
            .collect::<Vec<_>>(),
    }));

    // At least some modes should produce different output
    let distinct = mode_hashes
        .iter()
        .map(|(_, h)| h)
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert!(
        distinct >= 2,
        "Expected at least 2 distinct outputs across modes, got {}",
        distinct
    );
}

// =============================================================================
// Hit Region Scan Latency (Large Grids)
// =============================================================================

#[test]
fn stress_hit_region_scan_large_grid() {
    let area = Rect::new(0, 0, 200, 50);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::HitRegions;
    state.set_hover(Some((100, 25)));
    state.select(Some(HitId::new(42)));

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();
    let mut render_times = Vec::with_capacity(30);

    for _ in 0..30 {
        let mut frame = Frame::with_hit_grid(200, 50, &mut pool);
        // Dense hit grid: every 3×3 block is a region
        populate_hit_grid(&mut frame, area, 3);

        let start = Instant::now();
        overlay.render(area, &mut frame);
        render_times.push(start.elapsed().as_nanos() as u64);
    }

    render_times.sort();
    let avg_ns = render_times.iter().sum::<u64>() / render_times.len() as u64;
    let (p50, p95, p99) = percentiles(&render_times);

    log_jsonl(&serde_json::json!({
        "test": "stress_hit_region_scan_large_grid",
        "grid_size": "200x50",
        "cell_size": 3,
        "render_count": 30,
        "avg_ns": avg_ns,
        "p50_ns": p50,
        "p95_ns": p95,
        "p99_ns": p99,
    }));

    let budget_ns = coverage_budget_ns(10_000_000);
    // Budget: scanning 200×50 = 10,000 cells should be < 10ms (3× under coverage)
    assert!(
        avg_ns < budget_ns,
        "Hit region scan exceeded budget ({budget_ns}ns): avg={}ns",
        avg_ns,
    );
}

// =============================================================================
// Combined Stress (Worst-Case Scenario)
// =============================================================================

#[test]
fn stress_combined_worst_case() {
    let area = Rect::new(0, 0, 200, 50);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::Full;
    state.show_detail_panel = true;
    state.show_names = true;
    state.set_hover(Some((100, 25)));
    state.select(Some(HitId::new(1)));

    // 500 flat widgets + deep nested tree
    for widget in build_flat_widgets(500, area) {
        state.register_widget(widget);
    }
    state.register_widget(build_deep_tree(30, Rect::new(10, 5, 180, 40)));

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();
    let mut render_times = Vec::with_capacity(30);

    for _ in 0..30 {
        let mut frame = Frame::with_hit_grid(200, 50, &mut pool);
        populate_hit_grid(&mut frame, area, 5);

        let start = Instant::now();
        overlay.render(area, &mut frame);
        render_times.push(start.elapsed().as_nanos() as u64);
    }

    render_times.sort();
    let avg_ns = render_times.iter().sum::<u64>() / render_times.len() as u64;
    let (p50, p95, p99) = percentiles(&render_times);
    let max_ns = render_times.last().copied().unwrap_or(0);

    log_jsonl(&serde_json::json!({
        "test": "stress_combined_worst_case",
        "widget_count": 531,
        "tree_depth": 30,
        "grid_size": "200x50",
        "render_count": 30,
        "avg_ns": avg_ns,
        "p50_ns": p50,
        "p95_ns": p95,
        "p99_ns": p99,
        "max_ns": max_ns,
    }));

    let budget_ns = coverage_budget_ns(15_000_000);
    // Budget: worst-case should still be < 15ms (3× under coverage)
    assert!(
        avg_ns < budget_ns,
        "Combined worst-case render exceeded budget ({budget_ns}ns): avg={}ns",
        avg_ns,
    );
}

// =============================================================================
// Hover Throughput
// =============================================================================

#[test]
fn stress_hover_position_updates() {
    let mut state = InspectorState::new();
    state.mode = InspectorMode::Full;

    // Register widgets so hover has context
    let area = Rect::new(0, 0, 120, 40);
    for widget in build_flat_widgets(100, area) {
        state.register_widget(widget);
    }

    let update_count: u16 = if is_coverage_run() { 2000 } else { 10_000 };
    let start = Instant::now();
    for i in 0..update_count {
        state.set_hover(Some((i % 120, i % 40)));
    }
    let elapsed = start.elapsed();

    log_jsonl(&serde_json::json!({
        "test": "stress_hover_position_updates",
        "update_count": update_count,
        "elapsed_us": elapsed.as_micros(),
        "avg_ns": elapsed.as_nanos() / update_count as u128,
    }));

    let budget_ns = coverage_budget_ns(5_000_000);
    let avg_ns = elapsed.as_nanos() / update_count as u128;
    // Budget: hover updates should be < 5ms total (scaled for coverage)
    assert!(
        avg_ns < budget_ns as u128,
        "Hover update throughput exceeded budget ({budget_ns}ns): avg={}ns",
        avg_ns
    );
}

// =============================================================================
// Tiny Frame Edge Case
// =============================================================================

#[test]
fn stress_render_tiny_frame_with_many_widgets() {
    // Many widgets registered but rendered into a tiny area
    let area = Rect::new(0, 0, 10, 5);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::Full;
    state.show_detail_panel = true;
    state.show_names = true;

    // Register 1000 widgets (most will be outside the tiny frame)
    let big_area = Rect::new(0, 0, 200, 50);
    for widget in build_flat_widgets(1000, big_area) {
        state.register_widget(widget);
    }

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();

    // Should not panic and should complete quickly
    let render_count = if is_coverage_run() { 10 } else { 50 };
    let start = Instant::now();
    for _ in 0..render_count {
        let mut frame = Frame::with_hit_grid(10, 5, &mut pool);
        overlay.render(area, &mut frame);
    }
    let elapsed = start.elapsed();

    log_jsonl(&serde_json::json!({
        "test": "stress_render_tiny_frame_with_many_widgets",
        "widget_count": 1000,
        "frame_size": "10x5",
        "render_count": render_count,
        "elapsed_us": elapsed.as_micros(),
    }));

    // Budget: tiny frame should render fast regardless of widget count
    // Debug builds run ~7x slower than release, so adjust budget accordingly.
    let budget_ms = if is_debug_build() { 350 } else { 50 };
    assert!(
        elapsed.as_millis() < budget_ms,
        "Tiny frame render exceeded budget ({budget_ms}ms): {:?}",
        elapsed
    );
}

// =============================================================================
// Regression: Off Mode is Zero-Cost
// =============================================================================

#[test]
fn regression_off_mode_is_zero_cost() {
    let area = Rect::new(0, 0, 200, 50);
    let mut state = InspectorState::new();
    state.mode = InspectorMode::Off;

    // Register many widgets (should not matter in Off mode)
    for widget in build_flat_widgets(1000, area) {
        state.register_widget(widget);
    }

    let overlay = InspectorOverlay::new(&state);
    let mut pool = GraphemePool::new();

    // Pre-allocate frame with hit grid (frame creation cost is NOT what we test)
    let mut frame = Frame::with_hit_grid(200, 50, &mut pool);
    populate_hit_grid(&mut frame, area, 5);

    // Measure ONLY the overlay render calls (should be pure is_active() → return)
    let render_count = if is_coverage_run() { 2000 } else { 10_000 };
    let start = Instant::now();
    for _ in 0..render_count {
        overlay.render(area, &mut frame);
    }
    let elapsed = start.elapsed();

    log_jsonl(&serde_json::json!({
        "test": "regression_off_mode_is_zero_cost",
        "widget_count": 1000,
        "render_count": render_count,
        "elapsed_us": elapsed.as_micros(),
        "avg_ns": elapsed.as_nanos() / render_count as u128,
    }));

    // Off mode should be < 5ms for a large render batch (just the is_active() check)
    assert!(
        elapsed.as_millis() < 5,
        "Off mode render not zero-cost: {:?}",
        elapsed
    );
}

// =============================================================================
// Selection Stress
// =============================================================================

#[test]
fn stress_rapid_selection_changes() {
    let mut state = InspectorState::new();
    state.mode = InspectorMode::Full;

    let selection_count: u32 = if is_coverage_run() { 2000 } else { 10_000 };
    let start = Instant::now();
    for i in 0..selection_count {
        state.select(Some(HitId::new(i)));
    }
    state.clear_selection();
    let elapsed = start.elapsed();

    log_jsonl(&serde_json::json!({
        "test": "stress_rapid_selection_changes",
        "selection_count": selection_count,
        "elapsed_us": elapsed.as_micros(),
    }));

    assert!(state.selected.is_none());

    // Budget: selection throughput should be < 5ms
    assert!(
        elapsed.as_millis() < 5,
        "Selection throughput exceeded budget: {:?}",
        elapsed
    );
}

// =============================================================================
// Multi-Size Render Matrix
// =============================================================================

#[test]
fn stress_render_matrix_sizes_and_modes() {
    let sizes: &[(u16, u16)] = &[(40, 10), (80, 24), (120, 40), (200, 50)];
    let modes = [
        InspectorMode::HitRegions,
        InspectorMode::WidgetBounds,
        InspectorMode::Full,
    ];

    let mut results = Vec::new();

    for &(w, h) in sizes {
        for mode in &modes {
            let area = Rect::new(0, 0, w, h);
            let mut state = InspectorState::new();
            state.mode = *mode;
            state.show_names = true;

            for widget in build_flat_widgets(100, area) {
                state.register_widget(widget);
            }

            let overlay = InspectorOverlay::new(&state);
            let mut pool = GraphemePool::new();
            let mut render_times = Vec::with_capacity(20);

            for _ in 0..20 {
                let mut frame = Frame::with_hit_grid(w, h, &mut pool);
                populate_hit_grid(&mut frame, area, 8);

                let start = Instant::now();
                overlay.render(area, &mut frame);
                render_times.push(start.elapsed().as_nanos() as u64);
            }

            render_times.sort();
            let avg_ns = render_times.iter().sum::<u64>() / render_times.len() as u64;
            let (p50, p95, _p99) = percentiles(&render_times);

            results.push(serde_json::json!({
                "size": format!("{w}x{h}"),
                "mode": format!("{mode:?}"),
                "avg_ns": avg_ns,
                "p50_ns": p50,
                "p95_ns": p95,
            }));

            // Per-case budget: no single case should exceed 15ms
            assert!(
                avg_ns < 15_000_000,
                "Render {w}x{h} {:?} exceeded budget: avg={}ns",
                mode,
                avg_ns
            );
        }
    }

    log_jsonl(&serde_json::json!({
        "test": "stress_render_matrix_sizes_and_modes",
        "results": results,
    }));
}
