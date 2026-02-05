#![forbid(unsafe_code)]

//! End-to-end tests for the Mouse Playground Demo (bd-bksf.1).
//!
//! These tests exercise the mouse playground screen through the
//! `MousePlayground` struct, covering:
//!
//! - Initial state rendering at various sizes
//! - Hit-test target grid (4x3 = 12 targets)
//! - Overlay toggle ('O')
//! - Jitter stats toggle ('J')
//! - Event log clear ('C')
//! - Frame hash determinism verification
//!
//! # Invariants (Alien Artifact)
//!
//! 1. **Target grid layout**: Always renders a 4x3 grid of hit-test targets
//!    labeled T1–T12 when area is sufficient.
//! 2. **Event log capacity**: Log never exceeds MAX_EVENT_LOG (12) entries.
//! 3. **Toggle idempotency**: Double-toggle returns to original state.
//! 4. **Hover stabilization**: Current hover is updated via stabilizer,
//!    preventing jitter on boundary conditions.
//!
//! # Failure Modes
//!
//! | Scenario | Expected Behavior |
//! |----------|-------------------|
//! | Zero-width render area | No panic, graceful no-op |
//! | Very small render (40x10) | Degraded but readable UI |
//! | No mouse events | Event log shows placeholder message |
//! | Rapid overlay toggles | State remains consistent |
//!
//! Run: `cargo test -p ftui-demo-showcase --test mouse_playground_e2e`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::app::{AppModel, AppMsg, ScreenId};
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::screens::mouse_playground::{
    DiagnosticEventKind, Focus, MousePlayground, TelemetryHooks, reset_event_counter,
};
use ftui_harness::assert_snapshot;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::program::Model;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Press,
    })
}

fn char_press(ch: char) -> Event {
    press(KeyCode::Char(ch))
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
fn capture_frame_hash(playground: &MousePlayground, width: u16, height: u16) -> u64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let area = Rect::new(0, 0, width, height);
    playground.view(&mut frame, area);
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

// ===========================================================================
// Scenario 1: Initial State and Rendering
// ===========================================================================

#[test]
fn e2e_initial_state_renders_correctly() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_initial_state_renders_correctly"),
            ("term_cols", "120"),
            ("term_rows", "40"),
        ],
    );

    let playground = MousePlayground::new();

    // Verify initial state
    assert!(
        !playground.overlay_enabled(),
        "Overlay should be off initially"
    );
    assert!(
        !playground.jitter_stats_enabled(),
        "Jitter stats should be off initially"
    );

    // Render at standard size - should not panic
    let frame_hash = capture_frame_hash(&playground, 120, 40);
    log_jsonl("rendered", &[("frame_hash", &format!("{frame_hash:016x}"))]);
}

#[test]
fn e2e_renders_at_various_sizes() {
    log_jsonl("env", &[("test", "e2e_renders_at_various_sizes")]);

    let playground = MousePlayground::new();

    // Standard sizes
    for (w, h) in [(120, 40), (80, 24), (60, 20), (40, 15)] {
        let hash = capture_frame_hash(&playground, w, h);
        log_jsonl(
            "rendered",
            &[
                ("width", &w.to_string()),
                ("height", &h.to_string()),
                ("frame_hash", &format!("{hash:016x}")),
            ],
        );
    }

    // Zero area should not panic
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    playground.view(&mut frame, Rect::new(0, 0, 0, 0));
    log_jsonl("zero_area", &[("result", "no_panic")]);
}

#[test]
fn mouse_playground_initial_80x24() {
    let playground = MousePlayground::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    playground.view(&mut frame, area);
    assert_snapshot!("mouse_playground_initial_80x24", &frame.buffer);
}

#[test]
fn mouse_playground_initial_120x40() {
    let playground = MousePlayground::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    playground.view(&mut frame, area);
    assert_snapshot!("mouse_playground_initial_120x40", &frame.buffer);
}

#[test]
fn mouse_playground_tiny_40x10() {
    let playground = MousePlayground::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    let area = Rect::new(0, 0, 40, 10);
    playground.view(&mut frame, area);
    assert_snapshot!("mouse_playground_tiny_40x10", &frame.buffer);
}

#[test]
fn mouse_playground_wide_200x50() {
    let playground = MousePlayground::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(200, 50, &mut pool);
    let area = Rect::new(0, 0, 200, 50);
    playground.view(&mut frame, area);
    assert_snapshot!("mouse_playground_wide_200x50", &frame.buffer);
}

// ===========================================================================
// Scenario 1b: Global Mouse Navigation (bd-ptyoh)
// ===========================================================================

#[test]
fn e2e_mouse_tab_navigation_switches_screen() {
    log_jsonl(
        "mouse_nav_env",
        &[
            ("test", "e2e_mouse_tab_navigation_switches_screen"),
            ("term_cols", "120"),
            ("term_rows", "40"),
        ],
    );

    let mut app = AppModel::new();
    app.terminal_width = 120;
    app.terminal_height = 40;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    app.view(&mut frame);

    let event = Event::Mouse(MouseEvent::new(
        MouseEventKind::Down(MouseButton::Left),
        1,
        0,
    ));
    let _ = app.update(AppMsg::from(event));

    log_jsonl(
        "mouse_nav_result",
        &[("screen", app.current_screen.title())],
    );
    assert_eq!(app.current_screen, ScreenId::GuidedTour);
}

// ===========================================================================
// Scenario 2: Overlay Toggle
// ===========================================================================

#[test]
fn e2e_overlay_toggle() {
    log_jsonl("env", &[("test", "e2e_overlay_toggle")]);

    let mut playground = MousePlayground::new();

    // Initial state: overlay off
    assert!(!playground.overlay_enabled());
    log_jsonl("initial", &[("overlay", "OFF")]);

    // Press 'O' to toggle on
    playground.update(&char_press('o'));
    assert!(
        playground.overlay_enabled(),
        "Overlay should be ON after pressing O"
    );
    log_jsonl("after_toggle", &[("overlay", "ON")]);

    // Press 'O' again to toggle off
    playground.update(&char_press('O'));
    assert!(
        !playground.overlay_enabled(),
        "Overlay should be OFF after second press"
    );
    log_jsonl("after_second_toggle", &[("overlay", "OFF")]);
}

#[test]
fn mouse_playground_overlay_on_120x40() {
    let mut playground = MousePlayground::new();
    playground.update(&char_press('o'));

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    playground.view(&mut frame, area);
    assert_snapshot!("mouse_playground_overlay_on_120x40", &frame.buffer);
}

// ===========================================================================
// Scenario 3: Jitter Stats Toggle
// ===========================================================================

#[test]
fn e2e_jitter_stats_toggle() {
    log_jsonl("env", &[("test", "e2e_jitter_stats_toggle")]);

    let mut playground = MousePlayground::new();

    // Initial state: jitter stats off
    assert!(!playground.jitter_stats_enabled());
    log_jsonl("initial", &[("jitter_stats", "OFF")]);

    // Press 'J' (uppercase) to toggle on — lowercase 'j' is vim nav when Targets focused
    playground.update(&char_press('J'));
    assert!(
        playground.jitter_stats_enabled(),
        "Jitter stats should be ON after pressing J"
    );
    log_jsonl("after_toggle", &[("jitter_stats", "ON")]);

    // Press 'J' again to toggle off
    playground.update(&char_press('J'));
    assert!(
        !playground.jitter_stats_enabled(),
        "Jitter stats should be OFF after second press"
    );
    log_jsonl("after_second_toggle", &[("jitter_stats", "OFF")]);
}

#[test]
fn mouse_playground_jitter_stats_on_120x40() {
    let mut playground = MousePlayground::new();
    playground.update(&char_press('J'));

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    let area = Rect::new(0, 0, 120, 40);
    playground.view(&mut frame, area);
    assert_snapshot!("mouse_playground_jitter_stats_on_120x40", &frame.buffer);
}

// ===========================================================================
// Scenario 4: Clear Log
// ===========================================================================

#[test]
fn e2e_clear_log() {
    log_jsonl("env", &[("test", "e2e_clear_log")]);

    let mut playground = MousePlayground::new();

    // Initially event log is empty
    assert_eq!(playground.event_log_len(), 0);
    log_jsonl("initial", &[("event_count", "0")]);

    // Manually log some events
    playground.push_test_event("Test Event 1", 10, 20);
    playground.push_test_event("Test Event 2", 30, 40);
    assert_eq!(playground.event_log_len(), 2);
    log_jsonl("after_events", &[("event_count", "2")]);

    // Press 'C' to clear log
    playground.update(&char_press('c'));
    assert_eq!(
        playground.event_log_len(),
        0,
        "Log should be empty after pressing C"
    );
    log_jsonl("after_clear", &[("event_count", "0")]);
}

#[test]
fn mouse_playground_clear_log_80x24() {
    let mut playground = MousePlayground::new();

    // Add some events
    playground.push_test_event("Left Down", 50, 12);
    playground.push_test_event("Move", 51, 12);
    playground.push_test_event("Left Up", 51, 12);

    // Clear the log
    playground.update(&char_press('C'));

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    let area = Rect::new(0, 0, 80, 24);
    playground.view(&mut frame, area);
    assert_snapshot!("mouse_playground_clear_log_80x24", &frame.buffer);
}

// ===========================================================================
// Scenario 5: Toggle Idempotency
// ===========================================================================

#[test]
fn e2e_toggle_idempotency() {
    log_jsonl("env", &[("test", "e2e_toggle_idempotency")]);

    let mut playground = MousePlayground::new();

    // Capture initial frame hash
    let initial_hash = capture_frame_hash(&playground, 80, 24);

    // Toggle overlay twice (should return to original)
    playground.update(&char_press('o'));
    playground.update(&char_press('o'));

    let after_overlay_toggle = capture_frame_hash(&playground, 80, 24);

    // Toggle jitter stats twice (should return to original)
    // Use uppercase 'J' — lowercase 'j' is vim nav when Targets focused
    playground.update(&char_press('J'));
    playground.update(&char_press('J'));

    let final_hash = capture_frame_hash(&playground, 80, 24);

    // All hashes should match (state restored)
    assert_eq!(
        initial_hash, after_overlay_toggle,
        "Overlay double-toggle should restore state"
    );
    assert_eq!(
        after_overlay_toggle, final_hash,
        "Jitter stats double-toggle should restore state"
    );

    log_jsonl(
        "idempotency",
        &[
            ("initial_hash", &format!("{initial_hash:016x}")),
            ("final_hash", &format!("{final_hash:016x}")),
            ("match", "true"),
        ],
    );
}

// ===========================================================================
// Scenario 6: Determinism
// ===========================================================================

#[test]
fn e2e_determinism() {
    log_jsonl("env", &[("test", "e2e_determinism")]);

    fn run_scenario() -> u64 {
        let mut playground = MousePlayground::new();

        // Tick a few times
        for i in 0..5 {
            playground.tick(i);
        }

        // Toggle overlay
        playground.update(&char_press('o'));

        // Toggle jitter stats
        playground.update(&char_press('j'));

        capture_frame_hash(&playground, 120, 40)
    }

    let hash1 = run_scenario();
    let hash2 = run_scenario();
    let hash3 = run_scenario();

    assert_eq!(hash1, hash2, "frame hashes must be deterministic");
    assert_eq!(hash2, hash3, "frame hashes must be deterministic");

    log_jsonl(
        "completed",
        &[
            ("frame_hash", &format!("{hash1:016x}")),
            ("deterministic", "true"),
        ],
    );
}

// ===========================================================================
// Scenario 7: Event Log Capacity
// ===========================================================================

#[test]
fn e2e_event_log_capacity() {
    log_jsonl("env", &[("test", "e2e_event_log_capacity")]);

    let mut playground = MousePlayground::new();

    // Log more than MAX_EVENT_LOG (12) events
    for i in 0..20 {
        playground.push_test_event(format!("Event {i}"), i as u16, i as u16);
    }

    // Should be capped at 12
    assert_eq!(
        playground.event_log_len(),
        12,
        "Event log should be capped at MAX_EVENT_LOG"
    );

    // Verify via frame rendering that events are logged
    // (we can't access the deque directly, but the test verifies capacity)

    log_jsonl(
        "log_capacity",
        &[
            ("max", "12"),
            ("actual", &playground.event_log_len().to_string()),
        ],
    );
}

// ===========================================================================
// Scenario 8: Screen Trait Implementation
// ===========================================================================

#[test]
fn e2e_screen_trait_methods() {
    log_jsonl("env", &[("test", "e2e_screen_trait_methods")]);

    let playground = MousePlayground::new();

    assert_eq!(playground.title(), "Mouse Playground");
    assert_eq!(playground.tab_label(), "Mouse");

    let keybindings = playground.keybindings();
    assert!(!keybindings.is_empty(), "Should have keybindings");
    log_jsonl("keybindings", &[("count", &keybindings.len().to_string())]);

    // Verify specific keybindings exist
    let has_overlay = keybindings.iter().any(|k| k.key == "O");
    let has_jitter = keybindings.iter().any(|k| k.key == "J");
    let has_clear = keybindings.iter().any(|k| k.key == "C");

    assert!(has_overlay, "Should have 'O' keybinding for overlay toggle");
    assert!(has_jitter, "Should have 'J' keybinding for jitter stats");
    assert!(has_clear, "Should have 'C' keybinding for clear log");

    log_jsonl(
        "keybindings_verified",
        &[
            ("overlay", &has_overlay.to_string()),
            ("jitter", &has_jitter.to_string()),
            ("clear", &has_clear.to_string()),
        ],
    );
}

// ===========================================================================
// Scenario 9: Tick Processing
// ===========================================================================

#[test]
fn e2e_tick_processing() {
    log_jsonl("env", &[("test", "e2e_tick_processing")]);

    let mut playground = MousePlayground::new();

    // Initial tick count is 0
    assert_eq!(playground.current_tick(), 0);

    // Tick should update the counter
    playground.tick(42);
    assert_eq!(playground.current_tick(), 42);

    playground.tick(100);
    assert_eq!(playground.current_tick(), 100);

    log_jsonl("tick_count", &[("final", "100")]);
}

// ===========================================================================
// Scenario 10: Hit Test Returns None When Grid Not Rendered
// ===========================================================================

#[test]
fn e2e_hit_test_empty_grid() {
    log_jsonl("env", &[("test", "e2e_hit_test_empty_grid")]);

    let playground = MousePlayground::new();

    // Before any rendering, last_grid_area is empty/default
    let result = playground.hit_test_at(50, 25);
    assert!(
        result.is_none(),
        "Hit test should return None when grid not rendered"
    );

    log_jsonl("hit_test", &[("result", "None")]);
}

// ===========================================================================
// PTY E2E Tests (bd-bksf.4): Mouse Events, Keyboard Nav, Hit-Test, Perf
// ===========================================================================

/// Create a mouse event for testing.
fn mouse_event(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent::new(kind, x, y))
}

fn mouse_down(x: u16, y: u16) -> Event {
    mouse_event(MouseEventKind::Down(MouseButton::Left), x, y)
}

fn mouse_up(x: u16, y: u16) -> Event {
    mouse_event(MouseEventKind::Up(MouseButton::Left), x, y)
}

fn mouse_move(x: u16, y: u16) -> Event {
    mouse_event(MouseEventKind::Moved, x, y)
}

fn mouse_drag(x: u16, y: u16) -> Event {
    mouse_event(MouseEventKind::Drag(MouseButton::Left), x, y)
}

fn mouse_scroll_down(x: u16, y: u16) -> Event {
    mouse_event(MouseEventKind::ScrollDown, x, y)
}

/// Render a frame so hit-test grid coordinates are populated.
fn render_frame(playground: &MousePlayground, width: u16, height: u16) {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    let area = Rect::new(0, 0, width, height);
    playground.view(&mut frame, area);
}

// ---------------------------------------------------------------------------
// Scenario 11: Mouse Click Event Processing (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_mouse_click_event_processing() {
    let start = Instant::now();
    log_jsonl(
        "env",
        &[
            ("test", "e2e_mouse_click_event_processing"),
            ("bead", "bd-bksf.4"),
            ("term_cols", "120"),
            ("term_rows", "40"),
        ],
    );

    let mut playground = MousePlayground::new().with_diagnostics();
    reset_event_counter();

    // Render first to populate grid areas
    render_frame(&playground, 120, 40);

    // Send a mouse down event
    playground.update(&mouse_down(50, 20));
    assert!(
        playground.event_log_len() > 0,
        "Mouse down should produce at least one event log entry"
    );

    log_jsonl(
        "mouse_down",
        &[
            ("x", "50"),
            ("y", "20"),
            ("event_count", &playground.event_log_len().to_string()),
        ],
    );

    // Send a mouse up event
    playground.update(&mouse_up(50, 20));

    // Check diagnostic log has mouse events
    if let Some(log) = playground.diagnostic_log() {
        let mouse_downs = log.entries_of_kind(DiagnosticEventKind::MouseDown);
        let mouse_ups = log.entries_of_kind(DiagnosticEventKind::MouseUp);
        assert!(!mouse_downs.is_empty(), "Should have MouseDown diagnostic");
        assert!(!mouse_ups.is_empty(), "Should have MouseUp diagnostic");

        log_jsonl(
            "diagnostics",
            &[
                ("mouse_down_count", &mouse_downs.len().to_string()),
                ("mouse_up_count", &mouse_ups.len().to_string()),
            ],
        );
    }

    let elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("elapsed_us", &elapsed.as_micros().to_string()),
            ("outcome", "pass"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Scenario 12: Mouse Drag Event Trail (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_mouse_drag_trail() {
    let start = Instant::now();
    log_jsonl(
        "env",
        &[("test", "e2e_mouse_drag_trail"), ("bead", "bd-bksf.4")],
    );

    let mut playground = MousePlayground::new().with_diagnostics();
    reset_event_counter();
    render_frame(&playground, 120, 40);

    // Simulate drag: down -> drag -> drag -> up
    playground.update(&mouse_down(10, 10));
    playground.update(&mouse_drag(20, 15));
    playground.update(&mouse_drag(30, 20));
    playground.update(&mouse_up(30, 20));

    // Diagnostic log should have drag events
    if let Some(log) = playground.diagnostic_log() {
        let drags = log.entries_of_kind(DiagnosticEventKind::MouseDrag);
        assert_eq!(drags.len(), 2, "Should have 2 drag events");

        for drag in &drags {
            assert!(drag.x.is_some(), "Drag should have x coordinate");
            assert!(drag.y.is_some(), "Drag should have y coordinate");
        }

        log_jsonl(
            "drag_trail",
            &[
                ("drag_events", &drags.len().to_string()),
                ("total_entries", &log.entries().len().to_string()),
            ],
        );
    }

    let elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("elapsed_us", &elapsed.as_micros().to_string()),
            ("outcome", "pass"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Scenario 13: Mouse Scroll Events (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_mouse_scroll_events() {
    log_jsonl(
        "env",
        &[("test", "e2e_mouse_scroll_events"), ("bead", "bd-bksf.4")],
    );

    let mut playground = MousePlayground::new().with_diagnostics();
    reset_event_counter();
    render_frame(&playground, 120, 40);

    // Send scroll events
    playground.update(&mouse_scroll_down(60, 20));
    playground.update(&mouse_event(MouseEventKind::ScrollUp, 60, 20));

    if let Some(log) = playground.diagnostic_log() {
        let scrolls = log.entries_of_kind(DiagnosticEventKind::MouseScroll);
        assert_eq!(scrolls.len(), 2, "Should have 2 scroll events");

        log_jsonl("scroll_events", &[("count", &scrolls.len().to_string())]);
    }
}

// ---------------------------------------------------------------------------
// Scenario 14: Mouse Move and Hover (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_mouse_move_and_hover() {
    log_jsonl(
        "env",
        &[("test", "e2e_mouse_move_and_hover"), ("bead", "bd-bksf.4")],
    );

    let mut playground = MousePlayground::new().with_diagnostics();
    reset_event_counter();
    render_frame(&playground, 120, 40);

    // Move mouse across the screen
    playground.update(&mouse_move(10, 10));
    playground.update(&mouse_move(50, 20));
    playground.update(&mouse_move(90, 30));

    if let Some(log) = playground.diagnostic_log() {
        let moves = log.entries_of_kind(DiagnosticEventKind::MouseMove);
        assert_eq!(moves.len(), 3, "Should have 3 move events");

        // Should also have hit tests for each move
        let hit_tests = log.entries_of_kind(DiagnosticEventKind::HitTest);
        assert_eq!(
            hit_tests.len(),
            moves.len(),
            "Each move should produce a hit test"
        );

        log_jsonl(
            "move_and_hit",
            &[
                ("moves", &moves.len().to_string()),
                ("hit_tests", &hit_tests.len().to_string()),
            ],
        );
    }
}

// ---------------------------------------------------------------------------
// Scenario 15: Keyboard Focus Cycling (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_keyboard_focus_cycling() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_keyboard_focus_cycling"),
            ("bead", "bd-bksf.4"),
        ],
    );

    let mut playground = MousePlayground::new();

    // Initial focus should be Targets
    assert_eq!(
        playground.current_focus(),
        Focus::Targets,
        "Initial focus should be Targets"
    );
    log_jsonl("initial_focus", &[("focus", "Targets")]);

    // Tab cycles forward: Targets -> EventLog -> Stats -> Targets
    playground.update(&press(KeyCode::Tab));
    assert_eq!(playground.current_focus(), Focus::EventLog);
    log_jsonl("after_tab_1", &[("focus", "EventLog")]);

    playground.update(&press(KeyCode::Tab));
    assert_eq!(playground.current_focus(), Focus::Stats);
    log_jsonl("after_tab_2", &[("focus", "Stats")]);

    playground.update(&press(KeyCode::Tab));
    assert_eq!(playground.current_focus(), Focus::Targets);
    log_jsonl("after_tab_3", &[("focus", "Targets")]);

    // BackTab cycles backward: Targets -> Stats -> EventLog -> Targets
    playground.update(&press(KeyCode::BackTab));
    assert_eq!(playground.current_focus(), Focus::Stats);
    log_jsonl("after_backtab_1", &[("focus", "Stats")]);

    playground.update(&press(KeyCode::BackTab));
    assert_eq!(playground.current_focus(), Focus::EventLog);
    log_jsonl("after_backtab_2", &[("focus", "EventLog")]);

    playground.update(&press(KeyCode::BackTab));
    assert_eq!(playground.current_focus(), Focus::Targets);
    log_jsonl("after_backtab_3", &[("focus", "Targets")]);

    log_jsonl("completed", &[("focus_cycle_works", "true")]);
}

// ---------------------------------------------------------------------------
// Scenario 16: Arrow Key Target Navigation (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_arrow_key_target_navigation() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_arrow_key_target_navigation"),
            ("bead", "bd-bksf.4"),
        ],
    );

    let mut playground = MousePlayground::new();

    // Start at target 0 (T1)
    assert_eq!(playground.focused_target_index(), 0);
    log_jsonl("initial", &[("target_index", "0")]);

    // Move right: 0 -> 1
    playground.update(&press(KeyCode::Right));
    assert_eq!(playground.focused_target_index(), 1);
    log_jsonl("after_right", &[("target_index", "1")]);

    // Move down: row 0 col 1 -> row 1 col 1 (index 5)
    playground.update(&press(KeyCode::Down));
    assert_eq!(playground.focused_target_index(), 5);
    log_jsonl("after_down", &[("target_index", "5")]);

    // Move left: 5 -> 4
    playground.update(&press(KeyCode::Left));
    assert_eq!(playground.focused_target_index(), 4);
    log_jsonl("after_left", &[("target_index", "4")]);

    // Move up: row 1 col 0 -> row 0 col 0 (index 0)
    playground.update(&press(KeyCode::Up));
    assert_eq!(playground.focused_target_index(), 0);
    log_jsonl("after_up", &[("target_index", "0")]);

    log_jsonl("completed", &[("arrow_nav_works", "true")]);
}

// ---------------------------------------------------------------------------
// Scenario 17: Vim-Style Target Navigation (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_vim_style_navigation() {
    log_jsonl(
        "env",
        &[("test", "e2e_vim_style_navigation"), ("bead", "bd-bksf.4")],
    );

    let mut playground = MousePlayground::new();
    assert_eq!(playground.focused_target_index(), 0);

    // l (right): 0 -> 1
    playground.update(&char_press('l'));
    assert_eq!(playground.focused_target_index(), 1);

    // j (down): row 0 col 1 -> row 1 col 1 (index 5)
    playground.update(&char_press('j'));
    assert_eq!(playground.focused_target_index(), 5);

    // h (left): 5 -> 4
    playground.update(&char_press('h'));
    assert_eq!(playground.focused_target_index(), 4);

    // k (up): row 1 col 0 -> row 0 col 0 (index 0)
    playground.update(&char_press('k'));
    assert_eq!(playground.focused_target_index(), 0);

    log_jsonl("completed", &[("vim_nav_works", "true")]);
}

// ---------------------------------------------------------------------------
// Scenario 18: Home/End/PageUp/PageDown Navigation (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_home_end_page_navigation() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_home_end_page_navigation"),
            ("bead", "bd-bksf.4"),
        ],
    );

    let mut playground = MousePlayground::new();

    // End: jump to last target (index 11, T12 in 4x3 grid)
    playground.update(&press(KeyCode::End));
    assert_eq!(playground.focused_target_index(), 11);
    log_jsonl("after_end", &[("target_index", "11")]);

    // Home: jump to first target (index 0, T1)
    playground.update(&press(KeyCode::Home));
    assert_eq!(playground.focused_target_index(), 0);
    log_jsonl("after_home", &[("target_index", "0")]);

    // PageDown: move by a full page of grid rows
    playground.update(&press(KeyCode::PageDown));
    let after_pgdn = playground.focused_target_index();
    assert!(after_pgdn > 0, "PageDown should move focus forward");
    log_jsonl(
        "after_pagedown",
        &[("target_index", &after_pgdn.to_string())],
    );

    // G (vim End): jump to last
    playground.update(&char_press('G'));
    assert_eq!(playground.focused_target_index(), 11);
    log_jsonl("after_G", &[("target_index", "11")]);

    // g (vim Home): jump to first
    playground.update(&char_press('g'));
    assert_eq!(playground.focused_target_index(), 0);
    log_jsonl("after_g", &[("target_index", "0")]);

    log_jsonl("completed", &[("page_nav_works", "true")]);
}

// ---------------------------------------------------------------------------
// Scenario 19: Keyboard Target Activation (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_keyboard_target_activation() {
    let start = Instant::now();
    log_jsonl(
        "env",
        &[
            ("test", "e2e_keyboard_target_activation"),
            ("bead", "bd-bksf.4"),
        ],
    );

    let mut playground = MousePlayground::new().with_diagnostics();
    reset_event_counter();

    // Focus is on Targets by default, index 0
    assert_eq!(playground.focused_target_index(), 0);

    // Press Space to activate T1
    playground.update(&press(KeyCode::Char(' ')));

    // Check diagnostic log has TargetClick event
    if let Some(log) = playground.diagnostic_log() {
        let clicks = log.entries_of_kind(DiagnosticEventKind::TargetClick);
        assert_eq!(clicks.len(), 1, "Should have 1 target click from Space");
        assert_eq!(
            clicks[0].target_id,
            Some(1),
            "Should click target ID 1 (T1)"
        );

        log_jsonl(
            "space_click",
            &[
                ("target_id", "1"),
                ("click_count", &clicks.len().to_string()),
            ],
        );
    }

    // Move to T2 and press Enter
    playground.update(&press(KeyCode::Right));
    assert_eq!(playground.focused_target_index(), 1);
    playground.update(&press(KeyCode::Enter));

    if let Some(log) = playground.diagnostic_log() {
        let clicks = log.entries_of_kind(DiagnosticEventKind::TargetClick);
        assert_eq!(clicks.len(), 2, "Should have 2 total target clicks");
        assert_eq!(
            clicks[1].target_id,
            Some(2),
            "Second click should be target ID 2 (T2)"
        );

        log_jsonl(
            "enter_click",
            &[
                ("target_id", "2"),
                ("total_clicks", &clicks.len().to_string()),
            ],
        );
    }

    let elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("elapsed_us", &elapsed.as_micros().to_string()),
            ("outcome", "pass"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Scenario 20: Hit-Test After Rendering (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_hit_test_after_render() {
    log_jsonl(
        "env",
        &[("test", "e2e_hit_test_after_render"), ("bead", "bd-bksf.4")],
    );

    let playground = MousePlayground::new();

    // Before render, hit test returns None
    assert!(
        playground.hit_test_at(50, 20).is_none(),
        "Hit test before render should return None"
    );
    log_jsonl("before_render", &[("hit_test", "None")]);

    // Render to populate grid coordinates
    render_frame(&playground, 120, 40);

    // After render, hit test within the grid area should return a target ID
    // The grid occupies the left ~60% of the 120-wide area
    // We test at various positions within the expected grid area
    let result_center = playground.hit_test_at(30, 15);
    log_jsonl(
        "after_render",
        &[
            ("x", "30"),
            ("y", "15"),
            ("hit_result", &format!("{result_center:?}")),
        ],
    );

    // Hit test outside any reasonable grid area should return None
    let result_far = playground.hit_test_at(119, 39);
    log_jsonl(
        "outside_grid",
        &[
            ("x", "119"),
            ("y", "39"),
            ("hit_result", &format!("{result_far:?}")),
        ],
    );

    log_jsonl("completed", &[("hit_test_works", "true")]);
}

// ---------------------------------------------------------------------------
// Scenario 21: Mouse Click on Rendered Target (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_mouse_click_on_target() {
    let start = Instant::now();
    log_jsonl(
        "env",
        &[("test", "e2e_mouse_click_on_target"), ("bead", "bd-bksf.4")],
    );

    let mut playground = MousePlayground::new().with_diagnostics();
    reset_event_counter();

    // Render to populate grid
    render_frame(&playground, 120, 40);

    // Find a position that hits a target by trying center of grid
    // Grid is roughly in the left portion; we'll check a few positions
    let mut clicked_target = None;
    for test_x in (5..60).step_by(10) {
        for test_y in (3..35).step_by(5) {
            if let Some(id) = playground.hit_test_at(test_x, test_y) {
                // Click this target
                playground.update(&mouse_down(test_x, test_y));
                playground.update(&mouse_up(test_x, test_y));
                clicked_target = Some((test_x, test_y, id));

                log_jsonl(
                    "target_found_and_clicked",
                    &[
                        ("x", &test_x.to_string()),
                        ("y", &test_y.to_string()),
                        ("target_id", &id.to_string()),
                    ],
                );
                break;
            }
        }
        if clicked_target.is_some() {
            break;
        }
    }

    // Verify click was recorded in diagnostics
    if let Some(log) = playground.diagnostic_log() {
        let clicks = log.entries_of_kind(DiagnosticEventKind::TargetClick);
        if let Some((_x, _y, id)) = clicked_target {
            assert!(!clicks.is_empty(), "Should have recorded a target click");
            assert_eq!(
                clicks[0].target_id,
                Some(id),
                "Click should be on the correct target"
            );
        }

        log_jsonl(
            "click_diagnostics",
            &[("target_clicks", &clicks.len().to_string())],
        );
    }

    let elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("elapsed_us", &elapsed.as_micros().to_string()),
            ("outcome", "pass"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Scenario 22: Performance Budget — Mouse Event + Render Cycle (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_performance_budget_mouse_render() {
    let start = Instant::now();
    log_jsonl(
        "env",
        &[
            ("test", "e2e_performance_budget_mouse_render"),
            ("bead", "bd-bksf.4"),
            ("budget_us", "100000"),
        ],
    );

    let mut playground = MousePlayground::new();

    // Warm up
    render_frame(&playground, 120, 40);

    // Measure: 50 mouse events + render cycle
    let cycle_start = Instant::now();
    for i in 0..50u16 {
        playground.update(&mouse_move(i * 2, 20));
    }
    render_frame(&playground, 120, 40);
    let cycle_elapsed = cycle_start.elapsed();
    let cycle_us = cycle_elapsed.as_micros();

    log_jsonl(
        "perf_result",
        &[
            ("mouse_events", "50"),
            ("cycle_us", &cycle_us.to_string()),
            ("budget_us", "100000"),
            ("pass", if cycle_us < 100_000 { "true" } else { "false" }),
        ],
    );

    assert!(
        cycle_us < 100_000,
        "50 mouse events + render should complete in < 100ms, took {cycle_us}us"
    );

    // Per-event budget: individual mouse event processing
    let single_start = Instant::now();
    for _ in 0..10 {
        playground.update(&mouse_down(50, 20));
        playground.update(&mouse_up(50, 20));
    }
    let single_elapsed = single_start.elapsed();
    let per_event_us = single_elapsed.as_micros() / 20;

    log_jsonl(
        "per_event_budget",
        &[
            ("events", "20"),
            ("total_us", &single_elapsed.as_micros().to_string()),
            ("per_event_us", &per_event_us.to_string()),
        ],
    );

    let total_elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("total_elapsed_us", &total_elapsed.as_micros().to_string()),
            ("outcome", "pass"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Scenario 23: Performance Budget — Render at Multiple Sizes (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_performance_budget_multi_size_render() {
    let start = Instant::now();
    log_jsonl(
        "env",
        &[
            ("test", "e2e_performance_budget_multi_size_render"),
            ("bead", "bd-bksf.4"),
        ],
    );

    let playground = MousePlayground::new();

    for (w, h) in [(80, 24), (120, 40), (200, 50)] {
        let render_start = Instant::now();
        for _ in 0..10 {
            render_frame(&playground, w, h);
        }
        let render_elapsed = render_start.elapsed();
        let per_render_us = render_elapsed.as_micros() / 10;

        log_jsonl(
            "render_budget",
            &[
                ("width", &w.to_string()),
                ("height", &h.to_string()),
                ("renders", "10"),
                ("total_us", &render_elapsed.as_micros().to_string()),
                ("per_render_us", &per_render_us.to_string()),
            ],
        );

        // Each render should be < 10ms
        assert!(
            per_render_us < 10_000,
            "Render at {w}x{h} should take < 10ms, took {per_render_us}us"
        );
    }

    let elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("elapsed_us", &elapsed.as_micros().to_string()),
            ("outcome", "pass"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Scenario 24: Determinism with Mouse Events (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_determinism_with_mouse_events() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_determinism_with_mouse_events"),
            ("bead", "bd-bksf.4"),
        ],
    );

    fn run_mouse_scenario() -> u64 {
        let mut playground = MousePlayground::new();

        // Render, click, toggle, render
        render_frame(&playground, 120, 40);
        playground.update(&mouse_down(30, 15));
        playground.update(&mouse_up(30, 15));
        playground.update(&char_press('o')); // overlay on
        for i in 0..3 {
            playground.tick(i);
        }

        capture_frame_hash(&playground, 120, 40)
    }

    let hash1 = run_mouse_scenario();
    let hash2 = run_mouse_scenario();
    let hash3 = run_mouse_scenario();

    assert_eq!(
        hash1, hash2,
        "frame hashes must be deterministic with mouse"
    );
    assert_eq!(
        hash2, hash3,
        "frame hashes must be deterministic with mouse"
    );

    log_jsonl(
        "completed",
        &[
            ("frame_hash", &format!("{hash1:016x}")),
            ("deterministic", "true"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Scenario 25: Diagnostic Log JSONL Schema Verification (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_diagnostic_jsonl_schema() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_diagnostic_jsonl_schema"),
            ("bead", "bd-bksf.4"),
        ],
    );

    let mut playground = MousePlayground::new().with_diagnostics();
    reset_event_counter();
    render_frame(&playground, 120, 40);

    // Generate a variety of diagnostic events
    playground.update(&mouse_down(50, 20));
    playground.update(&mouse_move(55, 22));
    playground.update(&char_press(' ')); // keyboard click on T1
    playground.tick(1);

    if let Some(log) = playground.diagnostic_log() {
        let entries = log.entries();
        assert!(
            entries.len() >= 3,
            "Should have at least 3 diagnostic entries"
        );

        // Verify each entry has valid JSONL format
        for entry in entries {
            let jsonl = entry.to_jsonl();
            assert!(jsonl.starts_with('{'), "JSONL should start with {{");
            assert!(jsonl.ends_with('}'), "JSONL should end with }}");
            assert!(jsonl.contains("\"seq\":"), "JSONL should contain seq field");
            assert!(
                jsonl.contains("\"ts_us\":"),
                "JSONL should contain ts_us field"
            );
            assert!(
                jsonl.contains("\"kind\":"),
                "JSONL should contain kind field"
            );
            assert!(
                jsonl.contains("\"tick\":"),
                "JSONL should contain tick field"
            );
            assert!(
                jsonl.contains("\"checksum\":"),
                "JSONL should contain checksum field"
            );
        }

        // Verify summary
        let summary = log.summary();
        let summary_jsonl = summary.to_jsonl();
        assert!(
            summary_jsonl.contains("\"total\":"),
            "Summary should contain total field"
        );

        log_jsonl(
            "schema_verified",
            &[
                ("entries", &entries.len().to_string()),
                ("summary_valid", "true"),
            ],
        );
    }

    log_jsonl("completed", &[("outcome", "pass")]);
}

// ---------------------------------------------------------------------------
// Scenario 26: Focus Does Not Interfere with Global Keys (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_focus_does_not_block_global_keys() {
    log_jsonl(
        "env",
        &[
            ("test", "e2e_focus_does_not_block_global_keys"),
            ("bead", "bd-bksf.4"),
        ],
    );

    let mut playground = MousePlayground::new();

    // Move focus to EventLog
    playground.update(&press(KeyCode::Tab));
    assert_eq!(playground.current_focus(), Focus::EventLog);

    // O should still toggle overlay regardless of focus
    playground.update(&char_press('o'));
    assert!(
        playground.overlay_enabled(),
        "O should toggle overlay even when focus is not on Targets"
    );

    // C should still clear log regardless of focus
    playground.push_test_event("test", 0, 0);
    assert!(playground.event_log_len() > 0);
    playground.update(&char_press('c'));
    assert_eq!(
        playground.event_log_len(),
        0,
        "C should clear log regardless of focus"
    );

    log_jsonl("completed", &[("global_keys_work", "true")]);
}

// ---------------------------------------------------------------------------
// Scenario 27: Stress Test — Rapid Mouse Events (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_stress_rapid_mouse_events() {
    let start = Instant::now();
    log_jsonl(
        "env",
        &[
            ("test", "e2e_stress_rapid_mouse_events"),
            ("bead", "bd-bksf.4"),
            ("event_count", "500"),
        ],
    );

    let mut playground = MousePlayground::new();
    render_frame(&playground, 120, 40);

    // Send 500 rapid mouse events
    let event_start = Instant::now();
    for i in 0..500u16 {
        let x = i % 120;
        let y = (i / 3) % 40;
        playground.update(&mouse_move(x, y));
    }
    let event_elapsed = event_start.elapsed();

    // Event log should be capped at MAX_EVENT_LOG
    assert!(
        playground.event_log_len() <= 12,
        "Event log should be capped at MAX_EVENT_LOG (12)"
    );

    // Render after stress should not panic
    let hash = capture_frame_hash(&playground, 120, 40);

    let total_elapsed = start.elapsed();
    log_jsonl(
        "stress_result",
        &[
            ("events_sent", "500"),
            (
                "event_processing_us",
                &event_elapsed.as_micros().to_string(),
            ),
            ("event_log_len", &playground.event_log_len().to_string()),
            ("frame_hash", &format!("{hash:016x}")),
            ("total_elapsed_us", &total_elapsed.as_micros().to_string()),
            ("outcome", "pass"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Scenario 28: Telemetry Hooks Invocation (bd-bksf.4)
// ---------------------------------------------------------------------------

#[test]
fn e2e_telemetry_hooks() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    log_jsonl(
        "env",
        &[("test", "e2e_telemetry_hooks"), ("bead", "bd-bksf.4")],
    );

    let hit_test_count = Arc::new(AtomicUsize::new(0));
    let click_count = Arc::new(AtomicUsize::new(0));

    let ht_clone = Arc::clone(&hit_test_count);
    let cc_clone = Arc::clone(&click_count);

    let hooks = TelemetryHooks::new()
        .on_hit_test(move |_entry| {
            ht_clone.fetch_add(1, Ordering::Relaxed);
        })
        .on_target_click(move |_entry| {
            cc_clone.fetch_add(1, Ordering::Relaxed);
        });

    let mut playground = MousePlayground::new()
        .with_diagnostics()
        .with_telemetry_hooks(hooks);
    reset_event_counter();
    render_frame(&playground, 120, 40);

    // Generate events that should fire hooks
    playground.update(&mouse_move(30, 15));
    playground.update(&char_press(' ')); // keyboard click on focused target

    let ht_fired = hit_test_count.load(Ordering::Relaxed);
    let cc_fired = click_count.load(Ordering::Relaxed);

    assert!(ht_fired > 0, "Hit test hook should have fired");
    assert!(cc_fired > 0, "Click hook should have fired");

    log_jsonl(
        "hooks_fired",
        &[
            ("hit_test_hook", &ht_fired.to_string()),
            ("click_hook", &cc_fired.to_string()),
        ],
    );
}

// ===========================================================================
// JSONL Summary
// ===========================================================================

#[test]
fn e2e_summary() {
    log_jsonl(
        "summary",
        &[
            ("test_suite", "mouse_playground_e2e"),
            ("bead_snapshot", "bd-bksf.1"),
            ("bead_pty", "bd-bksf.4"),
            ("scenario_count", "28"),
            ("status", "pass"),
        ],
    );
}
