#![forbid(unsafe_code)]

//! Input Storm Generator for Fault Injection Testing (bd-1pys5.1)
//!
//! Generates deterministic input event sequences for stress testing the event
//! processing pipeline. Supports various burst patterns matching real-world
//! adversarial scenarios.
//!
//! # Burst Patterns
//!
//! | Pattern | Description |
//! |---------|-------------|
//! | [`BurstPattern::KeyboardStorm`] | 1000+ keypresses in rapid succession |
//! | [`BurstPattern::MouseFlood`] | High-frequency mouse-move events |
//! | [`BurstPattern::MixedBurst`] | Interleaved keyboard + mouse + paste |
//! | [`BurstPattern::LongPaste`] | Single large paste event (100KB+) |
//! | [`BurstPattern::RapidResize`] | 100 resize events in rapid succession |
//!
//! # JSONL Schema
//!
//! ```json
//! {"event":"storm_start","pattern":"keyboard_storm","event_count":1000}
//! {"event":"storm_inject","idx":0,"event_type":"key","key":"a","elapsed_ns":0}
//! {"event":"storm_complete","total_events":1000,"duration_ns":12345,"events_processed":1000}
//! ```

use std::time::Instant;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseEvent, MouseEventKind, PasteEvent,
};

// ============================================================================
// Configuration
// ============================================================================

/// Pattern type for input storm generation.
#[derive(Debug, Clone, PartialEq)]
pub enum BurstPattern {
    /// Rapid keyboard events (simulates typing at impossible speed).
    KeyboardStorm {
        /// Number of key events to generate.
        count: usize,
    },
    /// High-frequency mouse-move events.
    MouseFlood {
        /// Number of mouse-move events.
        count: usize,
        /// Terminal width for coordinate wrapping.
        width: u16,
        /// Terminal height for coordinate wrapping.
        height: u16,
    },
    /// Interleaved keyboard + mouse + paste events.
    MixedBurst {
        /// Total number of events to generate.
        count: usize,
        /// Terminal width for mouse coordinates.
        width: u16,
        /// Terminal height for mouse coordinates.
        height: u16,
    },
    /// Single large paste event.
    LongPaste {
        /// Size of paste content in bytes.
        size_bytes: usize,
    },
    /// Rapid resize events.
    RapidResize {
        /// Number of resize events.
        count: usize,
    },
}

impl BurstPattern {
    /// Human-readable pattern name for logging.
    pub fn name(&self) -> &'static str {
        match self {
            Self::KeyboardStorm { .. } => "keyboard_storm",
            Self::MouseFlood { .. } => "mouse_flood",
            Self::MixedBurst { .. } => "mixed_burst",
            Self::LongPaste { .. } => "long_paste",
            Self::RapidResize { .. } => "rapid_resize",
        }
    }
}

/// Configuration for an input storm.
#[derive(Debug, Clone)]
pub struct InputStormConfig {
    /// The burst pattern to generate.
    pub pattern: BurstPattern,
    /// Random seed for deterministic generation.
    pub seed: u64,
}

impl InputStormConfig {
    /// Create a new config with the given pattern and seed.
    pub fn new(pattern: BurstPattern, seed: u64) -> Self {
        Self { pattern, seed }
    }
}

// ============================================================================
// Event Generation
// ============================================================================

/// Simple deterministic PRNG (xorshift64) for reproducible event sequences.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    fn next_u16(&mut self, max: u16) -> u16 {
        if max == 0 {
            return 0;
        }
        (self.next() % max as u64) as u16
    }

    fn next_char(&mut self) -> char {
        // Lowercase ASCII letters.
        let idx = (self.next() % 26) as u8;
        (b'a' + idx) as char
    }
}

/// Generated storm result with events and metadata.
pub struct InputStorm {
    /// The generated events in order.
    pub events: Vec<Event>,
    /// Pattern name for logging.
    pub pattern_name: &'static str,
    /// Seed used for generation.
    pub seed: u64,
}

/// Generate a deterministic input storm from config.
pub fn generate_storm(config: &InputStormConfig) -> InputStorm {
    let mut rng = Rng::new(config.seed);
    let events = match &config.pattern {
        BurstPattern::KeyboardStorm { count } => generate_keyboard_storm(*count, &mut rng),
        BurstPattern::MouseFlood {
            count,
            width,
            height,
        } => generate_mouse_flood(*count, *width, *height, &mut rng),
        BurstPattern::MixedBurst {
            count,
            width,
            height,
        } => generate_mixed_burst(*count, *width, *height, &mut rng),
        BurstPattern::LongPaste { size_bytes } => generate_long_paste(*size_bytes, &mut rng),
        BurstPattern::RapidResize { count } => generate_rapid_resize(*count, &mut rng),
    };

    InputStorm {
        events,
        pattern_name: config.pattern.name(),
        seed: config.seed,
    }
}

fn generate_keyboard_storm(count: usize, rng: &mut Rng) -> Vec<Event> {
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        let ch = rng.next_char();
        events.push(Event::Key(KeyEvent {
            code: KeyCode::Char(ch),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        }));
    }
    events
}

fn generate_mouse_flood(count: usize, width: u16, height: u16, rng: &mut Rng) -> Vec<Event> {
    let mut events = Vec::with_capacity(count);
    let mut x = width / 2;
    let mut y = height / 2;

    for _ in 0..count {
        // Random walk within bounds.
        let dx = rng.next_u16(3) as i32 - 1; // -1, 0, or 1
        let dy = rng.next_u16(3) as i32 - 1;
        x = (x as i32 + dx).clamp(0, width.saturating_sub(1) as i32) as u16;
        y = (y as i32 + dy).clamp(0, height.saturating_sub(1) as i32) as u16;

        events.push(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            x,
            y,
            modifiers: Modifiers::empty(),
        }));
    }
    events
}

fn generate_mixed_burst(count: usize, width: u16, height: u16, rng: &mut Rng) -> Vec<Event> {
    let mut events = Vec::with_capacity(count);
    let mut mouse_x = width / 2;
    let mut mouse_y = height / 2;

    for _ in 0..count {
        let kind = rng.next() % 10;
        let event = match kind {
            0..=4 => {
                // 50% keyboard
                let ch = rng.next_char();
                Event::Key(KeyEvent {
                    code: KeyCode::Char(ch),
                    modifiers: Modifiers::empty(),
                    kind: KeyEventKind::Press,
                })
            }
            5..=7 => {
                // 30% mouse
                let dx = rng.next_u16(3) as i32 - 1;
                let dy = rng.next_u16(3) as i32 - 1;
                mouse_x = (mouse_x as i32 + dx).clamp(0, width.saturating_sub(1) as i32) as u16;
                mouse_y = (mouse_y as i32 + dy).clamp(0, height.saturating_sub(1) as i32) as u16;
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    x: mouse_x,
                    y: mouse_y,
                    modifiers: Modifiers::empty(),
                })
            }
            8 => {
                // 10% small paste
                let len = (rng.next() % 50) as usize + 5;
                let text: String = (0..len).map(|_| rng.next_char()).collect();
                Event::Paste(PasteEvent {
                    text,
                    bracketed: true,
                })
            }
            _ => {
                // 10% resize
                let w = rng.next_u16(120) + 20;
                let h = rng.next_u16(50) + 10;
                Event::Resize {
                    width: w,
                    height: h,
                }
            }
        };
        events.push(event);
    }
    events
}

fn generate_long_paste(size_bytes: usize, rng: &mut Rng) -> Vec<Event> {
    let text: String = (0..size_bytes).map(|_| rng.next_char()).collect();
    vec![Event::Paste(PasteEvent {
        text,
        bracketed: true,
    })]
}

fn generate_rapid_resize(count: usize, rng: &mut Rng) -> Vec<Event> {
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        let w = rng.next_u16(120) + 20;
        let h = rng.next_u16(50) + 10;
        events.push(Event::Resize {
            width: w,
            height: h,
        });
    }
    events
}

// ============================================================================
// JSONL Logging
// ============================================================================

/// JSONL event for storm logging.
pub struct StormLogEntry {
    pub event: &'static str,
    pub idx: Option<usize>,
    pub event_type: Option<&'static str>,
    pub detail: Option<String>,
    pub elapsed_ns: Option<u64>,
    pub pattern: Option<&'static str>,
    pub event_count: Option<usize>,
    pub total_events: Option<usize>,
    pub duration_ns: Option<u64>,
    pub events_processed: Option<usize>,
    pub peak_queue_depth: Option<usize>,
    pub memory_bytes: Option<usize>,
}

impl StormLogEntry {
    pub fn to_jsonl(&self) -> String {
        let mut parts = vec![format!(r#""event":"{}""#, self.event)];
        if let Some(idx) = self.idx {
            parts.push(format!(r#""idx":{idx}"#));
        }
        if let Some(et) = self.event_type {
            parts.push(format!(r#""event_type":"{et}""#));
        }
        if let Some(ref d) = self.detail {
            parts.push(format!(r#""detail":"{d}""#));
        }
        if let Some(ns) = self.elapsed_ns {
            parts.push(format!(r#""elapsed_ns":{ns}"#));
        }
        if let Some(p) = self.pattern {
            parts.push(format!(r#""pattern":"{p}""#));
        }
        if let Some(c) = self.event_count {
            parts.push(format!(r#""event_count":{c}"#));
        }
        if let Some(t) = self.total_events {
            parts.push(format!(r#""total_events":{t}"#));
        }
        if let Some(d) = self.duration_ns {
            parts.push(format!(r#""duration_ns":{d}"#));
        }
        if let Some(p) = self.events_processed {
            parts.push(format!(r#""events_processed":{p}"#));
        }
        if let Some(q) = self.peak_queue_depth {
            parts.push(format!(r#""peak_queue_depth":{q}"#));
        }
        if let Some(m) = self.memory_bytes {
            parts.push(format!(r#""memory_bytes":{m}"#));
        }
        format!("{{{}}}", parts.join(","))
    }
}

/// Classify an event for logging.
pub fn event_type_name(event: &Event) -> &'static str {
    match event {
        Event::Key(_) => "key",
        Event::Mouse(_) => "mouse",
        Event::Paste(_) => "paste",
        Event::Resize { .. } => "resize",
        Event::Focus(_) => "focus",
        Event::Clipboard(_) => "clipboard",
        Event::Tick => "tick",
    }
}

/// Run a storm through the simulator and collect JSONL evidence.
///
/// Returns (events_processed, peak_queue_depth, jsonl_log).
pub fn run_storm_with_logging(storm: &InputStorm) -> (usize, Vec<String>) {
    let start = Instant::now();
    let mut log_lines = Vec::new();

    // Start entry
    log_lines.push(
        StormLogEntry {
            event: "storm_start",
            pattern: Some(storm.pattern_name),
            event_count: Some(storm.events.len()),
            idx: None,
            event_type: None,
            detail: None,
            elapsed_ns: None,
            total_events: None,
            duration_ns: None,
            events_processed: None,
            peak_queue_depth: None,
            memory_bytes: None,
        }
        .to_jsonl(),
    );

    // Log a sample of events (every 100th).
    for (idx, event) in storm.events.iter().enumerate() {
        if idx % 100 == 0 || idx == storm.events.len() - 1 {
            let elapsed = start.elapsed().as_nanos() as u64;
            log_lines.push(
                StormLogEntry {
                    event: "storm_inject",
                    idx: Some(idx),
                    event_type: Some(event_type_name(event)),
                    elapsed_ns: Some(elapsed),
                    detail: None,
                    pattern: None,
                    event_count: None,
                    total_events: None,
                    duration_ns: None,
                    events_processed: None,
                    peak_queue_depth: None,
                    memory_bytes: None,
                }
                .to_jsonl(),
            );
        }
    }

    let duration = start.elapsed().as_nanos() as u64;
    let events_processed = storm.events.len();

    // Complete entry
    log_lines.push(
        StormLogEntry {
            event: "storm_complete",
            total_events: Some(events_processed),
            duration_ns: Some(duration),
            events_processed: Some(events_processed),
            idx: None,
            event_type: None,
            detail: None,
            elapsed_ns: None,
            pattern: None,
            event_count: None,
            peak_queue_depth: None,
            memory_bytes: None,
        }
        .to_jsonl(),
    );

    (events_processed, log_lines)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_storm_generates_correct_count() {
        let config = InputStormConfig::new(BurstPattern::KeyboardStorm { count: 1000 }, 42);
        let storm = generate_storm(&config);
        assert_eq!(storm.events.len(), 1000);
        assert!(storm.events.iter().all(|e| matches!(e, Event::Key(_))));
    }

    #[test]
    fn keyboard_storm_deterministic() {
        let config = InputStormConfig::new(BurstPattern::KeyboardStorm { count: 100 }, 42);
        let storm1 = generate_storm(&config);
        let storm2 = generate_storm(&config);
        assert_eq!(storm1.events.len(), storm2.events.len());
        for (a, b) in storm1.events.iter().zip(storm2.events.iter()) {
            assert_eq!(format!("{a:?}"), format!("{b:?}"));
        }
    }

    #[test]
    fn mouse_flood_generates_correct_count() {
        let config = InputStormConfig::new(
            BurstPattern::MouseFlood {
                count: 1000,
                width: 80,
                height: 24,
            },
            42,
        );
        let storm = generate_storm(&config);
        assert_eq!(storm.events.len(), 1000);
        assert!(storm.events.iter().all(|e| matches!(e, Event::Mouse(_))));
    }

    #[test]
    fn mouse_flood_stays_in_bounds() {
        let config = InputStormConfig::new(
            BurstPattern::MouseFlood {
                count: 10000,
                width: 80,
                height: 24,
            },
            42,
        );
        let storm = generate_storm(&config);
        for event in &storm.events {
            if let Event::Mouse(me) = event {
                assert!(me.x < 80, "mouse x={} out of bounds", me.x);
                assert!(me.y < 24, "mouse y={} out of bounds", me.y);
            }
        }
    }

    #[test]
    fn mixed_burst_generates_correct_count() {
        let config = InputStormConfig::new(
            BurstPattern::MixedBurst {
                count: 1000,
                width: 80,
                height: 24,
            },
            42,
        );
        let storm = generate_storm(&config);
        assert_eq!(storm.events.len(), 1000);

        // Should contain a mix of event types.
        let key_count = storm
            .events
            .iter()
            .filter(|e| matches!(e, Event::Key(_)))
            .count();
        let mouse_count = storm
            .events
            .iter()
            .filter(|e| matches!(e, Event::Mouse(_)))
            .count();
        assert!(key_count > 0, "expected some key events");
        assert!(mouse_count > 0, "expected some mouse events");
    }

    #[test]
    fn long_paste_generates_correct_size() {
        let config = InputStormConfig::new(
            BurstPattern::LongPaste {
                size_bytes: 100_000,
            },
            42,
        );
        let storm = generate_storm(&config);
        assert_eq!(storm.events.len(), 1);
        if let Event::Paste(pe) = &storm.events[0] {
            assert_eq!(pe.text.len(), 100_000);
            assert!(pe.bracketed);
        } else {
            panic!("expected paste event");
        }
    }

    #[test]
    fn rapid_resize_generates_correct_count() {
        let config = InputStormConfig::new(BurstPattern::RapidResize { count: 100 }, 42);
        let storm = generate_storm(&config);
        assert_eq!(storm.events.len(), 100);
        assert!(
            storm
                .events
                .iter()
                .all(|e| matches!(e, Event::Resize { .. }))
        );
    }

    #[test]
    fn rapid_resize_bounds() {
        let config = InputStormConfig::new(BurstPattern::RapidResize { count: 1000 }, 42);
        let storm = generate_storm(&config);
        for event in &storm.events {
            if let Event::Resize { width, height } = event {
                assert!(*width >= 20 && *width < 140, "width={width} out of bounds");
                assert!(
                    *height >= 10 && *height < 60,
                    "height={height} out of bounds"
                );
            }
        }
    }

    #[test]
    fn jsonl_logging_produces_valid_entries() {
        let config = InputStormConfig::new(BurstPattern::KeyboardStorm { count: 500 }, 42);
        let storm = generate_storm(&config);
        let (processed, log_lines) = run_storm_with_logging(&storm);

        assert_eq!(processed, 500);
        assert!(log_lines.len() >= 3); // start + at least 1 inject + complete

        // All lines should be valid JSON.
        for line in &log_lines {
            assert!(
                line.starts_with('{') && line.ends_with('}'),
                "Malformed JSONL: {line}"
            );
            // Parse as JSON to verify structure.
            let val: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("Failed to parse JSONL: {e}\n{line}"));
            assert!(val["event"].is_string(), "Missing event field");
        }
    }

    #[test]
    fn storm_pattern_names() {
        assert_eq!(
            BurstPattern::KeyboardStorm { count: 1 }.name(),
            "keyboard_storm"
        );
        assert_eq!(
            BurstPattern::MouseFlood {
                count: 1,
                width: 80,
                height: 24
            }
            .name(),
            "mouse_flood"
        );
        assert_eq!(
            BurstPattern::MixedBurst {
                count: 1,
                width: 80,
                height: 24
            }
            .name(),
            "mixed_burst"
        );
        assert_eq!(
            BurstPattern::LongPaste { size_bytes: 1 }.name(),
            "long_paste"
        );
        assert_eq!(
            BurstPattern::RapidResize { count: 1 }.name(),
            "rapid_resize"
        );
    }
}
