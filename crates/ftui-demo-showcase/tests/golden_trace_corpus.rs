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
use ftui_demo_showcase::app::{AppModel, ScreenId};
use ftui_demo_showcase::screens;
use ftui_runtime::render_trace::checksum_buffer;
use ftui_web::WebPatchRun;
use ftui_web::session_record::{SessionRecorder, replay};
use ftui_web::step_program::StepProgram;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

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
const E2E_SCHEMA_VERSION: &str = "e2e-jsonl-v1";
const HASH_ALGO: &str = "fnv1a64";
const FNV64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct WebSweepSignature {
    screen_id: String,
    cols: u16,
    rows: u16,
    frame_hash: String,
    patch_hash: String,
    patch_cells: u32,
    patch_runs: u32,
    patch_bytes: u64,
}

#[derive(Debug, Clone)]
struct WebSweepRecord {
    screen: ScreenId,
    frame_idx: u64,
    signature: WebSweepSignature,
    jsonl_line: String,
}

#[derive(Debug, Clone)]
struct WebSweepSoak {
    records: Vec<WebSweepRecord>,
    cycle_pool_lens: Vec<usize>,
    max_pool_len: usize,
}

fn screen_slug(screen: ScreenId) -> String {
    screen
        .title()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
}

fn fnv1a64_extend(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV64_PRIME);
    }
    hash
}

fn patch_hash(patches: &[WebPatchRun]) -> String {
    let mut hash = FNV64_OFFSET_BASIS;
    for patch in patches {
        hash = fnv1a64_extend(hash, &patch.offset.to_le_bytes());
        let cell_count = u32::try_from(patch.cells.len()).unwrap_or(u32::MAX);
        hash = fnv1a64_extend(hash, &cell_count.to_le_bytes());
        for cell in &patch.cells {
            hash = fnv1a64_extend(hash, &cell.bg.to_le_bytes());
            hash = fnv1a64_extend(hash, &cell.fg.to_le_bytes());
            hash = fnv1a64_extend(hash, &cell.glyph.to_le_bytes());
            hash = fnv1a64_extend(hash, &cell.attrs.to_le_bytes());
        }
    }
    format!("{HASH_ALGO}:{hash:016x}")
}

fn value_matches_schema_type(value: &Value, expected: &Value) -> bool {
    if let Some(options) = expected.as_array() {
        return options
            .iter()
            .any(|option| value_matches_schema_type(value, option));
    }
    match expected.as_str() {
        Some("string") => value.is_string(),
        Some("number") => value.is_number(),
        Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        _ => false,
    }
}

fn validate_frame_jsonl_schema(lines: &[String]) {
    let schema: Value =
        serde_json::from_str(include_str!("../../../tests/e2e/lib/e2e_jsonl_schema.json")).unwrap();
    let common_required = schema["common_required"].as_array().unwrap();
    let common_types = schema["common_types"].as_object().unwrap();
    let frame_schema = schema["events"]["frame"].as_object().unwrap();
    let frame_required = frame_schema["required"].as_array().unwrap();
    let frame_types = frame_schema["types"].as_object().unwrap();

    for (line_idx, line) in lines.iter().enumerate() {
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("invalid jsonl line {line_idx}: {err}"));
        let obj = value
            .as_object()
            .unwrap_or_else(|| panic!("line {line_idx} is not a JSON object"));
        assert_eq!(
            obj.get("type").and_then(Value::as_str),
            Some("frame"),
            "line {line_idx} is not frame type"
        );
        for field in common_required {
            let key = field.as_str().unwrap();
            assert!(
                obj.contains_key(key),
                "line {line_idx} missing common required field {key}"
            );
        }
        for field in frame_required {
            let key = field.as_str().unwrap();
            assert!(
                obj.contains_key(key),
                "line {line_idx} missing frame required field {key}"
            );
        }
        for (field, expected_type) in common_types {
            if let Some(actual) = obj.get(field) {
                assert!(
                    value_matches_schema_type(actual, expected_type),
                    "line {line_idx} common type mismatch for {field}: expected {expected_type:?}, got {actual:?}"
                );
            }
        }
        for (field, expected_type) in frame_types {
            if let Some(actual) = obj.get(field) {
                assert!(
                    value_matches_schema_type(actual, expected_type),
                    "line {line_idx} frame type mismatch for {field}: expected {expected_type:?}, got {actual:?}"
                );
            }
        }
    }
}

fn build_repro_trace(cols: u16, rows: u16, screen: ScreenId) -> String {
    let mut recorder = SessionRecorder::new(AppModel::new(), cols, rows, SEED);
    recorder.init().unwrap();
    recorder.program_mut().model_mut().current_screen = screen;
    let ts_ns = TICK_MS * 1_000_000;
    recorder.push_event(ts_ns, tick_event());
    recorder.advance_time(ts_ns, Duration::from_millis(TICK_MS));
    recorder.step().unwrap();
    recorder.finish().to_jsonl()
}

fn apply_web_sweep_deterministic_profile(program: &mut StepProgram<AppModel>, screen: ScreenId) {
    match screen {
        ScreenId::MermaidShowcase => {
            program
                .model_mut()
                .screens
                .mermaid_showcase
                .stabilize_metrics_for_snapshot();
            // Hide timing-heavy panel output for deterministic hash sweeps.
            program.push_event(key(KeyCode::Char('m')));
        }
        ScreenId::MermaidMegaShowcase => {
            // Collapse volatile side panels for deterministic hash sweeps.
            program.push_event(key(KeyCode::Escape));
        }
        _ => {}
    }
}

fn run_web_sweep(cols: u16, rows: u16, dpr: f32) -> Vec<WebSweepRecord> {
    let mut program = StepProgram::new(AppModel::new(), cols, rows);
    program.init().unwrap();

    let mut records = Vec::new();
    let mut ts_ms = 0_u64;

    for &screen in screens::screen_ids().iter() {
        ts_ms = ts_ms.saturating_add(TICK_MS);
        program.model_mut().current_screen = screen;
        apply_web_sweep_deterministic_profile(&mut program, screen);
        program.push_event(tick_event());
        program.advance_time(Duration::from_millis(TICK_MS));

        let start = Instant::now();
        let step = program.step().unwrap();
        let render_ms = start.elapsed().as_secs_f64() * 1_000.0;
        assert!(
            step.rendered,
            "screen sweep step should render for {}",
            screen.title()
        );

        let outputs = program.outputs();
        let buffer = outputs
            .last_buffer
            .as_ref()
            .expect("rendered step must capture last buffer");
        let frame_hash = format!(
            "{HASH_ALGO}:{:016x}",
            checksum_buffer(buffer, program.pool())
        );
        let patch_hash = patch_hash(&outputs.last_patches);
        let patch_stats = outputs
            .last_patch_stats
            .expect("rendered step must capture patch stats");
        let screen_id = screen_slug(screen);
        let hash_key = format!("web-{cols}x{rows}-seed{SEED}-{screen_id}");

        let jsonl_line = serde_json::to_string(&json!({
            "schema_version": E2E_SCHEMA_VERSION,
            "type": "frame",
            "timestamp": format!("t+{ts_ms}ms"),
            "run_id": format!("web_demo_sweep_{cols}x{rows}_seed{SEED}"),
            "seed": SEED,
            "frame_idx": step.frame_idx,
            "hash_algo": HASH_ALGO,
            "frame_hash": frame_hash,
            "patch_hash": patch_hash,
            "patch_bytes": patch_stats.bytes_uploaded,
            "patch_cells": patch_stats.dirty_cells,
            "patch_runs": patch_stats.patch_count,
            "render_ms": render_ms,
            "present_ms": 0.0,
            "present_bytes": patch_stats.bytes_uploaded,
            "mode": "web",
            "hash_key": hash_key,
            "cols": cols,
            "rows": rows,
            "screen_id": screen_id,
            "dpr": dpr,
        }))
        .unwrap();

        records.push(WebSweepRecord {
            screen,
            frame_idx: step.frame_idx,
            signature: WebSweepSignature {
                screen_id: screen_slug(screen),
                cols,
                rows,
                frame_hash,
                patch_hash,
                patch_cells: patch_stats.dirty_cells,
                patch_runs: patch_stats.patch_count,
                patch_bytes: patch_stats.bytes_uploaded,
            },
            jsonl_line,
        });
    }

    records
}

fn run_web_sweep_soak(cols: u16, rows: u16, dpr: f32, cycles: usize) -> WebSweepSoak {
    let mut program = StepProgram::new(AppModel::new(), cols, rows);
    program.init().unwrap();

    let mut records = Vec::new();
    let mut ts_ms = 0_u64;
    let mut cycle_pool_lens = Vec::with_capacity(cycles);
    let mut max_pool_len = program.pool().len();

    for cycle in 0..cycles {
        for &screen in screens::screen_ids().iter() {
            ts_ms = ts_ms.saturating_add(TICK_MS);
            program.model_mut().current_screen = screen;
            if cycle == 0 {
                apply_web_sweep_deterministic_profile(&mut program, screen);
            }
            program.push_event(tick_event());
            program.advance_time(Duration::from_millis(TICK_MS));

            let start = Instant::now();
            let step = program.step().unwrap();
            let render_ms = start.elapsed().as_secs_f64() * 1_000.0;
            assert!(
                step.rendered,
                "screen soak step should render for {}",
                screen.title()
            );

            let outputs = program.outputs();
            let buffer = outputs
                .last_buffer
                .as_ref()
                .expect("rendered step must capture last buffer");
            let frame_hash = format!(
                "{HASH_ALGO}:{:016x}",
                checksum_buffer(buffer, program.pool())
            );
            let patch_hash = patch_hash(&outputs.last_patches);
            let patch_stats = outputs
                .last_patch_stats
                .expect("rendered step must capture patch stats");
            let screen_id = screen_slug(screen);
            let hash_key = format!("web-{cols}x{rows}-seed{SEED}-cycle{cycle}-{screen_id}");

            let jsonl_line = serde_json::to_string(&json!({
                "schema_version": E2E_SCHEMA_VERSION,
                "type": "frame",
                "timestamp": format!("t+{ts_ms}ms"),
                "run_id": format!("web_demo_soak_{cols}x{rows}_seed{SEED}"),
                "seed": SEED,
                "frame_idx": step.frame_idx,
                "hash_algo": HASH_ALGO,
                "frame_hash": frame_hash,
                "patch_hash": patch_hash,
                "patch_bytes": patch_stats.bytes_uploaded,
                "patch_cells": patch_stats.dirty_cells,
                "patch_runs": patch_stats.patch_count,
                "render_ms": render_ms,
                "present_ms": 0.0,
                "present_bytes": patch_stats.bytes_uploaded,
                "mode": "web",
                "hash_key": hash_key,
                "cols": cols,
                "rows": rows,
                "screen_id": screen_id,
                "dpr": dpr,
            }))
            .unwrap();

            records.push(WebSweepRecord {
                screen,
                frame_idx: step.frame_idx,
                signature: WebSweepSignature {
                    screen_id: screen_slug(screen),
                    cols,
                    rows,
                    frame_hash,
                    patch_hash,
                    patch_cells: patch_stats.dirty_cells,
                    patch_runs: patch_stats.patch_count,
                    patch_bytes: patch_stats.bytes_uploaded,
                },
                jsonl_line,
            });

            max_pool_len = max_pool_len.max(program.pool().len());
        }
        cycle_pool_lens.push(program.pool().len());
    }

    WebSweepSoak {
        records,
        cycle_pool_lens,
        max_pool_len,
    }
}

fn assert_web_sweep_deterministic(
    left: &[WebSweepRecord],
    right: &[WebSweepRecord],
    cols: u16,
    rows: u16,
) {
    assert_eq!(
        left.len(),
        right.len(),
        "sweep length mismatch for {cols}x{rows}"
    );
    for (lhs, rhs) in left.iter().zip(right) {
        if lhs.signature != rhs.signature {
            let repro = build_repro_trace(cols, rows, lhs.screen);
            panic!(
                "web sweep mismatch for screen={} frame_idx={} @ {}x{}:\nleft={:?}\nright={:?}\nrepro_trace_jsonl:\n{}",
                lhs.signature.screen_id,
                lhs.frame_idx,
                cols,
                rows,
                lhs.signature,
                rhs.signature,
                repro
            );
        }
    }
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

// ---------------------------------------------------------------------------
// Trace 8: Web demo sweep JSONL + determinism
// ---------------------------------------------------------------------------

/// Browser-style sweep over every demo screen using `ftui-web::StepProgram`.
///
/// Verifies:
/// - deterministic frame + patch hashes across repeated runs (same build),
/// - per-frame JSONL records include patch stats for web hosts,
/// - emitted records conform to shared `e2e-jsonl-v1` frame schema.
#[test]
fn golden_web_demo_sweep_jsonl_deterministic() {
    let sweep_a_small = run_web_sweep(80, 24, 1.0);
    let sweep_b_small = run_web_sweep(80, 24, 1.0);
    assert_web_sweep_deterministic(&sweep_a_small, &sweep_b_small, 80, 24);

    let sweep_a_large = run_web_sweep(120, 40, 2.0);
    let sweep_b_large = run_web_sweep(120, 40, 2.0);
    assert_web_sweep_deterministic(&sweep_a_large, &sweep_b_large, 120, 40);

    let jsonl_lines: Vec<String> = sweep_a_small
        .iter()
        .chain(&sweep_a_large)
        .map(|record| record.jsonl_line.clone())
        .collect();
    validate_frame_jsonl_schema(&jsonl_lines);
}

/// Longer deterministic soak over repeated screen sweeps.
///
/// Verifies:
/// - deterministic signatures across repeated soak runs,
/// - grapheme-pool usage stabilizes after warmup (no runaway growth trend).
#[test]
fn golden_web_demo_soak_pool_stability() {
    const SOAK_CYCLES: usize = 12;
    let soak_a = run_web_sweep_soak(120, 40, 2.0, SOAK_CYCLES);
    let soak_b = run_web_sweep_soak(120, 40, 2.0, SOAK_CYCLES);
    assert_web_sweep_deterministic(&soak_a.records, &soak_b.records, 120, 40);

    assert_eq!(soak_a.cycle_pool_lens.len(), SOAK_CYCLES);
    assert_eq!(
        soak_a.cycle_pool_lens, soak_b.cycle_pool_lens,
        "soak pool profile must be deterministic"
    );

    // Memory stability gate:
    // after two warmup cycles, the grapheme pool should not continue to grow materially.
    let warmup_pool = soak_a.cycle_pool_lens[1];
    let final_pool = *soak_a
        .cycle_pool_lens
        .last()
        .expect("at least one pool sample");
    assert!(
        final_pool <= warmup_pool.saturating_add(128),
        "pool drift too high after warmup: warmup={warmup_pool} final={final_pool} profile={:?}",
        soak_a.cycle_pool_lens
    );

    assert!(
        soak_a.max_pool_len <= final_pool.saturating_add(256),
        "pool peak too far above steady-state: peak={} final={} profile={:?}",
        soak_a.max_pool_len,
        final_pool,
        soak_a.cycle_pool_lens
    );
}
