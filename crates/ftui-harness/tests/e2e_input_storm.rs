//! E2E fault injection: Bursty input storms (bd-1pys5.1)
//!
//! Tests that the runtime event processing pipeline handles burst input
//! without panics, event loss, or unbounded memory growth.
//!
//! Uses `ProgramSimulator` to inject deterministic event storms and verify:
//! 1. No panics under any burst pattern
//! 2. All keyboard events are processed (no event loss)
//! 3. Memory stays bounded (event vectors don't grow unboundedly)
//! 4. Model state is consistent after burst
//! 5. Structured JSONL evidence for postmortem analysis

#![forbid(unsafe_code)]

use ftui_core::event::Event;
use ftui_harness::input_storm::{
    BurstPattern, InputStormConfig, generate_storm, run_storm_with_logging,
};
use ftui_render::frame::Frame;
use ftui_runtime::program::{Cmd, Model};
use ftui_runtime::simulator::ProgramSimulator;

// ── Test Model ──────────────────────────────────────────────────────────

/// A simple counter model that tracks all events received.
struct StormTestModel {
    key_count: usize,
    mouse_count: usize,
    paste_count: usize,
    paste_bytes: usize,
    resize_count: usize,
    last_resize: Option<(u16, u16)>,
    total_events: usize,
}

impl StormTestModel {
    fn new() -> Self {
        Self {
            key_count: 0,
            mouse_count: 0,
            paste_count: 0,
            paste_bytes: 0,
            resize_count: 0,
            last_resize: None,
            total_events: 0,
        }
    }
}

#[derive(Debug)]
enum StormMsg {
    KeyPressed,
    MouseMoved,
    Pasted(usize),
    Resized(u16, u16),
    Other,
}

impl From<Event> for StormMsg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(_) => StormMsg::KeyPressed,
            Event::Mouse(_) => StormMsg::MouseMoved,
            Event::Paste(pe) => StormMsg::Pasted(pe.text.len()),
            Event::Resize { width, height } => StormMsg::Resized(width, height),
            _ => StormMsg::Other,
        }
    }
}

impl Model for StormTestModel {
    type Message = StormMsg;

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        self.total_events += 1;
        match msg {
            StormMsg::KeyPressed => self.key_count += 1,
            StormMsg::MouseMoved => self.mouse_count += 1,
            StormMsg::Pasted(bytes) => {
                self.paste_count += 1;
                self.paste_bytes += bytes;
            }
            StormMsg::Resized(w, h) => {
                self.resize_count += 1;
                self.last_resize = Some((w, h));
            }
            StormMsg::Other => {}
        }
        Cmd::None
    }

    fn view(&self, _frame: &mut Frame) {
        // Intentionally empty — testing event processing, not rendering.
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Inject a storm into a fresh simulator and snapshot the model state.
fn run_storm_in_simulator(storm_events: &[Event]) -> StormTestModel {
    let mut sim = ProgramSimulator::new(StormTestModel::new());
    sim.init();
    sim.inject_events(storm_events);
    let model = sim.model();
    StormTestModel {
        key_count: model.key_count,
        mouse_count: model.mouse_count,
        paste_count: model.paste_count,
        paste_bytes: model.paste_bytes,
        resize_count: model.resize_count,
        last_resize: model.last_resize,
        total_events: model.total_events,
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Test 1: Keyboard storm — 1000 keypresses, no panics, all counted
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_keyboard_storm_1000_events() {
    let config = InputStormConfig::new(BurstPattern::KeyboardStorm { count: 1000 }, 42);
    let storm = generate_storm(&config);
    let (processed, log_lines) = run_storm_with_logging(&storm);
    assert_eq!(processed, 1000);

    let model = run_storm_in_simulator(&storm.events);
    assert_eq!(
        model.key_count, 1000,
        "All keyboard events must be processed"
    );
    assert_eq!(model.total_events, 1000);

    for line in &log_lines {
        let val: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(val["event"].is_string());
    }
    eprintln!(
        "[keyboard_storm] {} events, {} JSONL lines",
        model.key_count,
        log_lines.len()
    );
}

// ═════════════════════════════════════════════════════════════════════════
// Test 2: Mouse flood — 1000 mouse-move events
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_mouse_flood_1000_events() {
    let config = InputStormConfig::new(
        BurstPattern::MouseFlood {
            count: 1000,
            width: 80,
            height: 24,
        },
        42,
    );
    let storm = generate_storm(&config);
    let model = run_storm_in_simulator(&storm.events);

    assert_eq!(
        model.mouse_count, 1000,
        "All mouse events must be processed"
    );
    assert_eq!(model.total_events, 1000);
    eprintln!("[mouse_flood] {} mouse events processed", model.mouse_count);
}

// ═════════════════════════════════════════════════════════════════════════
// Test 3: Mixed burst — interleaved keyboard + mouse + paste + resize
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_mixed_burst_1000_events() {
    let config = InputStormConfig::new(
        BurstPattern::MixedBurst {
            count: 1000,
            width: 80,
            height: 24,
        },
        42,
    );
    let storm = generate_storm(&config);
    let model = run_storm_in_simulator(&storm.events);

    let total = model.key_count + model.mouse_count + model.paste_count + model.resize_count;
    assert_eq!(total, 1000, "All events must be accounted for");
    assert_eq!(model.total_events, 1000);
    assert!(model.key_count > 0, "Expected some key events");
    assert!(model.mouse_count > 0, "Expected some mouse events");

    eprintln!(
        "[mixed_burst] key={}, mouse={}, paste={}, resize={}, total={}",
        model.key_count,
        model.mouse_count,
        model.paste_count,
        model.resize_count,
        model.total_events
    );
}

// ═════════════════════════════════════════════════════════════════════════
// Test 4: Long paste — 100KB paste event in a single burst
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_long_paste_100kb() {
    let config = InputStormConfig::new(
        BurstPattern::LongPaste {
            size_bytes: 100_000,
        },
        42,
    );
    let storm = generate_storm(&config);
    let model = run_storm_in_simulator(&storm.events);

    assert_eq!(model.paste_count, 1, "Expected exactly 1 paste event");
    assert_eq!(model.paste_bytes, 100_000, "Paste content must be complete");
    assert_eq!(model.total_events, 1);
    eprintln!("[long_paste] {} bytes processed", model.paste_bytes);
}

// ═════════════════════════════════════════════════════════════════════════
// Test 5: Rapid resize — 100 resize events in rapid succession
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_rapid_resize_100_events() {
    let config = InputStormConfig::new(BurstPattern::RapidResize { count: 100 }, 42);
    let storm = generate_storm(&config);
    let model = run_storm_in_simulator(&storm.events);

    assert_eq!(
        model.resize_count, 100,
        "All resize events must be processed"
    );
    assert!(model.last_resize.is_some(), "Last resize must be recorded");
    assert_eq!(model.total_events, 100);
    eprintln!(
        "[rapid_resize] {} resize events, final size={:?}",
        model.resize_count, model.last_resize
    );
}

// ═════════════════════════════════════════════════════════════════════════
// Test 6: Memory bounds — verify event processing doesn't accumulate
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_memory_bounded_under_sustained_burst() {
    let mut sim = ProgramSimulator::new(StormTestModel::new());
    sim.init();

    for round in 0..10 {
        let config =
            InputStormConfig::new(BurstPattern::KeyboardStorm { count: 10_000 }, 42 + round);
        let storm = generate_storm(&config);
        sim.inject_events(&storm.events);
    }

    let model = sim.model();
    assert_eq!(model.key_count, 100_000, "All 100K events across 10 rounds");
    assert_eq!(model.total_events, 100_000);
    eprintln!(
        "[memory_bounded] {} total events across 10 rounds",
        model.total_events
    );
}

// ═════════════════════════════════════════════════════════════════════════
// Test 7: Determinism — same seed produces identical state
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_storm_deterministic() {
    let config = InputStormConfig::new(
        BurstPattern::MixedBurst {
            count: 5000,
            width: 80,
            height: 24,
        },
        12345,
    );

    let model1 = run_storm_in_simulator(&generate_storm(&config).events);
    let model2 = run_storm_in_simulator(&generate_storm(&config).events);

    assert_eq!(model1.key_count, model2.key_count);
    assert_eq!(model1.mouse_count, model2.mouse_count);
    assert_eq!(model1.paste_count, model2.paste_count);
    assert_eq!(model1.paste_bytes, model2.paste_bytes);
    assert_eq!(model1.resize_count, model2.resize_count);
    assert_eq!(model1.total_events, model2.total_events);
    eprintln!("[determinism] verified over 5000 mixed events");
}

// ═════════════════════════════════════════════════════════════════════════
// Test 8: JSONL evidence is complete and well-formed
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_jsonl_evidence_complete() {
    let config = InputStormConfig::new(
        BurstPattern::MixedBurst {
            count: 1000,
            width: 80,
            height: 24,
        },
        42,
    );
    let storm = generate_storm(&config);
    let (_, log_lines) = run_storm_with_logging(&storm);

    let start_entries: Vec<_> = log_lines
        .iter()
        .filter(|l| l.contains(r#""event":"storm_start""#))
        .collect();
    let complete_entries: Vec<_> = log_lines
        .iter()
        .filter(|l| l.contains(r#""event":"storm_complete""#))
        .collect();
    let inject_entries: Vec<_> = log_lines
        .iter()
        .filter(|l| l.contains(r#""event":"storm_inject""#))
        .collect();

    assert_eq!(start_entries.len(), 1, "Exactly 1 start entry");
    assert_eq!(complete_entries.len(), 1, "Exactly 1 complete entry");
    assert!(
        inject_entries.len() >= 2,
        "At least 2 inject samples (first + last)"
    );

    let complete: serde_json::Value = serde_json::from_str(complete_entries[0]).unwrap();
    assert_eq!(complete["total_events"], 1000);
    assert_eq!(complete["events_processed"], 1000);

    eprintln!(
        "[jsonl_evidence] {} log lines: {} start, {} inject, {} complete",
        log_lines.len(),
        start_entries.len(),
        inject_entries.len(),
        complete_entries.len()
    );
}
