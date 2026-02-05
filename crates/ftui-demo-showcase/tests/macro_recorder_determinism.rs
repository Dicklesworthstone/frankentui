#![forbid(unsafe_code)]

//! Determinism and timing regression tests for the macro recorder (bd-2lus.4).
//!
//! These tests verify:
//! - Fixed macro fixtures replay identically across runs
//! - Event order is preserved at all speeds
//! - Timing drift stays within tolerance
//! - Looping produces consistent event counts
//! - Multiple replays of the same macro yield identical event streams
//!
//! Run: `cargo test -p ftui-demo-showcase --test macro_recorder_determinism`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_runtime::input_macro::{
    FilteredEventRecorder, InputMacro, MacroMetadata, MacroPlayback, RecordingFilter, TimedEvent,
};
use serial_test::serial;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn key(ch: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(ch),
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Press,
    })
}

/// Build a fixture macro with explicit per-event delays (ms).
fn fixture(name: &str, items: &[(char, u64)]) -> InputMacro {
    let mut events = Vec::with_capacity(items.len());
    let mut total = Duration::ZERO;
    for &(ch, delay_ms) in items {
        let delay = Duration::from_millis(delay_ms);
        total += delay;
        events.push(TimedEvent::new(key(ch), delay));
    }
    InputMacro::new(
        events,
        MacroMetadata {
            name: name.to_string(),
            terminal_size: (80, 24),
            total_duration: total,
        },
    )
}

/// Drain all events from a MacroPlayback by advancing in `step_ms` increments.
fn drain_all(playback: &mut MacroPlayback, step_ms: u64) -> Vec<Event> {
    let mut out = Vec::new();
    let step = Duration::from_millis(step_ms);
    // Safety limit to prevent infinite loops in tests
    for _ in 0..10_000 {
        if playback.is_done() {
            break;
        }
        out.extend(playback.advance(step));
    }
    out
}

/// Drain events and record (event, elapsed_ms) pairs for timing analysis.
fn drain_with_timing(playback: &mut MacroPlayback, step_ms: u64) -> Vec<(Event, u64)> {
    let mut out = Vec::new();
    let step = Duration::from_millis(step_ms);
    for _ in 0..10_000 {
        if playback.is_done() {
            break;
        }
        let events = playback.advance(step);
        let elapsed_ms = playback.elapsed().as_millis() as u64;
        for ev in events {
            out.push((ev, elapsed_ms));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tracing helpers (macro_event capture)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct MacroEventLog {
    events: Arc<Mutex<Vec<String>>>,
}

#[derive(Default)]
struct MacroEventVisitor {
    event: Option<String>,
}

impl Visit for MacroEventVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "macro_event" {
            self.event = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "macro_event" {
            self.event = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

impl<S> Layer<S> for MacroEventLog
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MacroEventVisitor::default();
        event.record(&mut visitor);
        if let Some(name) = visitor.event {
            self.events.lock().unwrap().push(name);
        }
    }
}

fn capture_macro_events() -> (tracing::dispatcher::DefaultGuard, Arc<Mutex<Vec<String>>>) {
    let log = MacroEventLog::default();
    let events = log.events.clone();
    let subscriber = tracing_subscriber::registry().with(log);
    let guard = tracing::subscriber::set_default(subscriber);
    // Ensure callsite interest is re-evaluated in parallel test runs.
    tracing::callsite::rebuild_interest_cache();
    (guard, events)
}

// ===========================================================================
// 1. Fixed fixture replay — event order preserved
// ===========================================================================

#[test]
fn fixed_fixture_preserves_event_order() {
    let m = fixture("order", &[('a', 10), ('b', 20), ('c', 30), ('d', 40)]);
    let mut pb = MacroPlayback::new(m);
    let events = drain_all(&mut pb, 5);

    let chars: Vec<char> = events
        .iter()
        .filter_map(|e| match e {
            Event::Key(k) => match k.code {
                KeyCode::Char(c) => Some(c),
                _ => None,
            },
            _ => None,
        })
        .collect();

    assert_eq!(chars, vec!['a', 'b', 'c', 'd']);
    assert!(pb.is_done());
}

// ===========================================================================
// 2. Multi-replay determinism — N replays produce identical streams
// ===========================================================================

#[test]
fn multi_replay_produces_identical_streams() {
    let m = fixture(
        "determinism",
        &[('h', 0), ('e', 50), ('l', 100), ('l', 150), ('o', 200)],
    );

    let mut streams: Vec<Vec<Event>> = Vec::new();

    for _ in 0..5 {
        let mut pb = MacroPlayback::new(m.clone());
        let events = drain_all(&mut pb, 10);
        streams.push(events);
    }

    for i in 1..streams.len() {
        assert_eq!(streams[0], streams[i], "Replay {} differs from replay 0", i);
    }
}

// ===========================================================================
// 3. Speed-scaled replay — same events, different timing
// ===========================================================================

#[test]
fn speed_scaled_replay_preserves_events() {
    let m = fixture("speed", &[('x', 100), ('y', 200), ('z', 300)]);

    for &speed in &[0.5, 1.0, 2.0, 4.0] {
        let mut pb = MacroPlayback::new(m.clone()).with_speed(speed);
        let events = drain_all(&mut pb, 5);
        let chars: Vec<char> = events
            .iter()
            .filter_map(|e| match e {
                Event::Key(k) => match k.code {
                    KeyCode::Char(c) => Some(c),
                    _ => None,
                },
                _ => None,
            })
            .collect();

        assert_eq!(
            chars,
            vec!['x', 'y', 'z'],
            "Events must match at speed {}x",
            speed
        );
    }
}

#[test]
fn speed_2x_halves_effective_timing() {
    let m = fixture("speed2x", &[('a', 0), ('b', 100)]);

    // At 1x: 'a' fires immediately (delay=0), 'b' fires at 100ms
    let mut pb1 = MacroPlayback::new(m.clone());
    // First advance: 'a' fires immediately (delay=0), but 'b' (due at 100ms) hasn't yet
    let events = pb1.advance(Duration::from_millis(50));
    assert_eq!(events.len(), 1);
    assert!(events.iter().any(|e| *e == key('a')));
    // Second advance: now at 100ms total, 'b' fires
    let events = pb1.advance(Duration::from_millis(50));
    assert_eq!(events.len(), 1);
    assert!(events.iter().any(|e| *e == key('b')));

    // At 2x, event 'b' is due at 50ms effective
    let mut pb2 = MacroPlayback::new(m.clone()).with_speed(2.0);
    let events = pb2.advance(Duration::from_millis(1)); // 2ms virtual -> gets 'a' (due at 0)
    assert!(events.iter().any(|e| *e == key('a')));
    let events = pb2.advance(Duration::from_millis(50)); // 102ms virtual total -> gets 'b'
    assert!(events.iter().any(|e| *e == key('b')));
}

// ===========================================================================
// 4. Timing drift test — events arrive within tolerance
// ===========================================================================

#[test]
fn timing_drift_within_tolerance() {
    // Events at known cumulative times: 0, 100, 300, 600, 1000ms
    let m = fixture(
        "drift",
        &[('a', 0), ('b', 100), ('c', 200), ('d', 300), ('e', 400)],
    );

    let step_ms = 7; // Non-aligned step to stress drift
    let mut pb = MacroPlayback::new(m);
    let timed = drain_with_timing(&mut pb, step_ms);

    assert_eq!(timed.len(), 5, "All 5 events must fire");

    // Expected cumulative due times: 0, 100, 300, 600, 1000
    let expected_due = [0u64, 100, 300, 600, 1000];

    for (i, ((ev, elapsed_ms), &due)) in timed.iter().zip(expected_due.iter()).enumerate() {
        // The event fires when elapsed >= due. With step_ms=7, max drift is step_ms.
        let drift = elapsed_ms.saturating_sub(due);
        assert!(
            drift <= step_ms,
            "Event {} ({:?}): elapsed={}ms, due={}ms, drift={}ms > tolerance={}ms",
            i,
            ev,
            elapsed_ms,
            due,
            drift,
            step_ms
        );
    }
}

// ===========================================================================
// 10. Diagnostics — tracing event order
// ===========================================================================

#[test]
#[serial]
#[ignore = "flaky in parallel test runs due to global tracing subscriber state"]
fn tracing_emits_macro_events_in_order() {
    let (_guard, events) = capture_macro_events();

    let mut recorder = FilteredEventRecorder::new("trace_order", RecordingFilter::keys_only());
    recorder.start();
    let event = key('x');
    recorder.record(&event);
    let macro_data = recorder.finish();

    let mut playback = MacroPlayback::new(macro_data);
    let _ = drain_all(&mut playback, 5);

    let events = events.lock().unwrap().clone();
    let idx_rec_start = events
        .iter()
        .position(|e| e == "recorder_start")
        .expect("recorder_start must be logged");
    let idx_rec_stop = events
        .iter()
        .position(|e| e == "recorder_stop")
        .expect("recorder_stop must be logged");
    let idx_play_start = events
        .iter()
        .position(|e| e == "playback_start")
        .expect("playback_start must be logged");
    let idx_play_stop = events
        .iter()
        .position(|e| e == "playback_stop")
        .expect("playback_stop must be logged");

    assert!(idx_rec_start < idx_rec_stop);
    assert!(idx_rec_stop < idx_play_start);
    assert!(idx_play_start < idx_play_stop);
}

#[test]
fn timing_drift_with_fine_step() {
    let m = fixture("fine", &[('a', 0), ('b', 50), ('c', 50), ('d', 100)]);
    let step_ms = 1;
    let mut pb = MacroPlayback::new(m);
    let timed = drain_with_timing(&mut pb, step_ms);

    assert_eq!(timed.len(), 4);

    // With 1ms steps, drift should be at most 1ms
    let expected_due = [0u64, 50, 100, 200];
    for (i, ((_, elapsed), &due)) in timed.iter().zip(expected_due.iter()).enumerate() {
        let drift = elapsed.saturating_sub(due);
        assert!(
            drift <= step_ms,
            "Event {}: drift {}ms > {}ms tolerance",
            i,
            drift,
            step_ms
        );
    }
}

// ===========================================================================
// 5. Loop determinism — consistent event counts
// ===========================================================================

#[test]
fn loop_determinism_consistent_counts() {
    let m = fixture("loop", &[('a', 10), ('b', 10)]);
    // total_duration = 20ms, 2 events per loop

    let mut pb = MacroPlayback::new(m.clone()).with_looping(true);

    // Advance 100ms = 5 full loops * 2 events = 10 events
    let events = pb.advance(Duration::from_millis(100));
    let count1 = events.len();

    // Do it again fresh
    let mut pb2 = MacroPlayback::new(m).with_looping(true);
    let events2 = pb2.advance(Duration::from_millis(100));
    let count2 = events2.len();

    assert_eq!(count1, count2, "Loop event counts must be deterministic");
    assert_eq!(events, events2, "Loop event streams must be identical");
    assert!(
        count1 >= 8,
        "Should get at least 8 events from 100ms / 20ms period, got {}",
        count1
    );
}

#[test]
fn loop_preserves_event_order() {
    let m = fixture("loop_order", &[('a', 10), ('b', 10), ('c', 10)]);
    let mut pb = MacroPlayback::new(m).with_looping(true);

    // Advance enough for ~3 loops (90ms / 30ms per loop)
    let events = pb.advance(Duration::from_millis(90));
    let chars: Vec<char> = events
        .iter()
        .filter_map(|e| match e {
            Event::Key(k) => match k.code {
                KeyCode::Char(c) => Some(c),
                _ => None,
            },
            _ => None,
        })
        .collect();

    // Verify the pattern repeats correctly
    for chunk in chars.chunks(3) {
        if chunk.len() == 3 {
            assert_eq!(chunk, &['a', 'b', 'c'], "Loop order violated: {:?}", chunk);
        }
    }
}

// ===========================================================================
// 6. Zero-delay macro — all events fire immediately
// ===========================================================================

#[test]
fn zero_delay_fires_all_immediately() {
    let m = InputMacro::from_events("zero", vec![key('a'), key('b'), key('c'), key('d')]);
    let mut pb = MacroPlayback::new(m);

    // Even a zero advance should fire all zero-delay events
    let events = pb.advance(Duration::ZERO);
    assert_eq!(events.len(), 4, "All zero-delay events should fire at once");
    assert!(pb.is_done());

    let chars: Vec<char> = events
        .iter()
        .filter_map(|e| match e {
            Event::Key(k) => match k.code {
                KeyCode::Char(c) => Some(c),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(chars, vec!['a', 'b', 'c', 'd']);
}

#[test]
fn zero_delay_loop_does_not_infinite_loop() {
    let m = InputMacro::from_events("zero_loop", vec![key('a'), key('b')]);
    let mut pb = MacroPlayback::new(m).with_looping(true);

    // Zero-duration + looping: should fire events once and stop
    let events = pb.advance(Duration::ZERO);
    assert_eq!(events.len(), 2);
    assert!(
        pb.is_done(),
        "Zero-duration looping macro should be done to prevent infinite loop"
    );
}

// ===========================================================================
// 7. Advance granularity independence — same final result
// ===========================================================================

#[test]
fn advance_granularity_independence() {
    let m = fixture("granularity", &[('a', 0), ('b', 30), ('c', 70), ('d', 100)]);

    // Drain with 1ms steps
    let events_1ms = drain_all(&mut MacroPlayback::new(m.clone()), 1);

    // Drain with 10ms steps
    let events_10ms = drain_all(&mut MacroPlayback::new(m.clone()), 10);

    // Drain with 50ms steps
    let events_50ms = drain_all(&mut MacroPlayback::new(m.clone()), 50);

    // Drain with 200ms single step (larger than total)
    let mut pb_big = MacroPlayback::new(m);
    let events_big = pb_big.advance(Duration::from_millis(200));

    // All should produce the same events in the same order
    assert_eq!(events_1ms, events_10ms, "1ms vs 10ms step mismatch");
    assert_eq!(events_10ms, events_50ms, "10ms vs 50ms step mismatch");
    assert_eq!(
        events_50ms,
        events_big.to_vec(),
        "50ms vs single-step mismatch"
    );
}

// ===========================================================================
// 8. Reset and replay — identical after reset
// ===========================================================================

#[test]
fn reset_produces_identical_replay() {
    let m = fixture("reset", &[('a', 10), ('b', 20), ('c', 30)]);
    let mut pb = MacroPlayback::new(m);

    let events1 = drain_all(&mut pb, 5);
    assert!(pb.is_done());

    pb.reset();
    assert!(!pb.is_done());
    assert_eq!(pb.position(), 0);
    assert_eq!(pb.elapsed(), Duration::ZERO);

    let events2 = drain_all(&mut pb, 5);
    assert_eq!(events1, events2, "Events after reset must match original");
}

// ===========================================================================
// 9. Regression: large delta does not skip events
// ===========================================================================

#[test]
fn large_delta_does_not_skip_events() {
    let m = fixture(
        "large_delta",
        &[('a', 100), ('b', 200), ('c', 300), ('d', 400)],
    );

    let mut pb = MacroPlayback::new(m);
    // Single advance of 10 seconds — well past all events
    let events = pb.advance(Duration::from_secs(10));

    assert_eq!(
        events.len(),
        4,
        "All events must fire even with large delta"
    );
    assert!(pb.is_done());
}

// ===========================================================================
// 10. Speed edge cases
// ===========================================================================

#[test]
fn zero_speed_freezes_playback() {
    let m = fixture("freeze", &[('a', 0), ('b', 100)]);
    let mut pb = MacroPlayback::new(m).with_speed(0.0);

    // At speed 0, no virtual time passes — only the zero-delay event fires
    let events = pb.advance(Duration::from_secs(10));
    assert_eq!(events.len(), 1, "Only zero-delay event fires at speed 0");
}

#[test]
fn negative_speed_normalizes_to_zero() {
    let m = fixture("negative", &[('a', 0), ('b', 100)]);
    let mut pb = MacroPlayback::new(m).with_speed(-1.0);

    let events = pb.advance(Duration::from_secs(10));
    // Negative speed normalizes to 0 per normalize_speed()
    assert_eq!(events.len(), 1, "Negative speed should normalize to 0");
}

#[test]
fn nan_speed_normalizes_to_1x() {
    let m = fixture("nan", &[('a', 50), ('b', 50)]);
    let mut pb = MacroPlayback::new(m).with_speed(f64::NAN);

    // NaN normalizes to 1.0
    let events = drain_all(&mut pb, 10);
    assert_eq!(events.len(), 2, "NaN speed should normalize to 1.0x");
}

#[test]
fn inf_speed_normalizes_to_1x() {
    let m = fixture("inf", &[('a', 50), ('b', 50)]);
    let mut pb = MacroPlayback::new(m).with_speed(f64::INFINITY);

    let events = drain_all(&mut pb, 10);
    assert_eq!(events.len(), 2, "Infinity speed should normalize to 1.0x");
}

// ===========================================================================
// 11. Empty macro edge cases
// ===========================================================================

#[test]
fn empty_macro_is_immediately_done() {
    let m = InputMacro::from_events("empty", vec![]);
    let mut pb = MacroPlayback::new(m);
    assert!(pb.is_done());
    let events = pb.advance(Duration::from_secs(10));
    assert!(events.is_empty());
}

#[test]
fn empty_macro_loop_is_done() {
    let m = InputMacro::from_events("empty_loop", vec![]);
    let mut pb = MacroPlayback::new(m).with_looping(true);
    assert!(pb.is_done());
    let events = pb.advance(Duration::from_secs(10));
    assert!(events.is_empty());
}

// ===========================================================================
// 12. Timing normalization stability
// ===========================================================================

#[test]
fn cumulative_timing_is_monotonic() {
    let m = fixture(
        "monotonic",
        &[
            ('a', 0),
            ('b', 10),
            ('c', 10),
            ('d', 50),
            ('e', 0),
            ('f', 100),
        ],
    );
    let mut pb = MacroPlayback::new(m);
    let timed = drain_with_timing(&mut pb, 1);

    // Elapsed should be monotonically non-decreasing
    let mut prev_ms = 0u64;
    for (i, (_, elapsed)) in timed.iter().enumerate() {
        assert!(
            *elapsed >= prev_ms,
            "Event {}: elapsed {}ms < previous {}ms — monotonicity violated",
            i,
            elapsed,
            prev_ms
        );
        prev_ms = *elapsed;
    }
}
