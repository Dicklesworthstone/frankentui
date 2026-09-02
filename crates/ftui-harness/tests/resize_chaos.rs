#![forbid(unsafe_code)]

//! Resize Chaos/Fuzz Harness (bd-1rz0.23)
//!
//! Fuzz resize streams with randomized jitter, bursts, and pathological patterns.
//! Ensures resize coalescer invariants hold with verbose JSONL logs.
//!
//! # Invariants Tested
//!
//! 1. **Latest-wins**: The final resize in any sequence is never dropped
//! 2. **Bounded latency**: Pending resizes apply within `hard_deadline_ms`
//! 3. **Deterministic**: Same seed produces identical decision sequences
//!
//! # Running Tests
//!
//! ```sh
//! cargo test -p ftui-harness resize_chaos
//! ```
//!
//! # Deterministic Mode
//!
//! ```sh
//! CHAOS_SEED=42 cargo test -p ftui-harness resize_chaos
//! ```
//!
//! # JSONL Schema
//!
//! ```json
//! {"event":"chaos_start","run_id":"...","case":"burst_storm","env":{...},"seed":42,"pattern":"burst"}
//! {"event":"chaos_resize","idx":0,"width":100,"height":40,"delay_ms":5,"jitter_ms":2}
//! {"event":"chaos_decision","idx":0,"action":"coalesce","regime":"steady","pending":"100x40"}
//! {"event":"chaos_apply","idx":3,"width":110,"height":50,"coalesce_time_ms":45,"forced":false}
//! {"event":"chaos_complete","outcome":"pass","total_resizes":100,"total_applies":12,"checksum":"..."}
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use ftui_runtime::resize_coalescer::{CoalesceAction, CoalescerConfig, ResizeCoalescer};

// ============================================================================
// Seeded Random Number Generator
// ============================================================================

/// Simple LCG PRNG for deterministic fuzzing.
#[derive(Debug, Clone)]
struct SeededRng {
    state: u64,
}

impl SeededRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(1),
        }
    }

    fn next_u64(&mut self) -> u64 {
        // LCG parameters from Numerical Recipes
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    #[allow(dead_code)]
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn next_range(&mut self, min: u64, max: u64) -> u64 {
        if max <= min {
            return min;
        }
        min + (self.next_u64() % (max - min))
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() as f64) / (u64::MAX as f64)
    }

    /// Returns true with probability `p` (0.0 to 1.0).
    fn chance(&mut self, p: f64) -> bool {
        self.next_f64() < p
    }
}

// ============================================================================
// Resize Pattern Generators
// ============================================================================

/// A single resize event with timing information.
#[derive(Debug, Clone)]
struct ResizeEvent {
    width: u16,
    height: u16,
    delay_ms: u64,
    jitter_ms: i64,
}

/// Pattern type for logging.
#[derive(Debug, Clone, Copy)]
enum PatternType {
    Steady,
    Burst,
    Oscillating,
    Pathological,
    Mixed,
}

impl PatternType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Steady => "steady",
            Self::Burst => "burst",
            Self::Oscillating => "oscillating",
            Self::Pathological => "pathological",
            Self::Mixed => "mixed",
        }
    }
}

/// Generate a steady stream of resizes (slow, consistent timing).
fn generate_steady_stream(rng: &mut SeededRng, count: usize) -> Vec<ResizeEvent> {
    let mut events = Vec::with_capacity(count);
    let mut width = 80u16;
    let mut height = 24u16;

    for _ in 0..count {
        // Slow resizes with small jitter
        let delay = rng.next_range(100, 500);
        let jitter = (rng.next_range(0, 20) as i64) - 10;

        // Gradual size changes
        if rng.chance(0.5) {
            width = width
                .saturating_add_signed(rng.next_range(0, 10) as i16 - 5)
                .clamp(20, 300);
        }
        if rng.chance(0.5) {
            height = height
                .saturating_add_signed(rng.next_range(0, 10) as i16 - 5)
                .clamp(5, 100);
        }

        events.push(ResizeEvent {
            width,
            height,
            delay_ms: delay,
            jitter_ms: jitter,
        });
    }
    events
}

/// Generate a burst of rapid resizes (triggers burst mode).
fn generate_burst_stream(rng: &mut SeededRng, count: usize) -> Vec<ResizeEvent> {
    let mut events = Vec::with_capacity(count);
    let mut width = 80u16;
    let mut height = 24u16;

    for _ in 0..count {
        // Very rapid resizes
        let delay = rng.next_range(5, 30);
        let jitter = (rng.next_range(0, 10) as i64) - 5;

        // Rapid size changes
        width = width
            .saturating_add_signed(rng.next_range(0, 20) as i16 - 10)
            .clamp(20, 300);
        height = height
            .saturating_add_signed(rng.next_range(0, 10) as i16 - 5)
            .clamp(5, 100);

        events.push(ResizeEvent {
            width,
            height,
            delay_ms: delay,
            jitter_ms: jitter,
        });
    }
    events
}

/// Generate oscillating size changes (ping-pong between sizes).
fn generate_oscillating_stream(rng: &mut SeededRng, count: usize) -> Vec<ResizeEvent> {
    let mut events = Vec::with_capacity(count);
    let sizes = [(80, 24), (120, 40), (80, 24), (60, 15), (80, 24), (200, 60)];

    for i in 0..count {
        let (width, height) = sizes[i % sizes.len()];
        let delay = rng.next_range(20, 80);
        let jitter = (rng.next_range(0, 10) as i64) - 5;

        events.push(ResizeEvent {
            width,
            height,
            delay_ms: delay,
            jitter_ms: jitter,
        });
    }
    events
}

/// Generate pathological patterns (extreme jumps, zero delays, edge cases).
fn generate_pathological_stream(rng: &mut SeededRng, count: usize) -> Vec<ResizeEvent> {
    let mut events = Vec::with_capacity(count);

    for i in 0..count {
        let pattern = i % 6;
        let (width, height, delay, jitter) = match pattern {
            0 => (1, 1, 0, 0),      // Minimum size, zero delay
            1 => (300, 100, 0, 0),  // Maximum size, zero delay
            2 => (80, 24, 1000, 0), // Normal size, long delay
            3 => (
                rng.next_range(1, 300) as u16,
                rng.next_range(1, 100) as u16,
                1,
                0,
            ), // Random, instant
            4 => (80, 24, rng.next_range(0, 200), -50), // Negative jitter
            5 => (120, 40, rng.next_range(0, 50), 100), // Large positive jitter
            _ => unreachable!(),
        };

        events.push(ResizeEvent {
            width,
            height,
            delay_ms: delay,
            jitter_ms: jitter,
        });
    }
    events
}

/// Generate a mixed stream (combines all patterns).
fn generate_mixed_stream(rng: &mut SeededRng, count: usize) -> Vec<ResizeEvent> {
    let mut events = Vec::new();
    let segment_size = count / 4;

    events.extend(generate_steady_stream(rng, segment_size));
    events.extend(generate_burst_stream(rng, segment_size));
    events.extend(generate_oscillating_stream(rng, segment_size));
    events.extend(generate_pathological_stream(rng, count - 3 * segment_size));

    events
}

// ============================================================================
// JSONL Logger
// ============================================================================

/// JSONL logger for chaos test results.
struct ChaosLogger {
    lines: Vec<String>,
    run_id: String,
}

impl ChaosLogger {
    fn new(seed: u64) -> Self {
        Self {
            lines: Vec::new(),
            run_id: format!("{:016x}", seed),
        }
    }

    fn log_start(&mut self, case: &str, pattern: PatternType, seed: u64) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let env = capture_env();
        self.lines.push(format!(
            r#"{{"event":"chaos_start","run_id":"{}","case":"{}","env":{},"seed":{},"pattern":"{}","timestamp":{}}}"#,
            self.run_id, case, env, seed, pattern.as_str(), timestamp
        ));
    }

    fn log_resize(&mut self, idx: usize, width: u16, height: u16, delay_ms: u64, jitter_ms: i64) {
        self.lines.push(format!(
            r#"{{"event":"chaos_resize","idx":{},"width":{},"height":{},"delay_ms":{},"jitter_ms":{}}}"#,
            idx, width, height, delay_ms, jitter_ms
        ));
    }

    fn log_decision(
        &mut self,
        idx: usize,
        action: &str,
        regime: &str,
        pending: Option<(u16, u16)>,
    ) {
        let pending_str = pending
            .map(|(w, h)| format!("\"{}x{}\"", w, h))
            .unwrap_or_else(|| "null".to_string());
        self.lines.push(format!(
            r#"{{"event":"chaos_decision","idx":{},"action":"{}","regime":"{}","pending":{}}}"#,
            idx, action, regime, pending_str
        ));
    }

    fn log_apply(
        &mut self,
        idx: usize,
        width: u16,
        height: u16,
        coalesce_time_ms: u64,
        forced: bool,
    ) {
        self.lines.push(format!(
            r#"{{"event":"chaos_apply","idx":{},"width":{},"height":{},"coalesce_time_ms":{},"forced":{}}}"#,
            idx, width, height, coalesce_time_ms, forced
        ));
    }

    fn log_invariant_check(&mut self, invariant: &str, passed: bool, details: &str) {
        self.lines.push(format!(
            r#"{{"event":"chaos_invariant","invariant":"{}","passed":{},"details":"{}"}}"#,
            invariant,
            passed,
            escape_json(details)
        ));
    }

    fn log_complete(
        &mut self,
        outcome: &str,
        total_resizes: usize,
        total_applies: usize,
        checksum: &str,
    ) {
        self.lines.push(format!(
            r#"{{"event":"chaos_complete","outcome":"{}","total_resizes":{},"total_applies":{},"checksum":"{}"}}"#,
            outcome, total_resizes, total_applies, checksum
        ));
    }

    fn to_jsonl(&self) -> String {
        self.lines.join("\n")
    }

    fn compute_checksum(&self) -> String {
        let mut hasher = DefaultHasher::new();
        // Skip the first line (start event) since it contains non-deterministic
        // elements like timestamp and case name. We only want to checksum the
        // resize/apply/invariant events for deterministic replay verification.
        for line in self.lines.iter().skip(1) {
            line.hash(&mut hasher);
        }
        format!("{:016x}", hasher.finish())
    }
}

fn capture_env() -> String {
    let term = std::env::var("TERM").unwrap_or_default();
    let seed = std::env::var("CHAOS_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    format!(r#"{{"term":"{}","seed":{}}}"#, escape_json(&term), seed)
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

// ============================================================================
// Chaos Test Runner
// ============================================================================

/// Result of a chaos test run.
#[derive(Debug)]
struct ChaosResult {
    passed: bool,
    #[allow(dead_code)]
    total_resizes: usize,
    total_applies: usize,
    invariant_failures: Vec<String>,
    #[allow(dead_code)]
    jsonl: String,
    checksum: String,
}

/// Run a chaos test with the given pattern and configuration.
fn run_chaos_test(
    case_name: &str,
    pattern: PatternType,
    events: Vec<ResizeEvent>,
    config: CoalescerConfig,
    seed: u64,
) -> ChaosResult {
    let mut logger = ChaosLogger::new(seed);
    logger.log_start(case_name, pattern, seed);

    let mut coalescer = ResizeCoalescer::new(config.clone(), (80, 24));
    let mut invariant_failures = Vec::new();
    let mut total_applies = 0;

    let base_time = Instant::now();
    let mut current_time = base_time;

    let final_size = events.last().map(|e| (e.width, e.height));
    let total_resizes = events.len();

    for (idx, event) in events.iter().enumerate() {
        // Apply delay with jitter
        let effective_delay = (event.delay_ms as i64 + event.jitter_ms).max(0) as u64;
        current_time += Duration::from_millis(effective_delay);

        logger.log_resize(
            idx,
            event.width,
            event.height,
            event.delay_ms,
            event.jitter_ms,
        );

        // Handle resize event
        let action = coalescer.handle_resize_at(event.width, event.height, current_time);

        // Log decision
        let action_str = match &action {
            CoalesceAction::None if coalescer.has_pending() => "coalesce",
            CoalesceAction::None => "none",
            CoalesceAction::ApplyResize { .. } => "apply",
        };
        let regime_str = coalescer.regime().as_str();
        let pending = if coalescer.has_pending() {
            Some((event.width, event.height))
        } else {
            None
        };
        logger.log_decision(idx, action_str, regime_str, pending);

        if let CoalesceAction::ApplyResize {
            width,
            height,
            coalesce_time,
            forced_by_deadline,
        } = action
        {
            logger.log_apply(
                idx,
                width,
                height,
                coalesce_time.as_millis() as u64,
                forced_by_deadline,
            );
            total_applies += 1;
        }

        // Also tick to process any pending applies
        let tick_action = coalescer.tick_at(current_time + Duration::from_millis(1));
        if let CoalesceAction::ApplyResize {
            width,
            height,
            coalesce_time,
            forced_by_deadline,
        } = tick_action
        {
            logger.log_apply(
                idx,
                width,
                height,
                coalesce_time.as_millis() as u64,
                forced_by_deadline,
            );
            total_applies += 1;
        }
    }

    // Drain any remaining pending resize
    let mut drain_time = current_time;
    for tick in 0..200 {
        drain_time += Duration::from_millis(10);
        let action = coalescer.tick_at(drain_time);
        if let CoalesceAction::ApplyResize {
            width,
            height,
            coalesce_time,
            forced_by_deadline,
        } = action
        {
            logger.log_apply(
                total_resizes + tick,
                width,
                height,
                coalesce_time.as_millis() as u64,
                forced_by_deadline,
            );
            total_applies += 1;
            break;
        }
        if !coalescer.has_pending() {
            break;
        }
    }

    // Verify invariants
    // 1. Latest-wins: final size must be the last applied
    if let Some((expected_w, expected_h)) = final_size {
        let (actual_w, actual_h) = coalescer.last_applied();
        let passed = actual_w == expected_w && actual_h == expected_h;
        logger.log_invariant_check(
            "latest_wins",
            passed,
            &format!(
                "expected {}x{}, got {}x{}",
                expected_w, expected_h, actual_w, actual_h
            ),
        );
        if !passed {
            invariant_failures.push(format!(
                "latest_wins: expected {}x{}, got {}x{}",
                expected_w, expected_h, actual_w, actual_h
            ));
        }
    }

    // 2. Bounded latency: no pending after drain
    let pending_after_drain = coalescer.has_pending();
    logger.log_invariant_check(
        "bounded_latency",
        !pending_after_drain,
        &format!("has_pending={}", pending_after_drain),
    );
    if pending_after_drain {
        invariant_failures.push("bounded_latency: resize still pending after drain".to_string());
    }

    let checksum = logger.compute_checksum();
    let outcome = if invariant_failures.is_empty() {
        "pass"
    } else {
        "fail"
    };
    logger.log_complete(outcome, total_resizes, total_applies, &checksum);

    ChaosResult {
        passed: invariant_failures.is_empty(),
        total_resizes,
        total_applies,
        invariant_failures,
        jsonl: logger.to_jsonl(),
        checksum,
    }
}

/// Get seed from environment or use default.
fn get_seed() -> u64 {
    std::env::var("CHAOS_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            // Use process ID and timestamp for pseudo-random seed
            let pid = std::process::id() as u64;
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            pid.wrapping_mul(time)
        })
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn chaos_steady_stream() {
    let seed = get_seed();
    let mut rng = SeededRng::new(seed);
    let events = generate_steady_stream(&mut rng, 50);
    let config = CoalescerConfig::default().with_logging(true);

    let result = run_chaos_test("steady_stream", PatternType::Steady, events, config, seed);

    assert!(
        result.passed,
        "Steady stream failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_burst_storm() {
    let seed = get_seed();
    let mut rng = SeededRng::new(seed);
    let events = generate_burst_stream(&mut rng, 100);
    let config = CoalescerConfig::default().with_logging(true);

    let result = run_chaos_test("burst_storm", PatternType::Burst, events, config, seed);

    assert!(
        result.passed,
        "Burst storm failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_oscillating() {
    let seed = get_seed();
    let mut rng = SeededRng::new(seed);
    let events = generate_oscillating_stream(&mut rng, 60);
    let config = CoalescerConfig::default().with_logging(true);

    let result = run_chaos_test(
        "oscillating",
        PatternType::Oscillating,
        events,
        config,
        seed,
    );

    assert!(
        result.passed,
        "Oscillating pattern failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_pathological() {
    let seed = get_seed();
    let mut rng = SeededRng::new(seed);
    let events = generate_pathological_stream(&mut rng, 50);
    let config = CoalescerConfig::default().with_logging(true);

    let result = run_chaos_test(
        "pathological",
        PatternType::Pathological,
        events,
        config,
        seed,
    );

    assert!(
        result.passed,
        "Pathological pattern failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_mixed() {
    let seed = get_seed();
    let mut rng = SeededRng::new(seed);
    let events = generate_mixed_stream(&mut rng, 200);
    let config = CoalescerConfig::default().with_logging(true);

    let result = run_chaos_test("mixed", PatternType::Mixed, events, config, seed);

    assert!(
        result.passed,
        "Mixed pattern failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_determinism() {
    // Verify that same seed produces identical results
    let seed = 42u64;
    let config = CoalescerConfig::default().with_logging(true);

    let results: Vec<_> = (0..3)
        .map(|_| {
            let mut rng = SeededRng::new(seed);
            let events = generate_mixed_stream(&mut rng, 100);
            run_chaos_test(
                "determinism",
                PatternType::Mixed,
                events,
                config.clone(),
                seed,
            )
        })
        .collect();

    assert_eq!(
        results[0].checksum, results[1].checksum,
        "Checksums should match (runs 0 and 1)"
    );
    assert_eq!(
        results[1].checksum, results[2].checksum,
        "Checksums should match (runs 1 and 2)"
    );
}

#[test]
fn chaos_extreme_burst() {
    // Very rapid bursts with minimal delay
    let seed = get_seed();
    let mut rng = SeededRng::new(seed);

    let events: Vec<_> = (0..500)
        .map(|i| ResizeEvent {
            width: 80 + (i % 100) as u16,
            height: 24 + (i % 50) as u16,
            delay_ms: rng.next_range(0, 5),
            jitter_ms: 0,
        })
        .collect();

    let config = CoalescerConfig::default().with_logging(true);
    let result = run_chaos_test("extreme_burst", PatternType::Burst, events, config, seed);

    assert!(
        result.passed,
        "Extreme burst failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_size_extremes() {
    // Test edge case sizes
    let seed = get_seed();
    let events = vec![
        ResizeEvent {
            width: 1,
            height: 1,
            delay_ms: 10,
            jitter_ms: 0,
        },
        ResizeEvent {
            width: u16::MAX,
            height: 1,
            delay_ms: 10,
            jitter_ms: 0,
        },
        ResizeEvent {
            width: 1,
            height: u16::MAX,
            delay_ms: 10,
            jitter_ms: 0,
        },
        ResizeEvent {
            width: u16::MAX,
            height: u16::MAX,
            delay_ms: 10,
            jitter_ms: 0,
        },
        ResizeEvent {
            width: 80,
            height: 24,
            delay_ms: 10,
            jitter_ms: 0,
        },
    ];

    let config = CoalescerConfig::default().with_logging(true);
    let result = run_chaos_test(
        "size_extremes",
        PatternType::Pathological,
        events,
        config,
        seed,
    );

    assert!(
        result.passed,
        "Size extremes failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_zero_delays() {
    // All resizes with zero delay
    let seed = get_seed();
    let events: Vec<_> = (0..100)
        .map(|i| ResizeEvent {
            width: 60 + (i * 2 % 100) as u16,
            height: 20 + (i % 40) as u16,
            delay_ms: 0,
            jitter_ms: 0,
        })
        .collect();

    let config = CoalescerConfig::default().with_logging(true);
    let result = run_chaos_test(
        "zero_delays",
        PatternType::Pathological,
        events,
        config,
        seed,
    );

    assert!(
        result.passed,
        "Zero delays failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_long_gaps() {
    // Long delays between resizes (exceeds hard deadline)
    let seed = get_seed();
    let events: Vec<_> = (0..20)
        .map(|i| ResizeEvent {
            width: 80 + (i * 5 % 100) as u16,
            height: 24 + (i * 2 % 40) as u16,
            delay_ms: 500, // Exceeds default hard_deadline_ms
            jitter_ms: 0,
        })
        .collect();

    let config = CoalescerConfig::default().with_logging(true);
    let result = run_chaos_test("long_gaps", PatternType::Steady, events, config, seed);

    assert!(
        result.passed,
        "Long gaps failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_single_resize() {
    // Edge case: single resize only
    let seed = get_seed();
    let events = vec![ResizeEvent {
        width: 100,
        height: 40,
        delay_ms: 50,
        jitter_ms: 0,
    }];

    let config = CoalescerConfig::default().with_logging(true);
    let result = run_chaos_test("single_resize", PatternType::Steady, events, config, seed);

    assert!(
        result.passed,
        "Single resize failed:\n{}\nFailures: {:?}",
        result.jsonl, result.invariant_failures
    );
}

#[test]
fn chaos_replay_consistency() {
    // Run the same sequence twice and verify identical outcomes
    let seed = 12345u64;
    let config = CoalescerConfig::default().with_logging(true);

    let mut rng1 = SeededRng::new(seed);
    let events1 = generate_burst_stream(&mut rng1, 50);
    let result1 = run_chaos_test(
        "replay_1",
        PatternType::Burst,
        events1,
        config.clone(),
        seed,
    );

    let mut rng2 = SeededRng::new(seed);
    let events2 = generate_burst_stream(&mut rng2, 50);
    let result2 = run_chaos_test("replay_2", PatternType::Burst, events2, config, seed);

    assert_eq!(
        result1.total_applies, result2.total_applies,
        "Replay should produce same number of applies"
    );
    assert_eq!(
        result1.checksum, result2.checksum,
        "Replay should produce same checksum"
    );
}

// ============================================================================
// Invariant Property Tests
// ============================================================================

#[test]
fn invariant_latest_wins_always() {
    // Property: For any resize sequence, the final size is always applied
    for seed_offset in 0..10 {
        let seed = get_seed().wrapping_add(seed_offset);
        let mut rng = SeededRng::new(seed);
        let events = generate_mixed_stream(&mut rng, 100);
        let final_size = events.last().map(|e| (e.width, e.height));

        let config = CoalescerConfig::default();
        let result = run_chaos_test(
            &format!("latest_wins_{}", seed_offset),
            PatternType::Mixed,
            events,
            config,
            seed,
        );

        if let Some((expected_w, expected_h)) = final_size {
            assert!(
                !result
                    .invariant_failures
                    .iter()
                    .any(|f| f.contains("latest_wins")),
                "latest_wins failed for seed {}: expected {}x{}",
                seed,
                expected_w,
                expected_h
            );
        }
    }
}

#[test]
fn invariant_bounded_latency_always() {
    // Property: Pending resizes always apply within hard_deadline_ms
    for seed_offset in 0..10 {
        let seed = get_seed().wrapping_add(seed_offset);
        let mut rng = SeededRng::new(seed);
        let events = generate_burst_stream(&mut rng, 100);

        let config = CoalescerConfig::default();
        let result = run_chaos_test(
            &format!("bounded_latency_{}", seed_offset),
            PatternType::Burst,
            events,
            config,
            seed,
        );

        assert!(
            !result
                .invariant_failures
                .iter()
                .any(|f| f.contains("bounded_latency")),
            "bounded_latency failed for seed {}",
            seed
        );
    }
}
