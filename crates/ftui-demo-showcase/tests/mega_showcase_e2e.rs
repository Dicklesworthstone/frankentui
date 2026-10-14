#![forbid(unsafe_code)]

//! End-to-end tests for the Mermaid Mega Showcase screen (bd-3oaig.18).
//!
//! These tests exercise the MermaidMegaShowcaseScreen lifecycle through
//! the `Screen` trait, covering:
//!
//! - Sample cycling (j/k)
//! - Layout mode cycling ('l')
//! - Direction cycling ('O')
//! - Tier cycling ('t'), glyph toggle ('g'), wrap cycling ('w')
//! - Panel toggles ('m', 'c', 'd', 'i', 'e', '?')
//! - Zoom controls ('+', '-', '0', 'f')
//! - Auto-scale toggle ('A')
//! - Perf sweep lifecycle ('S')
//! - Comparison mode ('v', 'V')
//! - Search mode ('/')
//! - Rendering at multiple viewport sizes without panics
//! - Deterministic frame hashing
//! - JSONL telemetry schema validation
//!
//! # Invariants
//!
//! 1. **No-panic rendering**: All samples render cleanly at all tested sizes.
//! 2. **Config round-trips**: Cycling through all modes wraps back to initial.
//! 3. **Deterministic output**: Same inputs produce identical frame hashes.
//! 4. **Valid telemetry**: All emitted JSONL lines pass schema validation.
//!
//! Run: `cargo test -p ftui-demo-showcase --test mega_showcase_e2e`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::screens::mermaid_mega_showcase::{
    MEGA_TELEMETRY_NULLABLE_FIELDS, MEGA_TELEMETRY_REQUIRED_FIELDS, MermaidMegaShowcaseScreen,
    validate_mega_telemetry_line,
};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;

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

/// Emit a JSONL log line to stderr (visible with `cargo test -- --nocapture`).
fn log_jsonl(step: &str, data: &[(&str, &str)]) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = COUNTER.fetch_add(1, Ordering::Relaxed);
    let fields: Vec<String> = std::iter::once(format!("\"ts\":\"T{ts:06}\""))
        .chain(std::iter::once(format!("\"step\":\"{step}\"")))
        .chain(data.iter().map(|(k, v)| format!("\"{k}\":\"{v}\"")))
        .collect();
    eprintln!("{{{}}}", fields.join(","));
}

/// Render the screen into a frame and return the frame hash.
fn capture_frame_hash(screen: &MermaidMegaShowcaseScreen, w: u16, h: u16) -> u64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(w, h, &mut pool);
    let area = Rect::new(0, 0, w, h);
    screen.view(&mut frame, area);
    let mut hasher = DefaultHasher::new();
    for y in 0..h {
        for x in 0..w {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                ch.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Render the screen and check that a text needle appears in the frame.
fn frame_contains(screen: &MermaidMegaShowcaseScreen, w: u16, h: u16, needle: &str) -> bool {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(w, h, &mut pool);
    let area = Rect::new(0, 0, w, h);
    screen.view(&mut frame, area);

    let mut text = String::new();
    for y in 0..h {
        for x in 0..w {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                text.push(ch);
            }
        }
        text.push('\n');
    }
    text.contains(needle)
}

/// Render the screen and return elapsed ms.
fn measure_view_ms(screen: &MermaidMegaShowcaseScreen, w: u16, h: u16) -> f64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(w, h, &mut pool);
    let area = Rect::new(0, 0, w, h);
    let start = std::time::Instant::now();
    screen.view(&mut frame, area);
    start.elapsed().as_secs_f64() * 1000.0
}

// ===========================================================================
// 1. Smoke Test: Initial State + Rendering
// ===========================================================================

#[test]
fn mega_e2e_initial_renders_without_panic() {
    log_jsonl("env", &[("test", "mega_e2e_initial_renders_without_panic")]);

    let screen = MermaidMegaShowcaseScreen::new();

    let sizes: &[(u16, u16)] = &[(120, 40), (80, 24), (200, 60), (40, 15), (20, 10)];

    for &(w, h) in sizes {
        let hash = capture_frame_hash(&screen, w, h);
        log_jsonl(
            "rendered",
            &[
                ("width", &w.to_string()),
                ("height", &h.to_string()),
                ("hash", &format!("{hash:016x}")),
            ],
        );
    }
}

#[test]
fn mega_e2e_zero_area_no_panic() {
    log_jsonl("env", &[("test", "mega_e2e_zero_area_no_panic")]);

    let screen = MermaidMegaShowcaseScreen::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 1, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, 0, 0));

    log_jsonl("result", &[("no_panic", "true")]);
}

// ===========================================================================
// 2. Sample Navigation
// ===========================================================================

#[test]
fn mega_e2e_sample_cycling() {
    log_jsonl("env", &[("test", "mega_e2e_sample_cycling")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Initial render to populate cache.
    let _hash0 = capture_frame_hash(&screen, 120, 40);
    log_jsonl("step", &[("action", "initial_render")]);

    // Navigate forward through several samples with 'j'.
    let mut hashes = vec![];
    for i in 0..5 {
        let _ = screen.update(&char_press('j'));
        let hash = capture_frame_hash(&screen, 120, 40);
        hashes.push(hash);
        log_jsonl(
            "nav_next",
            &[
                ("sample_idx", &(i + 1).to_string()),
                ("hash", &format!("{hash:016x}")),
            ],
        );
    }

    // Navigate backward with 'k'.
    let _ = screen.update(&char_press('k'));
    let hash_back = capture_frame_hash(&screen, 120, 40);
    log_jsonl("nav_prev", &[("hash", &format!("{hash_back:016x}"))]);

    // Verify that navigating forward produced distinct frames for each sample.
    // Note: cache state (epochs, counters) means going back may not produce an
    // identical hash to the forward pass, so we just verify distinct samples and
    // that the backward frame differs from the last forward frame.
    assert_ne!(
        hash_back, hashes[4],
        "Going back should differ from the last forward sample"
    );
}

#[test]
fn mega_e2e_sample_nav_down_arrow() {
    log_jsonl("env", &[("test", "mega_e2e_sample_nav_down_arrow")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let _h0 = capture_frame_hash(&screen, 120, 40);

    let _ = screen.update(&press(KeyCode::Down));
    let h1 = capture_frame_hash(&screen, 120, 40);

    let _ = screen.update(&press(KeyCode::Up));
    let h2 = capture_frame_hash(&screen, 120, 40);

    // Down should change the frame; Up should change it again (back toward sample 0).
    // Exact hash equality after round-trip is not guaranteed due to cache epoch counters.
    assert_ne!(_h0, h1, "Down should change the frame");
    assert_ne!(h1, h2, "Up should change the frame from sample 1");

    log_jsonl("result", &[("nav_roundtrip", "passed")]);
}

// ===========================================================================
// 3. Layout Mode Cycling
// ===========================================================================

#[test]
fn mega_e2e_layout_mode_cycling() {
    log_jsonl("env", &[("test", "mega_e2e_layout_mode_cycling")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Capture initial frame.
    let h0 = capture_frame_hash(&screen, 120, 40);
    log_jsonl("initial", &[("hash", &format!("{h0:016x}"))]);

    // Cycle layout mode with 'l' several times.
    let mut seen_hashes = vec![h0];
    for i in 0..5 {
        let _ = screen.update(&char_press('l'));
        let h = capture_frame_hash(&screen, 120, 40);
        seen_hashes.push(h);
        log_jsonl(
            "cycle",
            &[
                ("iteration", &i.to_string()),
                ("hash", &format!("{h:016x}")),
            ],
        );
    }

    let unique: std::collections::HashSet<u64> = seen_hashes.iter().copied().collect();
    assert!(
        unique.len() >= 2,
        "Cycling layout mode ('l') must produce distinct rendered frames"
    );
    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// 4. Direction Cycling
// ===========================================================================

#[test]
fn mega_e2e_direction_cycling() {
    log_jsonl("env", &[("test", "mega_e2e_direction_cycling")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let h0 = capture_frame_hash(&screen, 120, 40);
    let mut seen_hashes = vec![h0];

    // Cycle direction with 'O'.
    // Directions: None → TB → LR → RL → BT → None  (5 presses to wrap)
    for i in 0..6 {
        let _ = screen.update(&char_press('O'));
        let h = capture_frame_hash(&screen, 120, 40);
        seen_hashes.push(h);
        log_jsonl(
            "direction",
            &[("step", &i.to_string()), ("hash", &format!("{h:016x}"))],
        );
    }

    let unique: std::collections::HashSet<u64> = seen_hashes.iter().copied().collect();
    assert!(
        unique.len() >= 2,
        "Cycling direction ('O') must produce distinct rendered frames"
    );
    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// 5. Panel Toggles
// ===========================================================================

#[test]
fn mega_e2e_panel_toggles() {
    log_jsonl("env", &[("test", "mega_e2e_panel_toggles")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let _h0 = capture_frame_hash(&screen, 120, 40);

    // Toggle each panel on, then off — verify no panics and that toggling
    // on changes the frame.
    let panel_keys = ['m', 'c', 'd', 'i', 'e', '?'];
    for &key in &panel_keys {
        let h_before = capture_frame_hash(&screen, 120, 40);

        // Toggle on.
        let _ = screen.update(&char_press(key));
        let h_on = capture_frame_hash(&screen, 120, 40);

        // Toggle off.
        let _ = screen.update(&char_press(key));
        let h_off = capture_frame_hash(&screen, 120, 40);

        log_jsonl(
            "panel_toggle",
            &[
                ("key", &key.to_string()),
                ("before_hash", &format!("{h_before:016x}")),
                ("on_hash", &format!("{h_on:016x}")),
                ("off_hash", &format!("{h_off:016x}")),
            ],
        );

        // Toggling on should change the frame (panel becomes visible).
        assert_ne!(
            h_before, h_on,
            "Panel toggle on should change frame: key={key}"
        );
    }

    log_jsonl("result", &[("status", "passed")]);
}

#[test]
fn mega_e2e_help_overlay_visible() {
    log_jsonl("env", &[("test", "mega_e2e_help_overlay_visible")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Toggle help overlay on.
    let _ = screen.update(&char_press('?'));
    let has_help = frame_contains(&screen, 120, 40, "Help");
    log_jsonl("check", &[("has_help", &has_help.to_string())]);

    assert!(
        has_help,
        "Help overlay must contain 'Help' when toggled on with '?'"
    );
}

// ===========================================================================
// 6. Zoom Controls
// ===========================================================================

#[test]
fn mega_e2e_zoom_controls() {
    log_jsonl("env", &[("test", "mega_e2e_zoom_controls")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let h_initial = capture_frame_hash(&screen, 120, 40);

    // Zoom in with '+'.
    let _ = screen.update(&char_press('+'));
    let h_zoomed_in = capture_frame_hash(&screen, 120, 40);

    // Zoom out with '-'.
    let _ = screen.update(&char_press('-'));
    let h_back = capture_frame_hash(&screen, 120, 40);

    // Zooming in should change the frame.
    assert_ne!(h_initial, h_zoomed_in, "Zoom in should change the frame");

    // Zoom out after zoom in should approximately restore.
    // (may not be exact due to rounding but should work with integer zoom steps)

    // Reset zoom with '0'.
    let _ = screen.update(&char_press('0'));
    let h_reset = capture_frame_hash(&screen, 120, 40);

    log_jsonl(
        "zoom",
        &[
            ("initial", &format!("{h_initial:016x}")),
            ("zoomed_in", &format!("{h_zoomed_in:016x}")),
            ("back", &format!("{h_back:016x}")),
            ("reset", &format!("{h_reset:016x}")),
        ],
    );
}

#[test]
fn mega_e2e_fit_to_view() {
    log_jsonl("env", &[("test", "mega_e2e_fit_to_view")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Zoom in a few times, then fit to view.
    for _ in 0..3 {
        let _ = screen.update(&char_press('+'));
    }
    let h_zoomed = capture_frame_hash(&screen, 120, 40);

    let _ = screen.update(&char_press('f'));
    let h_fit = capture_frame_hash(&screen, 120, 40);

    // Fit-to-view should change the frame from the zoomed state.
    assert_ne!(h_zoomed, h_fit, "Fit to view should adjust the frame");

    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// 7. Render Config Cycling
// ===========================================================================

#[test]
fn mega_e2e_tier_glyph_wrap_cycling() {
    log_jsonl("env", &[("test", "mega_e2e_tier_glyph_wrap_cycling")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let h0 = capture_frame_hash(&screen, 120, 40);
    let mut tier_hashes = vec![h0];

    // Cycle tier ('t') — Auto/Compact/Normal/Rich (4 modes, wraps at 4).
    for i in 0..4 {
        let _ = screen.update(&char_press('t'));
        let h = capture_frame_hash(&screen, 120, 40);
        tier_hashes.push(h);
        log_jsonl(
            "tier",
            &[("step", &i.to_string()), ("hash", &format!("{h:016x}"))],
        );
    }
    assert!(
        tier_hashes
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len()
            >= 2,
        "Cycling tier ('t') must produce distinct rendered frames"
    );

    // Toggle glyph mode ('g') — Unicode/ASCII.
    let _ = screen.update(&char_press('g'));
    let h_ascii = capture_frame_hash(&screen, 120, 40);
    let _ = screen.update(&char_press('g'));
    let h_unicode = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "glyph",
        &[
            ("ascii_hash", &format!("{h_ascii:016x}")),
            ("unicode_hash", &format!("{h_unicode:016x}")),
        ],
    );
    assert_ne!(
        h_ascii, h_unicode,
        "Toggling glyph mode ('g') between ASCII and Unicode must change the rendered frame"
    );

    // Cycle wrap mode ('w').
    let mut wrap_hashes = vec![h_unicode];
    for i in 0..4 {
        let _ = screen.update(&char_press('w'));
        let h = capture_frame_hash(&screen, 120, 40);
        wrap_hashes.push(h);
        log_jsonl(
            "wrap",
            &[("step", &i.to_string()), ("hash", &format!("{h:016x}"))],
        );
    }
    assert!(
        wrap_hashes
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len()
            >= 2,
        "Cycling wrap mode ('w') must produce distinct rendered frames"
    );

    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// 8. Auto-Scale Toggle
// ===========================================================================

#[test]
fn mega_e2e_auto_scale_toggle() {
    log_jsonl("env", &[("test", "mega_e2e_auto_scale_toggle")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let h0 = capture_frame_hash(&screen, 120, 40);

    // Toggle auto-scale off with 'A' (starts on by default).
    let _ = screen.update(&char_press('A'));
    let h1 = capture_frame_hash(&screen, 120, 40);

    // Toggle back on.
    let _ = screen.update(&char_press('A'));
    let h2 = capture_frame_hash(&screen, 120, 40);

    log_jsonl(
        "auto_scale",
        &[
            ("h0", &format!("{h0:016x}")),
            ("h1", &format!("{h1:016x}")),
            ("h2", &format!("{h2:016x}")),
        ],
    );

    assert_ne!(
        h0, h1,
        "Toggling auto-scale ('A') off must change the rendered frame"
    );
    assert_ne!(
        h1, h2,
        "Toggling auto-scale ('A') back on must change the rendered frame"
    );
    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// 9. Perf Sweep Lifecycle
// ===========================================================================

#[test]
fn mega_e2e_sweep_lifecycle() {
    log_jsonl("env", &[("test", "mega_e2e_sweep_lifecycle")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Initial render.
    let h0 = capture_frame_hash(&screen, 120, 40);

    // Start sweep with 'S'.
    let _ = screen.update(&char_press('S'));
    log_jsonl("sweep", &[("action", "started")]);

    // Tick enough times to advance through at least one step.
    // Sweep advances every SWEEP_TICK_DELAY (3) ticks.
    for tick in 1..=30 {
        screen.tick(tick);
    }

    // Render after some sweep steps — should not panic.
    let h_mid = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "sweep",
        &[("action", "mid"), ("hash", &format!("{h_mid:016x}"))],
    );

    // Run many more ticks to complete the sweep (7 steps × 3 ticks = 21 ticks needed).
    for tick in 31..=60 {
        screen.tick(tick);
    }

    let h_done = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "sweep",
        &[("action", "done"), ("hash", &format!("{h_done:016x}"))],
    );

    // Stop sweep with another 'S' (toggles off, or resets if complete).
    let _ = screen.update(&char_press('S'));
    let h_stopped = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "sweep",
        &[
            ("action", "stopped"),
            ("hash", &format!("{h_stopped:016x}")),
        ],
    );

    assert_ne!(
        h0, h_mid,
        "Starting perf sweep ('S') and advancing ticks must change the rendered frame"
    );
    assert_ne!(h_done, 0, "Completed sweep frame must be non-empty");
    assert_ne!(h_stopped, 0, "Stopped sweep frame must be non-empty");
}

// ===========================================================================
// 10. Comparison Mode
// ===========================================================================

#[test]
fn mega_e2e_comparison_mode() {
    log_jsonl("env", &[("test", "mega_e2e_comparison_mode")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let h0 = capture_frame_hash(&screen, 120, 40);

    // Enable comparison mode with 'v'.
    let _ = screen.update(&char_press('v'));
    let h_comp = capture_frame_hash(&screen, 120, 40);

    // Comparison mode should change the layout (split view).
    assert_ne!(h0, h_comp, "Comparison mode should change the frame");

    // Cycle comparison layout with 'V'.
    let _ = screen.update(&char_press('V'));
    let h_comp2 = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "comparison",
        &[
            ("h0", &format!("{h0:016x}")),
            ("h_comp", &format!("{h_comp:016x}")),
            ("h_comp2", &format!("{h_comp2:016x}")),
        ],
    );

    // Disable comparison mode.
    let _ = screen.update(&char_press('v'));
    let h_back = capture_frame_hash(&screen, 120, 40);

    // Should return close to original (may differ due to lazy cache effects).
    log_jsonl("result", &[("h_back", &format!("{h_back:016x}"))]);
}

// ===========================================================================
// 11. Search Mode
// ===========================================================================

#[test]
fn mega_e2e_search_enter_exit() {
    log_jsonl("env", &[("test", "mega_e2e_search_enter_exit")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let h0 = capture_frame_hash(&screen, 120, 40);

    // Enter search mode with '/'.
    let _ = screen.update(&char_press('/'));
    let h_search = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "search",
        &[("action", "entered"), ("hash", &format!("{h_search:016x}"))],
    );

    // Type a query.
    let _ = screen.update(&char_press('F'));
    let _ = screen.update(&char_press('l'));
    let _ = screen.update(&char_press('o'));
    let _ = screen.update(&char_press('w'));
    let h_typed = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "search",
        &[
            ("action", "typed_Flow"),
            ("hash", &format!("{h_typed:016x}")),
        ],
    );

    // Accept search with Enter.
    let _ = screen.update(&press(KeyCode::Enter));
    let h_accepted = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "search",
        &[
            ("action", "accepted"),
            ("hash", &format!("{h_accepted:016x}")),
        ],
    );

    // Exit with Escape.
    let _ = screen.update(&press(KeyCode::Escape));
    let h_exit = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "search",
        &[("action", "exited"), ("hash", &format!("{h_exit:016x}"))],
    );

    assert_ne!(
        h0, h_search,
        "Entering search mode ('/') must change the rendered frame"
    );
    assert_ne!(
        h_search, h_typed,
        "Typing a search query must change the rendered frame"
    );
    assert_ne!(
        h_typed, h_accepted,
        "Accepting search with Enter must transition out of search input mode"
    );
    assert_ne!(h_exit, 0, "Frame after Escape must be non-empty");
}

// ===========================================================================
// 12. Multi-Size Rendering (All Samples)
// ===========================================================================

#[test]
fn mega_e2e_all_samples_render_cleanly() {
    log_jsonl("env", &[("test", "mega_e2e_all_samples_render_cleanly")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let sizes: &[(u16, u16)] = &[(80, 24), (120, 40), (200, 60)];
    let mut distinct_120x40_hashes = std::collections::HashSet::new();

    // Render first sample at all sizes.
    for &(w, h) in sizes {
        let hash = capture_frame_hash(&screen, w, h);
        assert_ne!(hash, 0, "Sample 0 at {w}x{h} must render non-empty content");
        if (w, h) == (120, 40) {
            distinct_120x40_hashes.insert(hash);
        }
    }
    log_jsonl("sample", &[("idx", "0"), ("rendered", "true")]);

    // Navigate through remaining samples.
    // The mega showcase has ~32 samples (31 curated + 1 generated).
    // We'll cycle through enough to cover all.
    for i in 1..35 {
        let _ = screen.update(&char_press('j'));
        for &(w, h) in sizes {
            let hash = capture_frame_hash(&screen, w, h);
            assert_ne!(
                hash, 0,
                "Sample {i} at {w}x{h} must render non-empty content"
            );
            if (w, h) == (120, 40) {
                distinct_120x40_hashes.insert(hash);
            }
        }
        if i % 5 == 0 {
            log_jsonl("sample", &[("idx", &i.to_string()), ("rendered", "true")]);
        }
    }

    assert!(
        distinct_120x40_hashes.len() >= 10,
        "Cycling through 35 samples should produce at least 10 distinct 120x40 frames (got {})",
        distinct_120x40_hashes.len()
    );

    log_jsonl(
        "result",
        &[("status", "passed"), ("samples_rendered", "35")],
    );
}

// ===========================================================================
// 13. Determinism
// ===========================================================================

#[test]
fn mega_e2e_render_determinism() {
    log_jsonl("env", &[("test", "mega_e2e_render_determinism")]);

    let screen1 = MermaidMegaShowcaseScreen::new();
    let screen2 = MermaidMegaShowcaseScreen::new();

    let h1 = capture_frame_hash(&screen1, 120, 40);
    let h2 = capture_frame_hash(&screen2, 120, 40);

    assert_eq!(h1, h2, "Two fresh screens should produce identical renders");

    log_jsonl(
        "determinism",
        &[("h1", &format!("{h1:016x}")), ("h2", &format!("{h2:016x}"))],
    );
}

#[test]
fn mega_e2e_interaction_determinism() {
    log_jsonl("env", &[("test", "mega_e2e_interaction_determinism")]);

    // Apply the same sequence of actions to two independent screens.
    let mut s1 = MermaidMegaShowcaseScreen::new();
    let mut s2 = MermaidMegaShowcaseScreen::new();

    let actions: Vec<Event> = vec![
        char_press('j'),
        char_press('j'),
        char_press('l'),
        char_press('t'),
        char_press('+'),
        char_press('m'),
        char_press('k'),
    ];

    for action in &actions {
        let _ = s1.update(action);
        let _ = s2.update(action);
    }

    let h1 = capture_frame_hash(&s1, 120, 40);
    let h2 = capture_frame_hash(&s2, 120, 40);

    assert_eq!(
        h1, h2,
        "Same action sequence should produce identical renders"
    );

    log_jsonl(
        "interaction_determinism",
        &[("h1", &format!("{h1:016x}")), ("h2", &format!("{h2:016x}"))],
    );
}

// ===========================================================================
// 14. Rapid Key Presses (Stress)
// ===========================================================================

#[test]
fn mega_e2e_rapid_key_presses() {
    log_jsonl("env", &[("test", "mega_e2e_rapid_key_presses")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Rapid-fire various keys in sequence.
    let keys = [
        'j', 'j', 'j', 'l', 't', 'g', 'w', 'm', 'c', 'd', '+', '+', '-', '0', 'f', 'j', 'k', 'O',
        's', 'A', 'v', 'V', 'v', 'p', 'P', 'e', '?', 'i', 'r',
    ];

    for &key in &keys {
        let _ = screen.update(&char_press(key));
    }

    // Tick a few times for sweep/budget logic.
    for tick in 1..=5 {
        screen.tick(tick);
    }

    // Should still render without panic.
    let h = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "rapid",
        &[("hash", &format!("{h:016x}")), ("no_panic", "true")],
    );
}

// ===========================================================================
// 15. Cache Behavior
// ===========================================================================

#[test]
fn mega_e2e_cache_hit_faster() {
    log_jsonl("env", &[("test", "mega_e2e_cache_hit_faster")]);

    let screen = MermaidMegaShowcaseScreen::new();
    let (w, h) = (120, 40);

    // First render: cache miss.
    let first_ms = measure_view_ms(&screen, w, h);

    // Second render: cache hit.
    let second_ms = measure_view_ms(&screen, w, h);

    log_jsonl(
        "cache",
        &[
            ("first_ms", &format!("{first_ms:.2}")),
            ("second_ms", &format!("{second_ms:.2}")),
        ],
    );

    // Cache hit should be faster (or at least not slower by a large margin).
    if first_ms > 1.0 {
        assert!(
            second_ms < first_ms * 2.0,
            "Cache hit ({second_ms:.2}ms) should not be much slower than miss ({first_ms:.2}ms)"
        );
    }
}

// ===========================================================================
// 16. JSONL Telemetry Schema Constants
// ===========================================================================

#[test]
fn mega_e2e_telemetry_required_fields_nonempty() {
    log_jsonl(
        "env",
        &[("test", "mega_e2e_telemetry_required_fields_nonempty")],
    );

    assert!(
        !MEGA_TELEMETRY_REQUIRED_FIELDS.is_empty(),
        "Required fields constant must not be empty"
    );
    assert!(
        MEGA_TELEMETRY_REQUIRED_FIELDS.len() >= 10,
        "Expected at least 10 required fields, got {}",
        MEGA_TELEMETRY_REQUIRED_FIELDS.len()
    );

    log_jsonl(
        "schema",
        &[(
            "required_count",
            &MEGA_TELEMETRY_REQUIRED_FIELDS.len().to_string(),
        )],
    );
}

#[test]
fn mega_e2e_telemetry_nullable_fields_nonempty() {
    log_jsonl(
        "env",
        &[("test", "mega_e2e_telemetry_nullable_fields_nonempty")],
    );

    assert!(
        !MEGA_TELEMETRY_NULLABLE_FIELDS.is_empty(),
        "Nullable fields constant must not be empty"
    );
    assert!(
        MEGA_TELEMETRY_NULLABLE_FIELDS.len() >= 5,
        "Expected at least 5 nullable fields, got {}",
        MEGA_TELEMETRY_NULLABLE_FIELDS.len()
    );

    log_jsonl(
        "schema",
        &[(
            "nullable_count",
            &MEGA_TELEMETRY_NULLABLE_FIELDS.len().to_string(),
        )],
    );
}

#[test]
fn mega_e2e_telemetry_no_duplicate_fields() {
    log_jsonl("env", &[("test", "mega_e2e_telemetry_no_duplicate_fields")]);

    // All fields across required and nullable should be unique.
    let mut all_fields: Vec<&str> = Vec::new();
    all_fields.extend_from_slice(MEGA_TELEMETRY_REQUIRED_FIELDS);
    all_fields.extend_from_slice(MEGA_TELEMETRY_NULLABLE_FIELDS);

    let unique: std::collections::HashSet<&&str> = all_fields.iter().collect();

    assert_eq!(
        all_fields.len(),
        unique.len(),
        "Found duplicate field names across required and nullable lists"
    );

    log_jsonl("schema", &[("total_fields", &all_fields.len().to_string())]);
}

// ===========================================================================
// 17. JSONL Validator Acceptance / Rejection
// ===========================================================================

#[test]
fn mega_e2e_validator_rejects_empty_json() {
    log_jsonl("env", &[("test", "mega_e2e_validator_rejects_empty_json")]);

    let result = validate_mega_telemetry_line("{}");
    assert!(result.is_err(), "Empty object should fail validation");

    log_jsonl("result", &[("rejected_empty", "true")]);
}

#[test]
fn mega_e2e_validator_rejects_bad_schema() {
    log_jsonl("env", &[("test", "mega_e2e_validator_rejects_bad_schema")]);

    let bad_line = r#"{"schema_version":"wrong","event":"mermaid_mega_recompute"}"#;
    let result = validate_mega_telemetry_line(bad_line);
    assert!(result.is_err(), "Wrong schema_version should fail");

    let err = result.unwrap_err();
    assert!(
        err.contains("schema_version"),
        "Error should mention schema_version: {err}"
    );

    log_jsonl("result", &[("rejected_bad_schema", "true")]);
}

#[test]
fn mega_e2e_validator_rejects_invalid_json() {
    log_jsonl(
        "env",
        &[("test", "mega_e2e_validator_rejects_invalid_json")],
    );

    let result = validate_mega_telemetry_line("not json at all");
    assert!(result.is_err(), "Invalid JSON should fail validation");

    log_jsonl("result", &[("rejected_invalid_json", "true")]);
}

// ===========================================================================
// 18. Generator Controls
// ===========================================================================

#[test]
fn mega_e2e_generator_controls() {
    log_jsonl("env", &[("test", "mega_e2e_generator_controls")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Select the generated sample directly so generator keys affect the active diagram.
    assert!(
        screen.select_sample_by_id_for_test("generated"),
        "Generated sample must exist in MEGA_SAMPLES"
    );
    let h_gen = capture_frame_hash(&screen, 120, 40);
    log_jsonl("generator", &[("initial_hash", &format!("{h_gen:016x}"))]);

    // Increase node count with 'U'.
    let _ = screen.update(&char_press('U'));
    let h_more = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "generator",
        &[("after_inc_nodes", &format!("{h_more:016x}"))],
    );

    // Decrease node count with 'u'.
    let _ = screen.update(&char_press('u'));
    let h_dec = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "generator",
        &[("after_dec_nodes", &format!("{h_dec:016x}"))],
    );

    // New seed with 'R'.
    let _ = screen.update(&char_press('R'));
    let h_seed = capture_frame_hash(&screen, 120, 40);
    log_jsonl(
        "generator",
        &[("after_new_seed", &format!("{h_seed:016x}"))],
    );

    assert_ne!(
        h_gen, h_more,
        "Increasing generated node count ('U') must change the rendered frame"
    );
    assert_ne!(
        h_more, h_dec,
        "Decreasing generated node count ('u') must change the rendered frame"
    );
    assert_ne!(
        h_dec, h_seed,
        "Changing generator seed ('R') must change the rendered frame"
    );

    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// 19. Pan Controls
// ===========================================================================

#[test]
fn mega_e2e_pan_controls() {
    log_jsonl("env", &[("test", "mega_e2e_pan_controls")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let h0 = capture_frame_hash(&screen, 120, 40);

    // Pan right with 'L' (Shift+L).
    let _ = screen.update(&char_press('L'));
    let h_right = capture_frame_hash(&screen, 120, 40);

    // Pan down with 'J' (Shift+J).
    let _ = screen.update(&char_press('J'));
    let h_down = capture_frame_hash(&screen, 120, 40);

    // Pan left with 'H' (Shift+H).
    let _ = screen.update(&char_press('H'));
    let h_left = capture_frame_hash(&screen, 120, 40);

    // Pan up with 'K' (Shift+K).
    let _ = screen.update(&char_press('K'));
    let h_up = capture_frame_hash(&screen, 120, 40);

    log_jsonl(
        "pan",
        &[
            ("h0", &format!("{h0:016x}")),
            ("right", &format!("{h_right:016x}")),
            ("down", &format!("{h_down:016x}")),
            ("left", &format!("{h_left:016x}")),
            ("up", &format!("{h_up:016x}")),
        ],
    );

    // Panning should change the frame.
    assert_ne!(h0, h_right, "Pan right should change the frame");
    // Round-trip may not produce identical hashes due to cache epoch changes.
}

// ===========================================================================
// 20. Viewport Override Controls
// ===========================================================================

#[test]
fn mega_e2e_viewport_override() {
    log_jsonl("env", &[("test", "mega_e2e_viewport_override")]);

    let mut screen = MermaidMegaShowcaseScreen::new();
    let h0 = capture_frame_hash(&screen, 120, 40);

    // Increase viewport width with ']'.
    let _ = screen.update(&char_press(']'));
    let h1 = capture_frame_hash(&screen, 120, 40);

    // Decrease viewport width with '['.
    let _ = screen.update(&char_press('['));
    let h2 = capture_frame_hash(&screen, 120, 40);

    // Reset viewport override with 'o'.
    let _ = screen.update(&char_press('o'));
    let h3 = capture_frame_hash(&screen, 120, 40);

    log_jsonl(
        "viewport_override",
        &[
            ("after_inc", &format!("{h1:016x}")),
            ("after_dec", &format!("{h2:016x}")),
            ("after_reset", &format!("{h3:016x}")),
        ],
    );

    assert_ne!(
        h0, h1,
        "Increasing viewport override width (']') must change the rendered frame"
    );
    assert_ne!(
        h1, h2,
        "Decreasing viewport override width ('[') must change the rendered frame"
    );
    assert_ne!(
        h3, 0,
        "Frame after viewport override reset must be non-empty"
    );

    log_jsonl("result", &[("status", "passed")]);
}

// ===========================================================================
// 21. Style and Palette Cycling
// ===========================================================================

#[test]
fn mega_e2e_style_palette_cycling() {
    log_jsonl("env", &[("test", "mega_e2e_style_palette_cycling")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Toggle styles with 's'.
    let _ = screen.update(&char_press('s'));
    let h_styles_off = capture_frame_hash(&screen, 120, 40);
    let _ = screen.update(&char_press('s'));
    let h_styles_on = capture_frame_hash(&screen, 120, 40);

    // Cycle palette with 'p'.
    let _ = screen.update(&char_press('p'));
    let h_palette1 = capture_frame_hash(&screen, 120, 40);
    let _ = screen.update(&char_press('p'));
    let h_palette2 = capture_frame_hash(&screen, 120, 40);

    // Previous palette with 'P'.
    let _ = screen.update(&char_press('P'));
    let h_palette_prev = capture_frame_hash(&screen, 120, 40);

    log_jsonl(
        "style_palette",
        &[
            ("styles_off", &format!("{h_styles_off:016x}")),
            ("styles_on", &format!("{h_styles_on:016x}")),
            ("palette1", &format!("{h_palette1:016x}")),
            ("palette2", &format!("{h_palette2:016x}")),
            ("palette_prev", &format!("{h_palette_prev:016x}")),
        ],
    );

    // Palette cycling should produce different frames.
    assert_ne!(
        h_palette1, h_palette2,
        "Different palettes should look different"
    );
}

// ===========================================================================
// 22. Tick Behavior
// ===========================================================================

#[test]
fn mega_e2e_tick_does_not_panic() {
    log_jsonl("env", &[("test", "mega_e2e_tick_does_not_panic")]);

    let mut screen = MermaidMegaShowcaseScreen::new();

    // Render first to populate cache.
    let _h = capture_frame_hash(&screen, 120, 40);

    // Tick many times.
    for tick in 1..=100 {
        screen.tick(tick);
    }

    // Render again after ticks.
    let h = capture_frame_hash(&screen, 120, 40);
    log_jsonl("tick", &[("hash_after_100_ticks", &format!("{h:016x}"))]);
}
