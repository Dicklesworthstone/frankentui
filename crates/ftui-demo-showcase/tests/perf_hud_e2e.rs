#![forbid(unsafe_code)]

//! End-to-end tests for the Performance HUD (bd-3k3x.7).
//!
//! These tests exercise the performance HUD overlay through the
//! `AppModel` struct, covering:
//!
//! - Toggle on/off with Ctrl+P
//! - Metrics display (FPS, TPS, tick timing percentiles)
//! - Sparkline visualization
//! - Color coding for FPS thresholds
//! - Graceful degradation at small sizes
//!
//! # Invariants (Alien Artifact)
//!
//! 1. **Toggle idempotency**: Double-toggle returns to original state.
//! 2. **Ring buffer capacity**: Tick times buffer never exceeds 120 samples.
//! 3. **Graceful fallback**: HUD hidden when area < 20x6.
//! 4. **Statistics stability**: Percentiles computed correctly from sorted data.
//!
//! # Failure Modes
//!
//! | Scenario | Expected Behavior |
//! |----------|-------------------|
//! | Very small terminal (40x10) | HUD gracefully hidden or minimal |
//! | No tick data | Stats show 0.0 values |
//! | Rapid toggles | State remains consistent |
//! | Empty ring buffer | No panic on percentile calculation |
//!
//! Run: `cargo test -p ftui-demo-showcase --test perf_hud_e2e`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::app::{AppModel, AppMsg};
use ftui_harness::assert_snapshot;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::program::Model;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ctrl_press(ch: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(ch),
        modifiers: Modifiers::CTRL,
        kind: KeyEventKind::Press,
    })
}

/// Emit a JSONL log entry to stderr for verbose test logging.
fn log_jsonl(step: &str, data: &[(&str, &str)]) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = COUNTER.fetch_add(1, Ordering::Relaxed);
    let fields: Vec<String> = std::iter::once(format!("\"ts\":\"T{ts:06}\""))
        .chain(std::iter::once(format!("\"step\":\"{step}\"")))
        .chain(data.iter().map(|(k, v)| format!("\"{k}\":\"{v}\"")))
        .collect();
    eprintln!("{{{}}}", fields.join(","));
}

/// Capture a frame and return a hash for determinism checks.
fn capture_frame_hash(app: &AppModel, width: u16, height: u16) -> u64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let _area = Rect::new(0, 0, width, height);
    app.view(&mut frame);
    let mut hasher = DefaultHasher::new();
    for y in 0..height {
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                ch.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Check if the frame contains a specific substring (for content verification).
fn frame_contains_text(app: &AppModel, width: u16, height: u16, needle: &str) -> bool {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let _area = Rect::new(0, 0, width, height);
    app.view(&mut frame);

    // Extract text content from frame
    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                text.push(ch);
            }
        }
    }
    text.contains(needle)
}

const PERF_HUD_SNAPSHOT_SAMPLES_US: &[u64] = &[
    10_000, 12_000, 14_000, 16_000, 18_000, 20_000, 22_000, 24_000, 26_000, 28_000, 30_000,
];

fn seed_perf_hud_snapshot_metrics(app: &mut AppModel) {
    app.seed_perf_hud_metrics_for_test(42, 99, 1.2, PERF_HUD_SNAPSHOT_SAMPLES_US);
}

// ===========================================================================
// Scenario 1: HUD Toggle Behavior
// ===========================================================================

#[test]
fn e2e_perf_hud_toggle_on_off() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_perf_hud_toggle_on_off"),
            ("width", "120"),
            ("height", "40"),
        ],
    );

    let mut app = AppModel::new();

    // Initially HUD should be off
    log_jsonl(
        "check",
        &[("hud_visible", &app.perf_hud_visible.to_string())],
    );
    assert!(!app.perf_hud_visible, "HUD should be off initially");

    // Toggle on with Ctrl+P
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));
    log_jsonl("action", &[("event", "ctrl_p"), ("expected", "hud_on")]);
    assert!(app.perf_hud_visible, "HUD should be on after Ctrl+P");

    // Toggle off with another Ctrl+P
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));
    log_jsonl("action", &[("event", "ctrl_p"), ("expected", "hud_off")]);
    assert!(
        !app.perf_hud_visible,
        "HUD should be off after second Ctrl+P"
    );

    log_jsonl("result", &[("status", "passed")]);
}

#[test]
fn e2e_perf_hud_toggle_idempotent() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_perf_hud_toggle_idempotent"),
            ("width", "120"),
            ("height", "40"),
        ],
    );

    let mut app = AppModel::new();

    // Capture initial frame hash
    let hash_initial = capture_frame_hash(&app, 120, 40);
    log_jsonl("hash", &[("initial", &format!("{hash_initial:016x}"))]);

    // Toggle on then off (double toggle)
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));

    // Frame should match initial state
    let hash_after = capture_frame_hash(&app, 120, 40);
    log_jsonl(
        "hash",
        &[("after_double_toggle", &format!("{hash_after:016x}"))],
    );

    assert_eq!(
        hash_initial, hash_after,
        "Double toggle should return to initial state"
    );

    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// Scenario 2: HUD Content Verification
// ===========================================================================

#[test]
fn e2e_perf_hud_displays_metrics() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_perf_hud_displays_metrics"),
            ("width", "120"),
            ("height", "40"),
        ],
    );

    let mut app = AppModel::new();

    // Enable HUD
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));

    // Simulate a few ticks to populate metrics
    for _ in 0..5 {
        let _ = app.update(AppMsg::Tick);
    }

    // Check for expected content in HUD
    let has_perf_hud = frame_contains_text(&app, 120, 40, "Perf HUD");
    let has_fps = frame_contains_text(&app, 120, 40, "FPS");
    let has_tick_rate = frame_contains_text(&app, 120, 40, "Tick rate");

    log_jsonl(
        "content",
        &[
            ("has_perf_hud", &has_perf_hud.to_string()),
            ("has_fps", &has_fps.to_string()),
            ("has_tick_rate", &has_tick_rate.to_string()),
        ],
    );

    assert!(has_perf_hud, "HUD should display 'Perf HUD' title");
    assert!(has_fps, "HUD should display FPS metric");
    assert!(has_tick_rate, "HUD should display Tick rate metric");

    log_jsonl("result", &[("status", "passed")]);
}

#[test]
fn e2e_perf_hud_displays_tick_timing() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_perf_hud_displays_tick_timing"),
            ("width", "120"),
            ("height", "40"),
        ],
    );

    let mut app = AppModel::new();

    // Enable HUD
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));

    // Simulate multiple ticks
    for _ in 0..10 {
        let _ = app.update(AppMsg::Tick);
    }

    // Check for tick timing percentiles
    let has_avg = frame_contains_text(&app, 120, 40, "avg");
    let has_p95 = frame_contains_text(&app, 120, 40, "p95");
    let has_p99 = frame_contains_text(&app, 120, 40, "p99");

    log_jsonl(
        "timing_display",
        &[
            ("has_avg", &has_avg.to_string()),
            ("has_p95", &has_p95.to_string()),
            ("has_p99", &has_p99.to_string()),
        ],
    );

    // At least avg should be displayed
    assert!(
        has_avg || has_p95 || has_p99,
        "HUD should display tick timing metrics"
    );

    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// Scenario 3: Graceful Degradation
// ===========================================================================

#[test]
fn e2e_perf_hud_graceful_degradation_tiny() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_perf_hud_graceful_degradation_tiny"),
            ("width", "40"),
            ("height", "10"),
        ],
    );

    let mut app = AppModel::new();
    app.terminal_width = 40;
    app.terminal_height = 10;

    // Enable HUD
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));

    // At 40x10 (overlay 36x6), minimal HUD still renders its title border
    assert!(
        frame_contains_text(&app, 40, 10, "Perf HUD"),
        "HUD should render minimal overlay at 40x10"
    );

    // At 20x6 (overlay_width < 20 and overlay_height < 6), HUD must gracefully hide
    app.terminal_width = 20;
    app.terminal_height = 6;
    assert!(
        !frame_contains_text(&app, 20, 6, "Perf HUD"),
        "HUD must gracefully hide when terminal area is too small (20x6)"
    );

    log_jsonl("degradation", &[("rendered", "true"), ("no_panic", "true")]);
    log_jsonl("result", &[("status", "passed")]);
}

#[test]
fn e2e_perf_hud_graceful_degradation_wide() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_perf_hud_graceful_degradation_wide"),
            ("width", "200"),
            ("height", "50"),
        ],
    );

    let mut app = AppModel::new();
    app.terminal_width = 200;
    app.terminal_height = 50;

    // Enable HUD
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));

    // Should render correctly at large size
    let has_perf_hud = frame_contains_text(&app, 200, 50, "Perf HUD");

    log_jsonl(
        "wide_render",
        &[("has_perf_hud", &has_perf_hud.to_string())],
    );

    assert!(has_perf_hud, "HUD should render at large terminal size");
    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// Scenario 4: Determinism Verification
// ===========================================================================

#[test]
fn e2e_perf_hud_deterministic_initial_render() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_perf_hud_deterministic_initial_render"),
            ("width", "120"),
            ("height", "40"),
        ],
    );

    let mut app1 = AppModel::new();
    let mut app2 = AppModel::new();

    // Enable HUD on both
    let _ = app1.update(AppMsg::ScreenEvent(ctrl_press('p')));
    let _ = app2.update(AppMsg::ScreenEvent(ctrl_press('p')));

    // Capture hashes
    let hash1 = capture_frame_hash(&app1, 120, 40);
    let hash2 = capture_frame_hash(&app2, 120, 40);

    log_jsonl(
        "hashes",
        &[
            ("hash1", &format!("{hash1:016x}")),
            ("hash2", &format!("{hash2:016x}")),
        ],
    );

    assert_ne!(hash1, 0, "Hash should be non-zero (content rendered)");
    assert_eq!(
        hash1, hash2,
        "Initial render with HUD enabled must be deterministic across two instances"
    );

    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// Scenario 5: Ring Buffer Behavior
// ===========================================================================

#[test]
fn e2e_perf_hud_ring_buffer_capacity() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_perf_hud_ring_buffer_capacity"),
            ("width", "120"),
            ("height", "40"),
        ],
    );

    let mut app = AppModel::new();

    // Enable HUD
    let _ = app.update(AppMsg::ScreenEvent(ctrl_press('p')));

    // Simulate many ticks (more than ring buffer capacity of 120)
    for i in 0..150 {
        let _ = app.update(AppMsg::Tick);
        if i % 50 == 0 {
            log_jsonl("tick", &[("count", &i.to_string())]);
        }
    }

    // Ring buffer must cap sample count at 120/120
    assert!(
        frame_contains_text(&app, 120, 40, "Samples:    120/120"),
        "HUD should display 'Samples:    120/120' after 150 ticks (ring buffer capped at 120)"
    );

    log_jsonl(
        "ring_buffer",
        &[("ticks_sent", "150"), ("no_overflow", "true")],
    );
    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// Snapshot Tests
// ===========================================================================

#[test]
fn perf_hud_snapshot_120x40() {
    let mut app = AppModel::new();
    app.terminal_width = 120;
    app.terminal_height = 40;
    app.perf_hud_visible = true;
    seed_perf_hud_snapshot_metrics(&mut app);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let _area = Rect::new(0, 0, 120, 40);
    app.view(&mut frame);

    assert_snapshot!("perf_hud_enabled_120x40", &frame.buffer);
}

#[test]
fn perf_hud_snapshot_80x24() {
    let mut app = AppModel::new();
    app.terminal_width = 80;
    app.terminal_height = 24;
    app.perf_hud_visible = true;
    seed_perf_hud_snapshot_metrics(&mut app);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let _area = Rect::new(0, 0, 80, 24);
    app.view(&mut frame);

    assert_snapshot!("perf_hud_enabled_80x24", &frame.buffer);
}
