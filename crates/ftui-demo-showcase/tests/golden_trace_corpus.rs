//! Golden trace corpus for the demo showcase (bd-lff4p.5.3).
//!
//! Each test records a scripted session through the demo showcase AppModel,
//! then replays it to verify frame checksums are deterministic.
//!
//! The corpus is intentionally small (< 20 frames per trace) so it runs in CI
//! under a second, while covering the key rendering surfaces:
//!
//! - **dense_dashboard**: Dashboard with many live widgets at 80x24.
//! - **screen_navigation**: Tab through multiple screens at 120x40.
//! - **resize_storm**: Rapid terminal resize events.
//! - **mouse_interaction**: Mouse click and move events on the dashboard.
//! - **tick_animation**: Multiple ticks advancing animated content.
//!
//! Run with:
//!   cargo test -p ftui-demo-showcase --test golden_trace_corpus

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_demo_showcase::app::AppModel;
use ftui_web::session_record::{SessionRecorder, replay};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::empty(),
        kind: KeyEventKind::Press,
    })
}

fn tab() -> Event {
    key(KeyCode::Tab)
}

fn backtab() -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::BackTab,
        modifiers: Modifiers::SHIFT,
        kind: KeyEventKind::Press,
    })
}

fn mouse_move(x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent::new(MouseEventKind::Moved, x, y))
}

fn mouse_click(x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent::new(
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
    ))
}

fn tick_event() -> Event {
    Event::Tick
}

const TICK_MS: u64 = 100;
const SEED: u64 = 42;

/// Record a session with the given script, then replay and assert determinism.
fn record_and_verify(cols: u16, rows: u16, script: impl FnOnce(&mut SessionRecorder<AppModel>)) {
    let model = AppModel::new();
    let mut rec = SessionRecorder::new(model, cols, rows, SEED);
    rec.init().unwrap();

    script(&mut rec);

    let trace = rec.finish();
    assert!(
        trace.frame_count() > 0,
        "trace must have at least one frame"
    );

    // Replay against a fresh model and verify checksums match.
    let replay_result = replay(AppModel::new(), &trace).unwrap();
    assert!(
        replay_result.ok(),
        "replay checksum mismatch at frame {:?}",
        replay_result.first_mismatch
    );
    assert_eq!(
        replay_result.final_checksum_chain,
        trace.final_checksum_chain().unwrap(),
        "final checksum chain must match"
    );
}

/// Helper to advance time by one tick and step.
fn tick_and_step(rec: &mut SessionRecorder<AppModel>, tick_num: u64) {
    let ts_ns = tick_num * TICK_MS * 1_000_000;
    rec.push_event(ts_ns, tick_event());
    rec.advance_time(ts_ns, Duration::from_millis(TICK_MS));
    rec.step().unwrap();
}

// ---------------------------------------------------------------------------
// Trace 1: Dense dashboard rendering
// ---------------------------------------------------------------------------

/// Records the initial dashboard view (densely populated) and a few ticks
/// to exercise animated widgets (sparklines, gauges, etc.).
#[test]
fn golden_dense_dashboard() {
    record_and_verify(80, 24, |rec| {
        // Let the dashboard tick a few times to populate live widgets.
        for tick in 1..=5 {
            tick_and_step(rec, tick);
        }
    });
}

/// Same as above but at a larger terminal size — catches layout differences.
#[test]
fn golden_dense_dashboard_large() {
    record_and_verify(120, 40, |rec| {
        for tick in 1..=5 {
            tick_and_step(rec, tick);
        }
    });
}

// ---------------------------------------------------------------------------
// Trace 2: Screen navigation
// ---------------------------------------------------------------------------

/// Tab through several screens, exercising different rendering codepaths
/// (text, charts, widgets, syntax highlighting).
#[test]
fn golden_screen_navigation() {
    record_and_verify(120, 40, |rec| {
        let mut ts = 0u64;

        // Dashboard → Shakespeare (dense text).
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, tab());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Shakespeare → CodeExplorer (syntax highlighting).
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, tab());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // CodeExplorer → WidgetGallery (mixed widgets).
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, tab());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // WidgetGallery → LayoutLab.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, tab());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Go back one screen (BackTab).
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, backtab());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();
    });
}

// ---------------------------------------------------------------------------
// Trace 3: Resize storm
// ---------------------------------------------------------------------------

/// Rapid resize events to exercise layout recomputation and buffer allocation.
#[test]
fn golden_resize_storm() {
    record_and_verify(80, 24, |rec| {
        let sizes: &[(u16, u16)] = &[
            (120, 40),
            (60, 20),
            (200, 60),
            (80, 24),
            (40, 12),
            (160, 50),
            (80, 24),
        ];

        for (i, &(w, h)) in sizes.iter().enumerate() {
            let ts = (i as u64 + 1) * TICK_MS * 1_000_000;
            rec.resize(ts, w, h);
            rec.push_event(ts, tick_event());
            rec.advance_time(ts, Duration::from_millis(TICK_MS));
            rec.step().unwrap();
        }
    });
}

// ---------------------------------------------------------------------------
// Trace 4: Mouse interaction
// ---------------------------------------------------------------------------

/// Mouse movement and clicks on the dashboard, exercising hit testing and
/// hover state changes.
#[test]
fn golden_mouse_interaction() {
    record_and_verify(120, 40, |rec| {
        let mut ts = 0u64;

        // Initial tick.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, tick_event());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Move mouse across the top chrome (tab bar area).
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, mouse_move(10, 0));
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Click on the tab area.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, mouse_click(30, 0));
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Move into the content area.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, mouse_move(60, 20));
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Click in the content area.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, mouse_click(60, 20));
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();
    });
}

// ---------------------------------------------------------------------------
// Trace 5: Tick-driven animation
// ---------------------------------------------------------------------------

/// Many consecutive ticks to exercise animated content (sparklines update,
/// clock advances, etc.). Tests that the rendering pipeline stays
/// deterministic over many frames.
#[test]
fn golden_tick_animation() {
    record_and_verify(80, 24, |rec| {
        for tick in 1..=15 {
            tick_and_step(rec, tick);
        }
    });
}

// ---------------------------------------------------------------------------
// Trace 6: Keyboard interaction within a screen
// ---------------------------------------------------------------------------

/// Keyboard events on the Shakespeare screen (scrolling through text).
#[test]
fn golden_keyboard_scrolling() {
    record_and_verify(80, 24, |rec| {
        let mut ts = 0u64;

        // Navigate to Shakespeare screen.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, tab());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Scroll down with arrow keys and page down.
        for _ in 0..5 {
            ts += TICK_MS * 1_000_000;
            rec.push_event(ts, key(KeyCode::Down));
            rec.advance_time(ts, Duration::from_millis(TICK_MS));
            rec.step().unwrap();
        }

        // Page down.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, key(KeyCode::PageDown));
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Scroll back up.
        for _ in 0..3 {
            ts += TICK_MS * 1_000_000;
            rec.push_event(ts, key(KeyCode::Up));
            rec.advance_time(ts, Duration::from_millis(TICK_MS));
            rec.step().unwrap();
        }
    });
}

// ---------------------------------------------------------------------------
// Trace 7: Mixed workload (navigation + resize + mouse)
// ---------------------------------------------------------------------------

/// Combines screen navigation, resize, mouse interaction, and ticks in a
/// single trace. This is the most comprehensive regression gate.
#[test]
fn golden_mixed_workload() {
    record_and_verify(80, 24, |rec| {
        let mut ts = 0u64;

        // A few ticks on dashboard.
        for _ in 0..3 {
            ts += TICK_MS * 1_000_000;
            rec.push_event(ts, tick_event());
            rec.advance_time(ts, Duration::from_millis(TICK_MS));
            rec.step().unwrap();
        }

        // Resize to larger terminal.
        ts += TICK_MS * 1_000_000;
        rec.resize(ts, 120, 40);
        rec.push_event(ts, tick_event());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Navigate to next screen.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, tab());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Mouse movement.
        ts += TICK_MS * 1_000_000;
        rec.push_event(ts, mouse_move(40, 15));
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // Resize back to smaller.
        ts += TICK_MS * 1_000_000;
        rec.resize(ts, 80, 24);
        rec.push_event(ts, tick_event());
        rec.advance_time(ts, Duration::from_millis(TICK_MS));
        rec.step().unwrap();

        // More ticks to settle.
        for _ in 0..2 {
            ts += TICK_MS * 1_000_000;
            rec.push_event(ts, tick_event());
            rec.advance_time(ts, Duration::from_millis(TICK_MS));
            rec.step().unwrap();
        }
    });
}
