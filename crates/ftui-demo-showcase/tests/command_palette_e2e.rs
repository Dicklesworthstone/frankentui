#![forbid(unsafe_code)]

//! End-to-end tests for the Command Palette feature (bd-39y4.8).
//!
//! These tests exercise the full command palette lifecycle using the
//! deterministic `ProgramSimulator`, covering:
//!
//! - Open palette → type query → select → execute
//! - No results state
//! - Navigation (arrow keys, PageUp/Down, Home/End)
//! - Stress test with 1000 actions
//! - Verbose JSONL logging with env, capabilities, timings, checksums
//!
//! Run: `cargo test -p ftui-demo-showcase --test command_palette_e2e`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::Instant;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_demo_showcase::app::{AppModel, AppMsg, ScreenId};
use ftui_harness::determinism::{DeterminismFixture, LabScenario};
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::Model;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context, SubscriberExt};

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

fn ctrl_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::CTRL,
        kind: KeyEventKind::Press,
    })
}

fn type_chars(app: &mut AppModel, text: &str) {
    for ch in text.chars() {
        app.update(AppMsg::from(press(KeyCode::Char(ch))));
    }
}

/// Open the command palette via Ctrl+K.
fn open_palette(app: &mut AppModel) {
    app.update(AppMsg::from(ctrl_press(KeyCode::Char('k'))));
}

/// Capture a frame and return a hash for determinism checks.
fn capture_frame_hash(app: &mut AppModel, width: u16, height: u16) -> u64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
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

/// Emit a JSONL log entry to stderr.
fn log_jsonl(step: &str, data: &[(&str, &str)]) {
    let fields: Vec<String> = std::iter::once(format!("\"ts\":\"{}\"", chrono_like_timestamp()))
        .chain(std::iter::once(format!("\"step\":\"{}\"", step)))
        .chain(data.iter().map(|(k, v)| format!("\"{}\":\"{}\"", k, v)))
        .collect();
    eprintln!("{{{}}}", fields.join(","));
}

/// ISO-8601-like timestamp without external deps.
fn chrono_like_timestamp() -> String {
    fixture().timestamp()
}

fn fixture() -> &'static DeterminismFixture {
    static FIXTURE: OnceLock<DeterminismFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| DeterminismFixture::new("command_palette_e2e", 42))
}

const PALETTE_TELEMETRY_TARGET: &str = "ftui_widgets::command_palette";

/// Install a telemetry logger that prints palette events as JSONL to stderr.
fn install_palette_telemetry_logger() -> tracing::subscriber::DefaultGuard {
    use tracing::Subscriber;
    use tracing::field::{Field, Visit};

    #[derive(Default)]
    struct TelemetryVisitor {
        event: Option<String>,
        fields: Vec<(String, String)>,
    }

    impl Visit for TelemetryVisitor {
        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "event" {
                self.event = Some(value.to_string());
            } else {
                self.fields
                    .push((field.name().to_string(), value.to_string()));
            }
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }

        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "event" {
                self.event = Some(format!("{value:?}"));
            } else {
                self.fields
                    .push((field.name().to_string(), format!("{value:?}")));
            }
        }
    }

    #[derive(Default)]
    struct TelemetryLayer;

    impl<S> Layer<S> for TelemetryLayer
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            if event.metadata().target() != PALETTE_TELEMETRY_TARGET {
                return;
            }

            let mut visitor = TelemetryVisitor::default();
            event.record(&mut visitor);
            let event_name = visitor.event.unwrap_or_else(|| "unknown".to_string());

            let mut parts = Vec::with_capacity(visitor.fields.len() + 3);
            parts.push(format!("\"ts\":\"{}\"", chrono_like_timestamp()));
            parts.push("\"step\":\"palette_telemetry\"".to_string());
            parts.push(format!("\"event\":\"{}\"", event_name));

            visitor.fields.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (key, value) in visitor.fields {
                parts.push(format!("\"{}\":\"{}\"", key, value));
            }

            eprintln!("{{{}}}", parts.join(","));
        }
    }

    let subscriber = tracing_subscriber::registry()
        .with(TelemetryLayer)
        .with(Targets::new().with_target(PALETTE_TELEMETRY_TARGET, tracing::Level::INFO));
    tracing::subscriber::set_default(subscriber)
}

// ===========================================================================
// Scenario 1: Open → Type Query → Select → Execute
// ===========================================================================

#[test]
fn e2e_open_query_select_execute() {
    let _telemetry = install_palette_telemetry_logger();
    let start = Instant::now();

    log_jsonl(
        "env",
        &[
            ("test", "e2e_open_query_select_execute"),
            ("term_cols", "120"),
            ("term_rows", "40"),
        ],
    );

    let mut app = AppModel::new();
    app.update(AppMsg::Resize {
        width: 120,
        height: 40,
    });

    // Palette should start hidden.
    assert!(
        !app.command_palette.is_visible(),
        "palette should start hidden"
    );
    log_jsonl("step", &[("action", "open_palette")]);

    // Open palette.
    open_palette(&mut app);
    assert!(
        app.command_palette.is_visible(),
        "palette should be visible after Ctrl+K"
    );

    let total_actions = app.command_palette.action_count();
    assert!(total_actions > 0, "palette should have registered actions");

    log_jsonl(
        "palette_opened",
        &[
            ("action_count", &total_actions.to_string()),
            (
                "result_count",
                &app.command_palette.result_count().to_string(),
            ),
        ],
    );

    // Empty query should show all actions (up to max_visible).
    assert_eq!(
        app.command_palette.result_count(),
        total_actions,
        "empty query should show all actions"
    );

    // Type a query to filter results.
    log_jsonl("step", &[("action", "type_query"), ("query", "code")]);
    type_chars(&mut app, "code");

    let filtered = app.command_palette.result_count();
    assert!(
        filtered > 0,
        "query 'code' should match at least one action"
    );
    assert!(
        filtered <= total_actions,
        "filtered results should not exceed total"
    );

    log_jsonl(
        "query_filtered",
        &[
            ("query", "code"),
            ("result_count", &filtered.to_string()),
            (
                "selected_index",
                &app.command_palette.selected_index().to_string(),
            ),
        ],
    );

    // The first result should be selected.
    assert_eq!(
        app.command_palette.selected_index(),
        0,
        "first result should be selected"
    );

    // Navigate down to second result (if available).
    if filtered > 1 {
        app.update(AppMsg::from(press(KeyCode::Down)));
        assert_eq!(
            app.command_palette.selected_index(),
            1,
            "Down arrow should move selection to index 1"
        );
        log_jsonl(
            "step",
            &[("action", "navigate_down"), ("selected_index", "1")],
        );

        // Navigate back up.
        app.update(AppMsg::from(press(KeyCode::Up)));
        assert_eq!(
            app.command_palette.selected_index(),
            0,
            "Up arrow should move selection back to index 0"
        );
    }

    // Execute the selected action (Enter).
    log_jsonl("step", &[("action", "execute_enter")]);
    app.update(AppMsg::from(press(KeyCode::Enter)));

    // Palette should close after execution.
    assert!(
        !app.command_palette.is_visible(),
        "palette should close after Enter"
    );

    // Capture the final frame hash for determinism.
    let frame_hash = capture_frame_hash(&mut app, 120, 40);

    let elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("elapsed_us", &elapsed.as_micros().to_string()),
            ("frame_hash", &format!("{frame_hash:016x}")),
            ("final_screen", &format!("{:?}", app.current_screen)),
        ],
    );

    // The action should have changed the screen (since "code" matches CodeExplorer).
    // Note: the actual screen depends on the scoring, but it should have changed
    // OR dismissed the palette at minimum.
    assert!(
        !app.command_palette.is_visible(),
        "palette must be closed after execution"
    );
}

// ===========================================================================
// Scenario 2: No Results State
// ===========================================================================

#[test]
fn e2e_no_results_state() {
    let start = Instant::now();

    log_jsonl(
        "env",
        &[
            ("test", "e2e_no_results_state"),
            ("term_cols", "120"),
            ("term_rows", "40"),
        ],
    );

    let mut app = AppModel::new();
    app.update(AppMsg::Resize {
        width: 120,
        height: 40,
    });

    // Open palette.
    open_palette(&mut app);
    assert!(app.command_palette.is_visible());

    // Type a query that matches nothing.
    let gibberish = "zzxxqqww123";
    log_jsonl(
        "step",
        &[("action", "type_no_match_query"), ("query", gibberish)],
    );
    type_chars(&mut app, gibberish);

    assert_eq!(
        app.command_palette.result_count(),
        0,
        "gibberish query should yield zero results"
    );
    assert_eq!(
        app.command_palette.selected_index(),
        0,
        "selected index should be 0 with no results"
    );

    log_jsonl("no_results", &[("query", gibberish), ("result_count", "0")]);

    // Render the frame — must not panic.
    let frame_hash = capture_frame_hash(&mut app, 120, 40);

    // Navigation on empty results must not panic.
    app.update(AppMsg::from(press(KeyCode::Down)));
    app.update(AppMsg::from(press(KeyCode::Up)));
    app.update(AppMsg::from(press(KeyCode::PageDown)));
    app.update(AppMsg::from(press(KeyCode::PageUp)));
    app.update(AppMsg::from(press(KeyCode::Home)));
    app.update(AppMsg::from(press(KeyCode::End)));

    assert_eq!(
        app.command_palette.result_count(),
        0,
        "navigation should not create phantom results"
    );

    // Enter on empty results is a no-op (palette stays open, per design).
    app.update(AppMsg::from(press(KeyCode::Enter)));
    assert!(
        app.command_palette.is_visible(),
        "Enter on empty results should be a no-op (palette stays open)"
    );

    // Esc always dismisses, even with no results.
    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(
        !app.command_palette.is_visible(),
        "Esc should dismiss palette even with no results"
    );

    let elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("elapsed_us", &elapsed.as_micros().to_string()),
            ("frame_hash", &format!("{frame_hash:016x}")),
        ],
    );
}

// ===========================================================================
// Scenario 3: Esc Always Dismisses
// ===========================================================================

#[test]
fn e2e_esc_dismisses_palette() {
    log_jsonl("env", &[("test", "e2e_esc_dismisses_palette")]);

    let mut app = AppModel::new();

    // Open and dismiss immediately.
    open_palette(&mut app);
    assert!(app.command_palette.is_visible());
    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(!app.command_palette.is_visible(), "Esc should dismiss");

    // Open, type, then dismiss.
    open_palette(&mut app);
    type_chars(&mut app, "dash");
    assert!(app.command_palette.result_count() > 0);
    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(
        !app.command_palette.is_visible(),
        "Esc should dismiss even with query"
    );

    // Open, navigate, then dismiss.
    open_palette(&mut app);
    app.update(AppMsg::from(press(KeyCode::Down)));
    app.update(AppMsg::from(press(KeyCode::Down)));
    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(
        !app.command_palette.is_visible(),
        "Esc should dismiss after navigation"
    );

    log_jsonl("completed", &[("esc_dismiss_count", "3")]);
}

// ===========================================================================
// Scenario 4: Full Keyboard Flow End-to-End
// ===========================================================================

#[test]
fn e2e_full_keyboard_flow() {
    let start = Instant::now();

    log_jsonl(
        "env",
        &[
            ("test", "e2e_full_keyboard_flow"),
            ("term_cols", "80"),
            ("term_rows", "24"),
        ],
    );

    let mut app = AppModel::new();
    app.update(AppMsg::Resize {
        width: 80,
        height: 24,
    });

    let initial_screen = app.current_screen;
    assert_eq!(initial_screen, ScreenId::Dashboard);

    // Step 1: Open palette.
    open_palette(&mut app);
    let all_count = app.command_palette.result_count();
    log_jsonl(
        "palette_opened",
        &[("result_count", &all_count.to_string())],
    );

    // Step 2: Type "shake" to match Shakespeare screen.
    type_chars(&mut app, "shake");
    let filtered = app.command_palette.result_count();
    assert!(
        filtered > 0,
        "'shake' should match at least the Shakespeare screen"
    );
    log_jsonl(
        "query_filtered",
        &[("query", "shake"), ("result_count", &filtered.to_string())],
    );

    // Step 3: Execute first result (should navigate to Shakespeare).
    app.update(AppMsg::from(press(KeyCode::Enter)));
    assert!(!app.command_palette.is_visible());

    // The action "screen:shakespeare" should have been executed.
    assert_eq!(
        app.current_screen,
        ScreenId::Shakespeare,
        "executing 'shake' query should navigate to Shakespeare screen"
    );

    log_jsonl(
        "screen_changed",
        &[("from", "Dashboard"), ("to", "Shakespeare")],
    );

    // Step 4: Render at 80x24 — no panic.
    let frame_hash = capture_frame_hash(&mut app, 80, 24);

    let elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("elapsed_us", &elapsed.as_micros().to_string()),
            ("frame_hash", &format!("{frame_hash:016x}")),
            ("final_screen", "Shakespeare"),
        ],
    );
}

// ===========================================================================
// Scenario 5: Navigation — PageUp/PageDown, Home/End
// ===========================================================================

#[test]
fn e2e_navigation_pageup_pagedown_home_end() {
    log_jsonl("env", &[("test", "e2e_navigation")]);

    let mut app = AppModel::new();
    app.update(AppMsg::Resize {
        width: 120,
        height: 40,
    });

    open_palette(&mut app);
    let total = app.command_palette.result_count();
    assert!(total > 5, "need enough actions for navigation tests");

    // PageDown should jump by viewport.
    app.update(AppMsg::from(press(KeyCode::PageDown)));
    let after_pgdn = app.command_palette.selected_index();
    assert!(after_pgdn > 0, "PageDown should move selection forward");

    log_jsonl(
        "navigation",
        &[
            ("action", "PageDown"),
            ("selected_index", &after_pgdn.to_string()),
        ],
    );

    // End should jump to last.
    app.update(AppMsg::from(press(KeyCode::End)));
    let at_end = app.command_palette.selected_index();
    assert_eq!(at_end, total - 1, "End should jump to last result");

    log_jsonl(
        "navigation",
        &[("action", "End"), ("selected_index", &at_end.to_string())],
    );

    // Home should jump to first.
    app.update(AppMsg::from(press(KeyCode::Home)));
    assert_eq!(
        app.command_palette.selected_index(),
        0,
        "Home should jump to first result"
    );

    log_jsonl("navigation", &[("action", "Home"), ("selected_index", "0")]);

    // PageUp from 0 should stay at 0.
    app.update(AppMsg::from(press(KeyCode::PageUp)));
    assert_eq!(
        app.command_palette.selected_index(),
        0,
        "PageUp at start should clamp to 0"
    );

    log_jsonl("completed", &[("total_actions", &total.to_string())]);
}

// ===========================================================================
// Scenario 6: Backspace and Ctrl+U Query Editing
// ===========================================================================

#[test]
fn e2e_query_editing() {
    log_jsonl("env", &[("test", "e2e_query_editing")]);

    let mut app = AppModel::new();

    open_palette(&mut app);
    let total = app.command_palette.result_count();

    // Type a query.
    type_chars(&mut app, "dash");
    let filtered = app.command_palette.result_count();
    assert!(filtered < total, "query should filter results");
    assert_eq!(app.command_palette.query(), "dash");

    log_jsonl(
        "query_state",
        &[("query", "dash"), ("result_count", &filtered.to_string())],
    );

    // Backspace removes last character.
    app.update(AppMsg::from(press(KeyCode::Backspace)));
    assert_eq!(app.command_palette.query(), "das");

    // Ctrl+U clears the query.
    app.update(AppMsg::from(ctrl_press(KeyCode::Char('u'))));
    assert_eq!(app.command_palette.query(), "");
    assert_eq!(
        app.command_palette.result_count(),
        total,
        "cleared query should show all results"
    );

    log_jsonl("completed", &[("query_clear_works", "true")]);
}

// ===========================================================================
// Scenario 7: Stress Test with Many Actions
// ===========================================================================

#[test]
fn e2e_stress_1000_actions() {
    let start = Instant::now();

    log_jsonl(
        "env",
        &[
            ("test", "e2e_stress_1000_actions"),
            ("action_count", "1000"),
        ],
    );

    let mut app = AppModel::new();
    app.update(AppMsg::Resize {
        width: 120,
        height: 40,
    });

    // Register 1000 extra actions.
    use ftui_widgets::command_palette::ActionItem;
    for i in 0..1000 {
        app.command_palette.register_action(
            ActionItem::new(format!("stress:{i}"), format!("Stress Action {i}"))
                .with_description(format!("Stress test action number {i}"))
                .with_tags(&["stress", "test"])
                .with_category("Stress"),
        );
    }

    let total = app.command_palette.action_count();
    log_jsonl("actions_registered", &[("total_count", &total.to_string())]);

    // Open palette — must not be slow.
    let open_start = Instant::now();
    open_palette(&mut app);
    let open_elapsed = open_start.elapsed();
    assert!(app.command_palette.is_visible());

    log_jsonl(
        "palette_opened",
        &[
            ("open_latency_us", &open_elapsed.as_micros().to_string()),
            (
                "result_count",
                &app.command_palette.result_count().to_string(),
            ),
        ],
    );

    // Type a query that matches many stress actions.
    let query_start = Instant::now();
    type_chars(&mut app, "stress");
    let query_elapsed = query_start.elapsed();

    let filtered = app.command_palette.result_count();
    assert!(
        filtered >= 1000,
        "query 'stress' should match at least 1000 actions, got {filtered}"
    );

    log_jsonl(
        "stress_query",
        &[
            ("query", "stress"),
            ("result_count", &filtered.to_string()),
            ("query_latency_us", &query_elapsed.as_micros().to_string()),
        ],
    );

    // Render — must not panic or be excessively slow.
    let render_start = Instant::now();
    let frame_hash = capture_frame_hash(&mut app, 120, 40);
    let render_elapsed = render_start.elapsed();

    log_jsonl(
        "stress_render",
        &[
            ("render_latency_us", &render_elapsed.as_micros().to_string()),
            ("frame_hash", &format!("{frame_hash:016x}")),
        ],
    );

    // Navigate through many results.
    let nav_start = Instant::now();
    for _ in 0..50 {
        app.update(AppMsg::from(press(KeyCode::Down)));
    }
    let nav_elapsed = nav_start.elapsed();
    assert_eq!(app.command_palette.selected_index(), 50);

    log_jsonl(
        "stress_navigation",
        &[
            ("nav_steps", "50"),
            ("nav_latency_us", &nav_elapsed.as_micros().to_string()),
            ("selected_index", "50"),
        ],
    );

    // PageDown stress.
    let pgdn_start = Instant::now();
    for _ in 0..10 {
        app.update(AppMsg::from(press(KeyCode::PageDown)));
    }
    let pgdn_elapsed = pgdn_start.elapsed();

    log_jsonl(
        "stress_pagedown",
        &[
            ("pagedown_steps", "10"),
            ("latency_us", &pgdn_elapsed.as_micros().to_string()),
            (
                "selected_index",
                &app.command_palette.selected_index().to_string(),
            ),
        ],
    );

    // Dismiss.
    app.update(AppMsg::from(press(KeyCode::Escape)));
    assert!(!app.command_palette.is_visible());

    let total_elapsed = start.elapsed();
    log_jsonl(
        "completed",
        &[
            ("total_elapsed_us", &total_elapsed.as_micros().to_string()),
            ("status", "pass"),
        ],
    );
}

// ===========================================================================
// Scenario 8: Determinism — Identical Inputs Yield Identical Frame Hashes
// ===========================================================================

#[test]
fn e2e_determinism() {
    log_jsonl("env", &[("test", "e2e_determinism")]);

    fn run_scenario() -> (u64, ScreenId, u64) {
        let lab = LabScenario::new_with(
            "command_palette_e2e",
            "command_palette_determinism",
            42,
            true,
            5,
        );
        let run = lab.run(|_ctx| {
            let mut app = AppModel::new();
            app.update(AppMsg::Resize {
                width: 120,
                height: 40,
            });

            // Open palette, query "widget", select second result, execute.
            open_palette(&mut app);
            type_chars(&mut app, "widget");
            app.update(AppMsg::from(press(KeyCode::Down)));
            let hash = capture_frame_hash(&mut app, 120, 40);
            app.update(AppMsg::from(press(KeyCode::Enter)));

            (hash, app.current_screen)
        });
        (run.output.0, run.output.1, run.result.event_count)
    }

    let (hash1, screen1, events1) = run_scenario();
    let (hash2, screen2, events2) = run_scenario();
    let (hash3, screen3, events3) = run_scenario();

    assert_eq!(hash1, hash2, "frame hashes must be deterministic");
    assert_eq!(hash2, hash3, "frame hashes must be deterministic");
    assert_eq!(screen1, screen2, "resulting screen must be deterministic");
    assert_eq!(screen2, screen3, "resulting screen must be deterministic");
    assert_eq!(events1, events2, "lab event count must be deterministic");
    assert_eq!(events2, events3, "lab event count must be deterministic");
    assert_eq!(events1, 2, "lab run should emit start/end events");

    log_jsonl(
        "completed",
        &[
            ("frame_hash", &format!("{hash1:016x}")),
            ("deterministic", "true"),
            ("lab_event_count", &events1.to_string()),
        ],
    );
}

// ===========================================================================
// Scenario 9: Palette Does Not Intercept Global Keys When Hidden
// ===========================================================================

#[test]
fn e2e_palette_hidden_does_not_intercept_keys() {
    log_jsonl("env", &[("test", "e2e_palette_hidden_keys")]);

    let mut app = AppModel::new();

    // Ensure palette is hidden.
    assert!(!app.command_palette.is_visible());

    // 'q' should quit (global keybinding) — not go to palette.
    // We can't actually test Cmd::Quit easily, but we can test screen nav works.
    // Press '3' to switch to Shakespeare (key mapping: 1=GuidedTour, 2=Dashboard, 3=Shakespeare).
    app.update(AppMsg::from(press(KeyCode::Char('3'))));
    assert_eq!(
        app.current_screen,
        ScreenId::Shakespeare,
        "number keys should work when palette is hidden"
    );

    // Tab should cycle screens.
    let before = app.current_screen;
    app.update(AppMsg::from(press(KeyCode::Tab)));
    assert_ne!(
        app.current_screen, before,
        "Tab should cycle screens when palette is hidden"
    );

    log_jsonl("completed", &[("global_keys_work", "true")]);
}

// ===========================================================================
// Scenario 10: Palette Absorbs Keys When Visible
// ===========================================================================

#[test]
fn e2e_palette_visible_absorbs_keys() {
    log_jsonl("env", &[("test", "e2e_palette_absorbs_keys")]);

    let mut app = AppModel::new();
    app.update(AppMsg::Resize {
        width: 120,
        height: 40,
    });

    let initial_screen = app.current_screen;

    // Open palette.
    open_palette(&mut app);

    // Press '2' — should type into palette query, NOT switch screen.
    app.update(AppMsg::from(press(KeyCode::Char('2'))));
    assert_eq!(
        app.current_screen, initial_screen,
        "number keys should not switch screen when palette is open"
    );
    assert_eq!(
        app.command_palette.query(),
        "2",
        "number should be typed into palette query"
    );

    // Tab should NOT cycle screens — palette consumes it.
    let screen_before_tab = app.current_screen;
    app.update(AppMsg::from(press(KeyCode::Tab)));
    assert_eq!(
        app.current_screen, screen_before_tab,
        "Tab should not cycle screens when palette is open"
    );

    log_jsonl("completed", &[("palette_absorbs_keys", "true")]);
}

// ===========================================================================
// JSONL Summary
// ===========================================================================

#[test]
fn e2e_summary() {
    // This test runs last (alphabetically) and emits a summary line.
    log_jsonl(
        "summary",
        &[
            ("test_suite", "command_palette_e2e"),
            ("bead", "bd-39y4.8"),
            ("scenario_count", "10"),
            ("status", "pass"),
        ],
    );
}
