#![forbid(unsafe_code)]

//! Resize Storm Generator + Replay Harness (bd-1rz0.15)
//!
//! Generates deterministic resize event sequences for E2E and performance testing.
//! Supports various storm patterns (burst, sweep, oscillate, pathological) with
//! verbose JSONL logging for debugging and replay.
//!
//! # Key Features
//!
//! - **Deterministic**: Same seed produces identical resize sequences
//! - **Pattern Library**: Pre-defined patterns for common resize scenarios
//! - **JSONL Logging**: Comprehensive logging with stable schema
//! - **Replay Harness**: Record and replay resize sequences for regression testing
//! - **Flicker Integration**: Analyze captured output for visual artifacts
//!
//! # JSONL Schema
//!
//! ```json
//! {"event":"storm_start","run_id":"...","case":"burst_50","env":{...},"seed":42,"pattern":"burst","capabilities":{...}}
//! {"event":"storm_resize","idx":0,"width":80,"height":24,"delay_ms":10,"elapsed_ms":0}
//! {"event":"storm_capture","idx":0,"bytes_captured":1024,"checksum":"...","flicker_free":true}
//! {"event":"storm_complete","outcome":"pass","total_resizes":50,"total_bytes":51200,"duration_ms":1500,"checksum":"..."}
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use ftui_harness::resize_storm::{StormConfig, StormPattern, ResizeStorm};
//!
//! let config = StormConfig::default()
//!     .with_seed(42)
//!     .with_pattern(StormPattern::Burst { count: 50 });
//!
//! let storm = ResizeStorm::new(config);
//! let events = storm.generate();
//! ```

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as FmtWrite;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::flicker_detection::FlickerAnalysis;

// ============================================================================
// Configuration
// ============================================================================

/// Pattern type for resize storm generation.
#[derive(Debug, Clone, PartialEq)]
pub enum StormPattern {
    /// Rapid burst of resizes with minimal delay.
    Burst {
        /// Number of resize events.
        count: usize,
    },
    /// Gradual size sweep from min to max.
    Sweep {
        /// Starting width.
        start_width: u16,
        /// Starting height.
        start_height: u16,
        /// Ending width.
        end_width: u16,
        /// Ending height.
        end_height: u16,
        /// Number of steps.
        steps: usize,
    },
    /// Oscillate between two sizes.
    Oscillate {
        /// First size (width, height).
        size_a: (u16, u16),
        /// Second size (width, height).
        size_b: (u16, u16),
        /// Number of oscillations.
        cycles: usize,
    },
    /// Pathological edge cases (extremes, zero delays).
    Pathological {
        /// Number of events.
        count: usize,
    },
    /// Mixed pattern combining all types.
    Mixed {
        /// Total number of events.
        count: usize,
    },
    /// Custom resize sequence.
    Custom {
        /// List of (width, height, delay_ms) tuples.
        events: Vec<(u16, u16, u64)>,
    },
}

impl StormPattern {
    /// Get the pattern name for logging.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Burst { .. } => "burst",
            Self::Sweep { .. } => "sweep",
            Self::Oscillate { .. } => "oscillate",
            Self::Pathological { .. } => "pathological",
            Self::Mixed { .. } => "mixed",
            Self::Custom { .. } => "custom",
        }
    }

    /// Get the total number of events this pattern will generate.
    pub fn event_count(&self) -> usize {
        match self {
            Self::Burst { count } => *count,
            Self::Sweep { steps, .. } => *steps,
            Self::Oscillate { cycles, .. } => cycles * 2,
            Self::Pathological { count } => *count,
            Self::Mixed { count } => *count,
            Self::Custom { events } => events.len(),
        }
    }
}

impl Default for StormPattern {
    fn default() -> Self {
        Self::Burst { count: 50 }
    }
}

/// Configuration for resize storm generation.
#[derive(Debug, Clone)]
pub struct StormConfig {
    /// Random seed for deterministic generation.
    pub seed: u64,
    /// Storm pattern to generate.
    pub pattern: StormPattern,
    /// Initial terminal size before storm begins.
    pub initial_size: (u16, u16),
    /// Minimum delay between resizes (ms).
    pub min_delay_ms: u64,
    /// Maximum delay between resizes (ms).
    pub max_delay_ms: u64,
    /// Minimum terminal width.
    pub min_width: u16,
    /// Maximum terminal width.
    pub max_width: u16,
    /// Minimum terminal height.
    pub min_height: u16,
    /// Maximum terminal height.
    pub max_height: u16,
    /// Test case name for logging.
    pub case_name: String,
    /// Enable verbose JSONL logging.
    pub logging_enabled: bool,
}

impl Default for StormConfig {
    fn default() -> Self {
        Self {
            seed: 0,
            pattern: StormPattern::default(),
            initial_size: (80, 24),
            min_delay_ms: 5,
            max_delay_ms: 50,
            min_width: 20,
            max_width: 300,
            min_height: 5,
            max_height: 100,
            case_name: "default".into(),
            logging_enabled: true,
        }
    }
}

impl StormConfig {
    /// Set the random seed.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Set the storm pattern.
    #[must_use]
    pub fn with_pattern(mut self, pattern: StormPattern) -> Self {
        self.pattern = pattern;
        self
    }

    /// Set the initial terminal size.
    #[must_use]
    pub fn with_initial_size(mut self, width: u16, height: u16) -> Self {
        self.initial_size = (width, height);
        self
    }

    /// Set delay range between resizes.
    #[must_use]
    pub fn with_delay_range(mut self, min_ms: u64, max_ms: u64) -> Self {
        self.min_delay_ms = min_ms;
        self.max_delay_ms = max_ms;
        self
    }

    /// Set size bounds.
    #[must_use]
    pub fn with_size_bounds(
        mut self,
        min_width: u16,
        max_width: u16,
        min_height: u16,
        max_height: u16,
    ) -> Self {
        self.min_width = min_width;
        self.max_width = max_width;
        self.min_height = min_height;
        self.max_height = max_height;
        self
    }

    /// Set the test case name.
    #[must_use]
    pub fn with_case_name(mut self, name: impl Into<String>) -> Self {
        self.case_name = name.into();
        self
    }

    /// Enable or disable logging.
    #[must_use]
    pub fn with_logging(mut self, enabled: bool) -> Self {
        self.logging_enabled = enabled;
        self
    }
}

// ============================================================================
// Seeded RNG
// ============================================================================

/// Simple LCG PRNG for deterministic generation.
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

    fn next_range(&mut self, min: u64, max: u64) -> u64 {
        if max <= min {
            return min;
        }
        min + (self.next_u64() % (max - min))
    }

    fn next_u16_range(&mut self, min: u16, max: u16) -> u16 {
        self.next_range(min as u64, max as u64) as u16
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() as f64) / (u64::MAX as f64)
    }

    fn chance(&mut self, p: f64) -> bool {
        self.next_f64() < p
    }
}

// ============================================================================
// Resize Event
// ============================================================================

/// A single resize event in a storm sequence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResizeEvent {
    /// Target width.
    pub width: u16,
    /// Target height.
    pub height: u16,
    /// Delay before this resize (ms).
    pub delay_ms: u64,
    /// Index in the sequence.
    pub index: usize,
}

impl ResizeEvent {
    /// Create a new resize event.
    pub fn new(width: u16, height: u16, delay_ms: u64, index: usize) -> Self {
        Self {
            width,
            height,
            delay_ms,
            index,
        }
    }

    /// Convert to JSONL format.
    pub fn to_jsonl(&self, elapsed_ms: u64) -> String {
        format!(
            r#"{{"event":"storm_resize","idx":{},"width":{},"height":{},"delay_ms":{},"elapsed_ms":{}}}"#,
            self.index, self.width, self.height, self.delay_ms, elapsed_ms
        )
    }
}

// ============================================================================
// Storm Generator
// ============================================================================

/// Resize storm generator.
#[derive(Debug, Clone)]
pub struct ResizeStorm {
    config: StormConfig,
    events: Vec<ResizeEvent>,
    run_id: String,
}

impl ResizeStorm {
    /// Create a new storm generator with the given configuration.
    pub fn new(config: StormConfig) -> Self {
        let run_id = format!(
            "{:016x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64 ^ config.seed)
                .unwrap_or(config.seed)
        );

        let mut storm = Self {
            config,
            events: Vec::new(),
            run_id,
        };
        storm.generate_events();
        storm
    }

    /// Get the run ID.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Get the generated events.
    pub fn events(&self) -> &[ResizeEvent] {
        &self.events
    }

    /// Get the configuration.
    pub fn config(&self) -> &StormConfig {
        &self.config
    }

    /// Generate resize events based on the pattern.
    fn generate_events(&mut self) {
        let mut rng = SeededRng::new(self.config.seed);

        self.events = match &self.config.pattern {
            StormPattern::Burst { count } => self.generate_burst(&mut rng, *count),
            StormPattern::Sweep {
                start_width,
                start_height,
                end_width,
                end_height,
                steps,
            } => self.generate_sweep(*start_width, *start_height, *end_width, *end_height, *steps),
            StormPattern::Oscillate {
                size_a,
                size_b,
                cycles,
            } => self.generate_oscillate(&mut rng, *size_a, *size_b, *cycles),
            StormPattern::Pathological { count } => self.generate_pathological(&mut rng, *count),
            StormPattern::Mixed { count } => self.generate_mixed(&mut rng, *count),
            StormPattern::Custom { events } => events
                .iter()
                .enumerate()
                .map(|(i, (w, h, d))| ResizeEvent::new(*w, *h, *d, i))
                .collect(),
        };
    }

    fn generate_burst(&self, rng: &mut SeededRng, count: usize) -> Vec<ResizeEvent> {
        let mut events = Vec::with_capacity(count);
        let (mut width, mut height) = self.config.initial_size;

        for i in 0..count {
            // Rapid resizes with minimal delay
            let delay = rng.next_range(self.config.min_delay_ms, self.config.max_delay_ms / 2);

            // Random size changes within bounds
            if rng.chance(0.7) {
                let delta = rng.next_u16_range(1, 20) as i16;
                let sign = if rng.chance(0.5) { 1 } else { -1 };
                width = (width as i16 + delta * sign)
                    .clamp(self.config.min_width as i16, self.config.max_width as i16)
                    as u16;
            }
            if rng.chance(0.7) {
                let delta = rng.next_u16_range(1, 10) as i16;
                let sign = if rng.chance(0.5) { 1 } else { -1 };
                height = (height as i16 + delta * sign)
                    .clamp(self.config.min_height as i16, self.config.max_height as i16)
                    as u16;
            }

            events.push(ResizeEvent::new(width, height, delay, i));
        }
        events
    }

    fn generate_sweep(
        &self,
        start_w: u16,
        start_h: u16,
        end_w: u16,
        end_h: u16,
        steps: usize,
    ) -> Vec<ResizeEvent> {
        let mut events = Vec::with_capacity(steps);

        for i in 0..steps {
            let t = if steps > 1 {
                i as f64 / (steps - 1) as f64
            } else {
                1.0
            };

            let width = (start_w as f64 + (end_w as f64 - start_w as f64) * t).round() as u16;
            let height = (start_h as f64 + (end_h as f64 - start_h as f64) * t).round() as u16;
            let delay = (self.config.min_delay_ms + self.config.max_delay_ms) / 2;

            events.push(ResizeEvent::new(width, height, delay, i));
        }
        events
    }

    fn generate_oscillate(
        &self,
        rng: &mut SeededRng,
        size_a: (u16, u16),
        size_b: (u16, u16),
        cycles: usize,
    ) -> Vec<ResizeEvent> {
        let mut events = Vec::with_capacity(cycles * 2);

        for cycle in 0..cycles {
            let delay_a = rng.next_range(self.config.min_delay_ms, self.config.max_delay_ms);
            let delay_b = rng.next_range(self.config.min_delay_ms, self.config.max_delay_ms);

            events.push(ResizeEvent::new(size_a.0, size_a.1, delay_a, cycle * 2));
            events.push(ResizeEvent::new(size_b.0, size_b.1, delay_b, cycle * 2 + 1));
        }
        events
    }

    fn generate_pathological(&self, rng: &mut SeededRng, count: usize) -> Vec<ResizeEvent> {
        let mut events = Vec::with_capacity(count);

        for i in 0..count {
            let pattern = i % 8;
            let (width, height, delay) = match pattern {
                0 => (self.config.min_width, self.config.min_height, 0), // Minimum, instant
                1 => (self.config.max_width, self.config.max_height, 0), // Maximum, instant
                2 => (1, 1, 1),                                          // Extreme minimum
                3 => (500, 200, 1),                                      // Large
                4 => (80, 24, 500),                                      // Normal, long delay
                5 => {
                    // Random, zero delay
                    (
                        rng.next_u16_range(self.config.min_width, self.config.max_width),
                        rng.next_u16_range(self.config.min_height, self.config.max_height),
                        0,
                    )
                }
                6 => (80, 24, rng.next_range(0, 1000)), // Normal, random delay
                7 => {
                    // Alternating extremes
                    if i % 2 == 0 {
                        (self.config.min_width, self.config.max_height, 5)
                    } else {
                        (self.config.max_width, self.config.min_height, 5)
                    }
                }
                _ => unreachable!(),
            };

            events.push(ResizeEvent::new(width, height, delay, i));
        }
        events
    }

    fn generate_mixed(&self, rng: &mut SeededRng, count: usize) -> Vec<ResizeEvent> {
        let segment = count / 4;
        let mut events = Vec::with_capacity(count);

        // Burst segment
        let burst = self.generate_burst(rng, segment);
        events.extend(burst);

        // Sweep segment
        let sweep = self.generate_sweep(60, 15, 150, 50, segment);
        for (i, mut e) in sweep.into_iter().enumerate() {
            e.index = events.len() + i;
            events.push(e);
        }

        // Oscillate segment
        let oscillate = self.generate_oscillate(rng, (80, 24), (120, 40), segment / 2);
        for (i, mut e) in oscillate.into_iter().enumerate() {
            e.index = events.len() + i;
            events.push(e);
        }

        // Pathological segment
        let remaining = count - events.len();
        let pathological = self.generate_pathological(rng, remaining);
        for (i, mut e) in pathological.into_iter().enumerate() {
            e.index = events.len() + i;
            events.push(e);
        }

        events
    }

    /// Compute a deterministic checksum of the event sequence.
    pub fn sequence_checksum(&self) -> String {
        let mut hasher = DefaultHasher::new();
        for event in &self.events {
            event.hash(&mut hasher);
        }
        format!("{:016x}", hasher.finish())
    }

    /// Get total duration of the storm (sum of delays).
    pub fn total_duration_ms(&self) -> u64 {
        self.events.iter().map(|e| e.delay_ms).sum()
    }
}

// ============================================================================
// JSONL Logger
// ============================================================================

/// JSONL logger for storm execution.
pub struct StormLogger {
    lines: Vec<String>,
    run_id: String,
    start_time: Instant,
}

impl StormLogger {
    /// Create a new logger.
    pub fn new(run_id: &str) -> Self {
        Self {
            lines: Vec::new(),
            run_id: run_id.to_string(),
            start_time: Instant::now(),
        }
    }

    /// Log storm start event.
    pub fn log_start(&mut self, storm: &ResizeStorm, capabilities: &TerminalCapabilities) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let env = capture_env();
        let caps = capabilities.to_json();

        self.lines.push(format!(
            r#"{{"event":"storm_start","run_id":"{}","case":"{}","env":{},"seed":{},"pattern":"{}","event_count":{},"capabilities":{},"timestamp":{}}}"#,
            self.run_id,
            storm.config.case_name,
            env,
            storm.config.seed,
            storm.config.pattern.name(),
            storm.events.len(),
            caps,
            timestamp
        ));
    }

    /// Log a resize event.
    pub fn log_resize(&mut self, event: &ResizeEvent) {
        let elapsed = self.start_time.elapsed().as_millis() as u64;
        self.lines.push(event.to_jsonl(elapsed));
    }

    /// Log capture result after a resize.
    pub fn log_capture(
        &mut self,
        idx: usize,
        bytes_captured: usize,
        checksum: &str,
        flicker_free: bool,
    ) {
        self.lines.push(format!(
            r#"{{"event":"storm_capture","idx":{},"bytes_captured":{},"checksum":"{}","flicker_free":{}}}"#,
            idx, bytes_captured, checksum, flicker_free
        ));
    }

    /// Log storm completion.
    pub fn log_complete(
        &mut self,
        outcome: &str,
        total_resizes: usize,
        total_bytes: usize,
        checksum: &str,
    ) {
        let duration_ms = self.start_time.elapsed().as_millis() as u64;
        self.lines.push(format!(
            r#"{{"event":"storm_complete","outcome":"{}","total_resizes":{},"total_bytes":{},"duration_ms":{},"checksum":"{}"}}"#,
            outcome, total_resizes, total_bytes, duration_ms, checksum
        ));
    }

    /// Log an error.
    pub fn log_error(&mut self, message: &str) {
        self.lines.push(format!(
            r#"{{"event":"storm_error","message":"{}"}}"#,
            escape_json(message)
        ));
    }

    /// Get all log lines as JSONL.
    pub fn to_jsonl(&self) -> String {
        self.lines.join("\n")
    }

    /// Write to a file.
    pub fn write_to_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut file = std::fs::File::create(path)?;
        for line in &self.lines {
            writeln!(file, "{}", line)?;
        }
        Ok(())
    }
}

// ============================================================================
// Terminal Capabilities
// ============================================================================

/// Terminal capabilities detected at runtime.
#[derive(Debug, Clone, Default)]
pub struct TerminalCapabilities {
    /// TERM environment variable.
    pub term: String,
    /// COLORTERM environment variable.
    pub colorterm: String,
    /// Whether NO_COLOR is set.
    pub no_color: bool,
    /// Whether running in a multiplexer (tmux, screen, etc.).
    pub in_mux: bool,
    /// Detected multiplexer name.
    pub mux_name: Option<String>,
    /// Whether synchronized output is supported.
    pub sync_output: bool,
}

impl TerminalCapabilities {
    /// Detect capabilities from environment.
    pub fn detect() -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        let no_color = std::env::var("NO_COLOR").is_ok();

        let (in_mux, mux_name) = detect_mux();

        // Assume sync output support for modern terminals
        let sync_output = term.contains("256color")
            || term.contains("kitty")
            || term.contains("alacritty")
            || colorterm == "truecolor";

        Self {
            term,
            colorterm,
            no_color,
            in_mux,
            mux_name,
            sync_output,
        }
    }

    /// Convert to JSON string.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"term":"{}","colorterm":"{}","no_color":{},"in_mux":{},"mux_name":{},"sync_output":{}}}"#,
            escape_json(&self.term),
            escape_json(&self.colorterm),
            self.no_color,
            self.in_mux,
            self.mux_name
                .as_ref()
                .map(|s| format!(r#""{}""#, escape_json(s)))
                .unwrap_or_else(|| "null".to_string()),
            self.sync_output
        )
    }
}

fn detect_mux() -> (bool, Option<String>) {
    if std::env::var("TMUX").is_ok() {
        return (true, Some("tmux".to_string()));
    }
    if std::env::var("STY").is_ok() {
        return (true, Some("screen".to_string()));
    }
    if std::env::var("ZELLIJ").is_ok() {
        return (true, Some("zellij".to_string()));
    }
    if let Ok(prog) = std::env::var("TERM_PROGRAM")
        && prog.to_lowercase().contains("tmux")
    {
        return (true, Some("tmux".to_string()));
    }
    (false, None)
}

// ============================================================================
// Storm Result
// ============================================================================

/// Result of executing a resize storm.
#[derive(Debug)]
pub struct StormResult {
    /// Whether the storm passed all checks.
    pub passed: bool,
    /// Total resize events executed.
    pub total_resizes: usize,
    /// Total bytes captured from output.
    pub total_bytes: usize,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
    /// Flicker analysis results (if analyzed).
    pub flicker_analysis: Option<FlickerAnalysis>,
    /// Sequence checksum for replay verification.
    pub sequence_checksum: String,
    /// Output checksum.
    pub output_checksum: String,
    /// JSONL log.
    pub jsonl: String,
    /// Any error messages.
    pub errors: Vec<String>,
}

impl StormResult {
    /// Assert that the storm passed.
    pub fn assert_passed(&self) {
        if !self.passed {
            let mut msg = String::new();
            msg.push_str("\n=== Resize Storm Failed ===\n\n");
            writeln!(msg, "Resizes: {}", self.total_resizes).unwrap();
            writeln!(msg, "Bytes: {}", self.total_bytes).unwrap();
            writeln!(msg, "Duration: {}ms", self.duration_ms).unwrap();

            if !self.errors.is_empty() {
                msg.push_str("\nErrors:\n");
                for err in &self.errors {
                    writeln!(msg, "  - {}", err).unwrap();
                }
            }

            if let Some(ref analysis) = self.flicker_analysis
                && !analysis.flicker_free
            {
                msg.push_str("\nFlicker Issues:\n");
                for issue in &analysis.issues {
                    writeln!(
                        msg,
                        "  - [{}] {}: {}",
                        issue.severity, issue.event_type, issue.details.message
                    )
                    .unwrap();
                }
            }

            msg.push_str("\nJSONL Log:\n");
            msg.push_str(&self.jsonl);

            panic!("{}", msg);
        }
    }
}

// ============================================================================
// Replay Harness
// ============================================================================

/// Recorded storm for replay.
#[derive(Debug, Clone)]
pub struct RecordedStorm {
    /// Configuration used.
    pub config: StormConfig,
    /// Generated events.
    pub events: Vec<ResizeEvent>,
    /// Sequence checksum for verification.
    pub sequence_checksum: String,
    /// Expected output checksum (if known).
    pub expected_output_checksum: Option<String>,
}

impl RecordedStorm {
    /// Record a storm for later replay.
    pub fn record(storm: &ResizeStorm) -> Self {
        Self {
            config: storm.config.clone(),
            events: storm.events.clone(),
            sequence_checksum: storm.sequence_checksum(),
            expected_output_checksum: None,
        }
    }

    /// Record with expected output checksum.
    pub fn record_with_output(storm: &ResizeStorm, output_checksum: String) -> Self {
        let mut recorded = Self::record(storm);
        recorded.expected_output_checksum = Some(output_checksum);
        recorded
    }

    /// Verify that a replay matches this recording.
    pub fn verify_replay(&self, storm: &ResizeStorm) -> bool {
        self.sequence_checksum == storm.sequence_checksum()
    }

    /// Serialize to JSON for storage.
    pub fn to_json(&self) -> String {
        let events_json: Vec<String> = self
            .events
            .iter()
            .map(|e| {
                format!(
                    r#"{{"width":{},"height":{},"delay_ms":{},"index":{}}}"#,
                    e.width, e.height, e.delay_ms, e.index
                )
            })
            .collect();

        format!(
            r#"{{"seed":{},"pattern":"{}","case_name":"{}","initial_size":[{},{}],"events":[{}],"sequence_checksum":"{}","expected_output_checksum":{}}}"#,
            self.config.seed,
            self.config.pattern.name(),
            escape_json(&self.config.case_name),
            self.config.initial_size.0,
            self.config.initial_size.1,
            events_json.join(","),
            self.sequence_checksum,
            self.expected_output_checksum
                .as_ref()
                .map(|s| format!(r#""{}""#, s))
                .unwrap_or_else(|| "null".to_string())
        )
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn capture_env() -> String {
    let term = std::env::var("TERM").unwrap_or_default();
    let colorterm = std::env::var("COLORTERM").unwrap_or_default();
    let seed = std::env::var("STORM_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    format!(
        r#"{{"term":"{}","colorterm":"{}","env_seed":{}}}"#,
        escape_json(&term),
        escape_json(&colorterm),
        seed
    )
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Get seed from environment or generate from time.
pub fn get_storm_seed() -> u64 {
    std::env::var("STORM_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            let pid = std::process::id() as u64;
            let time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            pid.wrapping_mul(time)
        })
}

/// Compute checksum of captured output.
pub fn compute_output_checksum(data: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// ============================================================================
// Integration with Flicker Detection
// ============================================================================

/// Analyze captured output for flicker.
pub fn analyze_storm_output(output: &[u8], run_id: &str) -> FlickerAnalysis {
    crate::flicker_detection::analyze_stream_with_id(run_id, output)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_pattern_generates_correct_count() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Burst { count: 100 });

        let storm = ResizeStorm::new(config);
        assert_eq!(storm.events().len(), 100);
    }

    #[test]
    fn sweep_pattern_interpolates_sizes() {
        let config = StormConfig::default().with_pattern(StormPattern::Sweep {
            start_width: 80,
            start_height: 24,
            end_width: 160,
            end_height: 48,
            steps: 5,
        });

        let storm = ResizeStorm::new(config);
        let events = storm.events();

        assert_eq!(events.len(), 5);
        assert_eq!(events[0].width, 80);
        assert_eq!(events[0].height, 24);
        assert_eq!(events[4].width, 160);
        assert_eq!(events[4].height, 48);
    }

    #[test]
    fn oscillate_pattern_alternates() {
        let config = StormConfig::default().with_pattern(StormPattern::Oscillate {
            size_a: (80, 24),
            size_b: (120, 40),
            cycles: 3,
        });

        let storm = ResizeStorm::new(config);
        let events = storm.events();

        assert_eq!(events.len(), 6);
        assert_eq!((events[0].width, events[0].height), (80, 24));
        assert_eq!((events[1].width, events[1].height), (120, 40));
        assert_eq!((events[2].width, events[2].height), (80, 24));
    }

    #[test]
    fn deterministic_with_seed() {
        let config = StormConfig::default()
            .with_seed(12345)
            .with_pattern(StormPattern::Burst { count: 50 });

        let storm1 = ResizeStorm::new(config.clone());
        let storm2 = ResizeStorm::new(config);

        assert_eq!(storm1.sequence_checksum(), storm2.sequence_checksum());
        assert_eq!(storm1.events(), storm2.events());
    }

    #[test]
    fn different_seeds_produce_different_sequences() {
        let storm1 = ResizeStorm::new(
            StormConfig::default()
                .with_seed(1)
                .with_pattern(StormPattern::Burst { count: 50 }),
        );
        let storm2 = ResizeStorm::new(
            StormConfig::default()
                .with_seed(2)
                .with_pattern(StormPattern::Burst { count: 50 }),
        );

        assert_ne!(storm1.sequence_checksum(), storm2.sequence_checksum());
    }

    #[test]
    fn custom_pattern_uses_provided_events() {
        let custom_events = vec![(100, 50, 10), (80, 24, 20), (120, 40, 15)];

        let config = StormConfig::default().with_pattern(StormPattern::Custom {
            events: custom_events,
        });

        let storm = ResizeStorm::new(config);
        let events = storm.events();

        assert_eq!(events.len(), 3);
        assert_eq!((events[0].width, events[0].height), (100, 50));
        assert_eq!(events[0].delay_ms, 10);
    }

    #[test]
    fn mixed_pattern_combines_all() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Mixed { count: 100 });

        let storm = ResizeStorm::new(config);
        assert_eq!(storm.events().len(), 100);
    }

    #[test]
    fn pathological_pattern_includes_extremes() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Pathological { count: 16 });

        let storm = ResizeStorm::new(config);
        let events = storm.events();

        // Should include min/max sizes and zero delays
        assert!(events.iter().any(|e| e.delay_ms == 0));
        assert!(events.iter().any(|e| e.width == 20)); // min_width default
    }

    #[test]
    fn storm_logger_produces_valid_jsonl() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_case_name("test_case")
            .with_pattern(StormPattern::Burst { count: 5 });

        let storm = ResizeStorm::new(config);
        let mut logger = StormLogger::new(storm.run_id());
        let caps = TerminalCapabilities::default();

        logger.log_start(&storm, &caps);
        for event in storm.events() {
            logger.log_resize(event);
        }
        logger.log_complete("pass", 5, 1000, "abc123");

        let jsonl = logger.to_jsonl();
        assert!(jsonl.contains(r#""event":"storm_start""#));
        assert!(jsonl.contains(r#""event":"storm_resize""#));
        assert!(jsonl.contains(r#""event":"storm_complete""#));
    }

    #[test]
    fn recorded_storm_can_verify_replay() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Burst { count: 20 });

        let storm1 = ResizeStorm::new(config.clone());
        let recorded = RecordedStorm::record(&storm1);

        let storm2 = ResizeStorm::new(config);
        assert!(recorded.verify_replay(&storm2));
    }

    #[test]
    fn terminal_capabilities_to_json() {
        let caps = TerminalCapabilities {
            term: "xterm-256color".to_string(),
            colorterm: "truecolor".to_string(),
            no_color: false,
            in_mux: true,
            mux_name: Some("tmux".to_string()),
            sync_output: true,
        };

        let json = caps.to_json();
        assert!(json.contains(r#""term":"xterm-256color""#));
        assert!(json.contains(r#""in_mux":true"#));
        assert!(json.contains(r#""mux_name":"tmux""#));
    }

    #[test]
    fn resize_event_to_jsonl() {
        let event = ResizeEvent::new(100, 50, 25, 3);
        let jsonl = event.to_jsonl(1500);

        assert!(jsonl.contains(r#""width":100"#));
        assert!(jsonl.contains(r#""height":50"#));
        assert!(jsonl.contains(r#""delay_ms":25"#));
        assert!(jsonl.contains(r#""elapsed_ms":1500"#));
    }

    #[test]
    fn total_duration_calculation() {
        let config = StormConfig::default().with_pattern(StormPattern::Custom {
            events: vec![(80, 24, 100), (100, 40, 200), (80, 24, 150)],
        });

        let storm = ResizeStorm::new(config);
        assert_eq!(storm.total_duration_ms(), 450);
    }

    #[test]
    fn size_bounds_are_respected() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_size_bounds(50, 100, 20, 40)
            .with_pattern(StormPattern::Burst { count: 100 });

        let storm = ResizeStorm::new(config);

        for event in storm.events() {
            assert!(event.width >= 50 && event.width <= 100);
            assert!(event.height >= 20 && event.height <= 40);
        }
    }

    // ─── Edge-case tests (bd-38ujl) ─────────────────────────────

    #[test]
    fn pattern_name_all_variants() {
        assert_eq!(StormPattern::Burst { count: 1 }.name(), "burst");
        assert_eq!(
            StormPattern::Sweep {
                start_width: 80,
                start_height: 24,
                end_width: 160,
                end_height: 48,
                steps: 5
            }
            .name(),
            "sweep"
        );
        assert_eq!(
            StormPattern::Oscillate {
                size_a: (80, 24),
                size_b: (120, 40),
                cycles: 1
            }
            .name(),
            "oscillate"
        );
        assert_eq!(
            StormPattern::Pathological { count: 1 }.name(),
            "pathological"
        );
        assert_eq!(StormPattern::Mixed { count: 1 }.name(), "mixed");
        assert_eq!(StormPattern::Custom { events: Vec::new() }.name(), "custom");
    }

    #[test]
    fn pattern_event_count_all_variants() {
        assert_eq!(StormPattern::Burst { count: 42 }.event_count(), 42);
        assert_eq!(
            StormPattern::Sweep {
                start_width: 80,
                start_height: 24,
                end_width: 160,
                end_height: 48,
                steps: 10
            }
            .event_count(),
            10
        );
        assert_eq!(
            StormPattern::Oscillate {
                size_a: (80, 24),
                size_b: (120, 40),
                cycles: 5
            }
            .event_count(),
            10 // cycles * 2
        );
        assert_eq!(StormPattern::Pathological { count: 7 }.event_count(), 7);
        assert_eq!(StormPattern::Mixed { count: 20 }.event_count(), 20);
        assert_eq!(
            StormPattern::Custom {
                events: vec![(80, 24, 10), (100, 50, 20)]
            }
            .event_count(),
            2
        );
    }

    #[test]
    fn pattern_default_is_burst_50() {
        let pattern = StormPattern::default();
        assert_eq!(pattern, StormPattern::Burst { count: 50 });
    }

    #[test]
    fn pattern_clone_and_eq() {
        let pattern = StormPattern::Oscillate {
            size_a: (80, 24),
            size_b: (120, 40),
            cycles: 3,
        };
        let cloned = pattern.clone();
        assert_eq!(pattern, cloned);
    }

    #[test]
    fn pattern_debug_format() {
        let pattern = StormPattern::Burst { count: 5 };
        let debug = format!("{pattern:?}");
        assert!(debug.contains("Burst"));
        assert!(debug.contains("5"));
    }

    #[test]
    fn config_default_values() {
        let config = StormConfig::default();
        assert_eq!(config.seed, 0);
        assert_eq!(config.initial_size, (80, 24));
        assert_eq!(config.min_delay_ms, 5);
        assert_eq!(config.max_delay_ms, 50);
        assert_eq!(config.min_width, 20);
        assert_eq!(config.max_width, 300);
        assert_eq!(config.min_height, 5);
        assert_eq!(config.max_height, 100);
        assert_eq!(config.case_name, "default");
        assert!(config.logging_enabled);
    }

    #[test]
    fn config_builder_chain() {
        let config = StormConfig::default()
            .with_seed(99)
            .with_pattern(StormPattern::Pathological { count: 3 })
            .with_initial_size(100, 50)
            .with_delay_range(10, 100)
            .with_size_bounds(30, 200, 10, 80)
            .with_case_name("my_test")
            .with_logging(false);

        assert_eq!(config.seed, 99);
        assert_eq!(config.initial_size, (100, 50));
        assert_eq!(config.min_delay_ms, 10);
        assert_eq!(config.max_delay_ms, 100);
        assert_eq!(config.min_width, 30);
        assert_eq!(config.max_width, 200);
        assert_eq!(config.min_height, 10);
        assert_eq!(config.max_height, 80);
        assert_eq!(config.case_name, "my_test");
        assert!(!config.logging_enabled);
        assert_eq!(config.pattern, StormPattern::Pathological { count: 3 });
    }

    #[test]
    fn config_debug_format() {
        let config = StormConfig::default();
        let debug = format!("{config:?}");
        assert!(debug.contains("StormConfig"));
        assert!(debug.contains("seed"));
    }

    #[test]
    fn config_clone() {
        let config = StormConfig::default().with_seed(42);
        let cloned = config.clone();
        assert_eq!(cloned.seed, 42);
    }

    #[test]
    fn resize_event_fields() {
        let event = ResizeEvent::new(120, 40, 25, 7);
        assert_eq!(event.width, 120);
        assert_eq!(event.height, 40);
        assert_eq!(event.delay_ms, 25);
        assert_eq!(event.index, 7);
    }

    #[test]
    fn resize_event_clone_eq_hash() {
        let event = ResizeEvent::new(80, 24, 10, 0);
        let cloned = event.clone();
        assert_eq!(event, cloned);

        // Hash equality
        let mut h1 = DefaultHasher::new();
        let mut h2 = DefaultHasher::new();
        event.hash(&mut h1);
        cloned.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn resize_event_debug_format() {
        let event = ResizeEvent::new(80, 24, 10, 0);
        let debug = format!("{event:?}");
        assert!(debug.contains("ResizeEvent"));
        assert!(debug.contains("80"));
    }

    #[test]
    fn resize_event_to_jsonl_format() {
        let event = ResizeEvent::new(80, 24, 10, 3);
        let jsonl = event.to_jsonl(500);
        assert!(jsonl.starts_with('{'));
        assert!(jsonl.ends_with('}'));
        assert!(jsonl.contains(r#""event":"storm_resize""#));
        assert!(jsonl.contains(r#""idx":3"#));
        assert!(jsonl.contains(r#""width":80"#));
        assert!(jsonl.contains(r#""height":24"#));
        assert!(jsonl.contains(r#""delay_ms":10"#));
        assert!(jsonl.contains(r#""elapsed_ms":500"#));
    }

    #[test]
    fn burst_zero_count() {
        let config = StormConfig::default()
            .with_seed(1)
            .with_pattern(StormPattern::Burst { count: 0 });
        let storm = ResizeStorm::new(config);
        assert!(storm.events().is_empty());
    }

    #[test]
    fn burst_single_event() {
        let config = StormConfig::default()
            .with_seed(1)
            .with_pattern(StormPattern::Burst { count: 1 });
        let storm = ResizeStorm::new(config);
        assert_eq!(storm.events().len(), 1);
        assert_eq!(storm.events()[0].index, 0);
    }

    #[test]
    fn sweep_single_step() {
        let config = StormConfig::default().with_pattern(StormPattern::Sweep {
            start_width: 80,
            start_height: 24,
            end_width: 160,
            end_height: 48,
            steps: 1,
        });
        let storm = ResizeStorm::new(config);
        assert_eq!(storm.events().len(), 1);
        // Single step should use t=1.0 → end values
        assert_eq!(storm.events()[0].width, 160);
        assert_eq!(storm.events()[0].height, 48);
    }

    #[test]
    fn sweep_zero_steps() {
        let config = StormConfig::default().with_pattern(StormPattern::Sweep {
            start_width: 80,
            start_height: 24,
            end_width: 160,
            end_height: 48,
            steps: 0,
        });
        let storm = ResizeStorm::new(config);
        assert!(storm.events().is_empty());
    }

    #[test]
    fn sweep_same_start_end() {
        let config = StormConfig::default().with_pattern(StormPattern::Sweep {
            start_width: 80,
            start_height: 24,
            end_width: 80,
            end_height: 24,
            steps: 5,
        });
        let storm = ResizeStorm::new(config);
        for event in storm.events() {
            assert_eq!(event.width, 80);
            assert_eq!(event.height, 24);
        }
    }

    #[test]
    fn oscillate_zero_cycles() {
        let config = StormConfig::default().with_pattern(StormPattern::Oscillate {
            size_a: (80, 24),
            size_b: (120, 40),
            cycles: 0,
        });
        let storm = ResizeStorm::new(config);
        assert!(storm.events().is_empty());
    }

    #[test]
    fn oscillate_single_cycle() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Oscillate {
                size_a: (80, 24),
                size_b: (120, 40),
                cycles: 1,
            });
        let storm = ResizeStorm::new(config);
        assert_eq!(storm.events().len(), 2);
        assert_eq!(
            (storm.events()[0].width, storm.events()[0].height),
            (80, 24)
        );
        assert_eq!(
            (storm.events()[1].width, storm.events()[1].height),
            (120, 40)
        );
    }

    #[test]
    fn pathological_zero_count() {
        let config = StormConfig::default()
            .with_seed(1)
            .with_pattern(StormPattern::Pathological { count: 0 });
        let storm = ResizeStorm::new(config);
        assert!(storm.events().is_empty());
    }

    #[test]
    fn pathological_covers_all_8_patterns() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Pathological { count: 8 });
        let storm = ResizeStorm::new(config);
        assert_eq!(storm.events().len(), 8);

        // Pattern 0: min_width, min_height, delay=0
        assert_eq!(storm.events()[0].width, 20);
        assert_eq!(storm.events()[0].height, 5);
        assert_eq!(storm.events()[0].delay_ms, 0);

        // Pattern 1: max_width, max_height, delay=0
        assert_eq!(storm.events()[1].width, 300);
        assert_eq!(storm.events()[1].height, 100);
        assert_eq!(storm.events()[1].delay_ms, 0);

        // Pattern 2: extreme minimum 1x1
        assert_eq!(storm.events()[2].width, 1);
        assert_eq!(storm.events()[2].height, 1);

        // Pattern 3: large 500x200
        assert_eq!(storm.events()[3].width, 500);
        assert_eq!(storm.events()[3].height, 200);

        // Pattern 4: normal 80x24 with long delay
        assert_eq!(storm.events()[4].width, 80);
        assert_eq!(storm.events()[4].height, 24);
        assert_eq!(storm.events()[4].delay_ms, 500);
    }

    #[test]
    fn mixed_zero_count() {
        let config = StormConfig::default()
            .with_seed(1)
            .with_pattern(StormPattern::Mixed { count: 0 });
        let storm = ResizeStorm::new(config);
        assert!(storm.events().is_empty());
    }

    #[test]
    fn custom_empty_events() {
        let config =
            StormConfig::default().with_pattern(StormPattern::Custom { events: Vec::new() });
        let storm = ResizeStorm::new(config);
        assert!(storm.events().is_empty());
    }

    #[test]
    fn custom_preserves_order_and_indices() {
        let config = StormConfig::default().with_pattern(StormPattern::Custom {
            events: vec![(80, 24, 10), (100, 50, 20), (60, 15, 5)],
        });
        let storm = ResizeStorm::new(config);
        let events = storm.events();
        assert_eq!(events[0].index, 0);
        assert_eq!(events[1].index, 1);
        assert_eq!(events[2].index, 2);
        assert_eq!((events[2].width, events[2].height), (60, 15));
    }

    #[test]
    fn storm_run_id_is_nonempty() {
        let storm = ResizeStorm::new(StormConfig::default());
        assert!(!storm.run_id().is_empty());
    }

    #[test]
    fn storm_config_accessor() {
        let config = StormConfig::default().with_seed(77);
        let storm = ResizeStorm::new(config);
        assert_eq!(storm.config().seed, 77);
    }

    #[test]
    fn storm_total_duration_zero_delays() {
        let config = StormConfig::default().with_pattern(StormPattern::Custom {
            events: vec![(80, 24, 0), (100, 50, 0), (60, 15, 0)],
        });
        let storm = ResizeStorm::new(config);
        assert_eq!(storm.total_duration_ms(), 0);
    }

    #[test]
    fn storm_sequence_checksum_deterministic() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Burst { count: 10 });
        let storm1 = ResizeStorm::new(config.clone());
        let storm2 = ResizeStorm::new(config);
        assert_eq!(storm1.sequence_checksum(), storm2.sequence_checksum());
    }

    #[test]
    fn storm_sequence_checksum_format() {
        let storm = ResizeStorm::new(StormConfig::default().with_seed(42));
        let checksum = storm.sequence_checksum();
        assert_eq!(checksum.len(), 16, "checksum should be 16 hex chars");
        assert!(
            checksum.chars().all(|c| c.is_ascii_hexdigit()),
            "checksum should be hex"
        );
    }

    #[test]
    fn storm_debug_format() {
        let storm = ResizeStorm::new(StormConfig::default().with_seed(42));
        let debug = format!("{storm:?}");
        assert!(debug.contains("ResizeStorm"));
    }

    #[test]
    fn storm_clone() {
        let storm = ResizeStorm::new(
            StormConfig::default()
                .with_seed(42)
                .with_pattern(StormPattern::Burst { count: 5 }),
        );
        let cloned = storm.clone();
        assert_eq!(storm.events(), cloned.events());
        assert_eq!(storm.sequence_checksum(), cloned.sequence_checksum());
    }

    #[test]
    fn logger_log_capture() {
        let mut logger = StormLogger::new("test-run");
        logger.log_capture(3, 2048, "checksum123", true);
        let jsonl = logger.to_jsonl();
        assert!(jsonl.contains(r#""event":"storm_capture""#));
        assert!(jsonl.contains(r#""idx":3"#));
        assert!(jsonl.contains(r#""bytes_captured":2048"#));
        assert!(jsonl.contains(r#""checksum":"checksum123""#));
        assert!(jsonl.contains(r#""flicker_free":true"#));
    }

    #[test]
    fn logger_log_capture_flicker_false() {
        let mut logger = StormLogger::new("test-run");
        logger.log_capture(0, 512, "abc", false);
        let jsonl = logger.to_jsonl();
        assert!(jsonl.contains(r#""flicker_free":false"#));
    }

    #[test]
    fn logger_log_error() {
        let mut logger = StormLogger::new("test-run");
        logger.log_error("something went wrong");
        let jsonl = logger.to_jsonl();
        assert!(jsonl.contains(r#""event":"storm_error""#));
        assert!(jsonl.contains("something went wrong"));
    }

    #[test]
    fn logger_log_error_special_chars() {
        let mut logger = StormLogger::new("test-run");
        logger.log_error("error with \"quotes\" and \nnewline");
        let jsonl = logger.to_jsonl();
        assert!(jsonl.contains(r#"\"quotes\""#));
        assert!(jsonl.contains(r#"\n"#));
    }

    #[test]
    fn logger_empty() {
        let logger = StormLogger::new("test-run");
        let jsonl = logger.to_jsonl();
        assert!(jsonl.is_empty());
    }

    #[test]
    fn logger_line_count() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_case_name("line_test")
            .with_pattern(StormPattern::Burst { count: 3 });
        let storm = ResizeStorm::new(config);
        let mut logger = StormLogger::new(storm.run_id());
        let caps = TerminalCapabilities::default();

        logger.log_start(&storm, &caps);
        for event in storm.events() {
            logger.log_resize(event);
        }
        logger.log_complete("pass", 3, 500, "abc");

        let jsonl = logger.to_jsonl();
        let line_count = jsonl.lines().count();
        // 1 start + 3 resize + 1 complete = 5
        assert_eq!(line_count, 5);
    }

    #[test]
    fn terminal_capabilities_default() {
        let caps = TerminalCapabilities::default();
        assert_eq!(caps.term, "");
        assert_eq!(caps.colorterm, "");
        assert!(!caps.no_color);
        assert!(!caps.in_mux);
        assert!(caps.mux_name.is_none());
        assert!(!caps.sync_output);
    }

    #[test]
    fn terminal_capabilities_to_json_null_mux() {
        let caps = TerminalCapabilities {
            mux_name: None,
            ..Default::default()
        };
        let json = caps.to_json();
        assert!(json.contains(r#""mux_name":null"#));
    }

    #[test]
    fn terminal_capabilities_clone() {
        let caps = TerminalCapabilities {
            term: "xterm".to_string(),
            in_mux: true,
            mux_name: Some("tmux".to_string()),
            ..Default::default()
        };
        let cloned = caps.clone();
        assert_eq!(cloned.term, "xterm");
        assert!(cloned.in_mux);
        assert_eq!(cloned.mux_name.as_deref(), Some("tmux"));
    }

    #[test]
    fn terminal_capabilities_debug() {
        let caps = TerminalCapabilities::default();
        let debug = format!("{caps:?}");
        assert!(debug.contains("TerminalCapabilities"));
    }

    #[test]
    fn recorded_storm_record_with_output() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Burst { count: 5 });
        let storm = ResizeStorm::new(config);

        let recorded = RecordedStorm::record_with_output(&storm, "output_hash".to_string());
        assert_eq!(
            recorded.expected_output_checksum.as_deref(),
            Some("output_hash")
        );
    }

    #[test]
    fn recorded_storm_verify_replay_different_seed_fails() {
        let config1 = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Burst { count: 10 });
        let storm1 = ResizeStorm::new(config1);
        let recorded = RecordedStorm::record(&storm1);

        let config2 = StormConfig::default()
            .with_seed(99)
            .with_pattern(StormPattern::Burst { count: 10 });
        let storm2 = ResizeStorm::new(config2);

        assert!(!recorded.verify_replay(&storm2));
    }

    #[test]
    fn recorded_storm_to_json_format() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_case_name("json_test")
            .with_pattern(StormPattern::Burst { count: 2 });
        let storm = ResizeStorm::new(config);
        let recorded = RecordedStorm::record(&storm);

        let json = recorded.to_json();
        assert!(json.starts_with('{'));
        assert!(json.ends_with('}'));
        assert!(json.contains(r#""seed":42"#));
        assert!(json.contains(r#""pattern":"burst""#));
        assert!(json.contains(r#""case_name":"json_test""#));
        assert!(json.contains(r#""sequence_checksum":""#));
        assert!(json.contains(r#""expected_output_checksum":null"#));
    }

    #[test]
    fn recorded_storm_to_json_with_output_checksum() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Burst { count: 1 });
        let storm = ResizeStorm::new(config);
        let recorded = RecordedStorm::record_with_output(&storm, "deadbeef".to_string());

        let json = recorded.to_json();
        assert!(json.contains(r#""expected_output_checksum":"deadbeef""#));
    }

    #[test]
    fn recorded_storm_clone_debug() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Burst { count: 3 });
        let storm = ResizeStorm::new(config);
        let recorded = RecordedStorm::record(&storm);

        let cloned = recorded.clone();
        assert_eq!(cloned.sequence_checksum, recorded.sequence_checksum);

        let debug = format!("{recorded:?}");
        assert!(debug.contains("RecordedStorm"));
    }

    #[test]
    fn escape_json_special_chars() {
        assert_eq!(escape_json(r#"hello "world""#), r#"hello \"world\""#);
        assert_eq!(escape_json("line1\nline2"), r#"line1\nline2"#);
        assert_eq!(escape_json("tab\there"), r#"tab\there"#);
        assert_eq!(escape_json("cr\rhere"), r#"cr\rhere"#);
        assert_eq!(escape_json(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn escape_json_empty() {
        assert_eq!(escape_json(""), "");
    }

    #[test]
    fn escape_json_no_special() {
        assert_eq!(escape_json("hello world"), "hello world");
    }

    #[test]
    fn compute_output_checksum_deterministic() {
        let data = b"hello world";
        let c1 = compute_output_checksum(data);
        let c2 = compute_output_checksum(data);
        assert_eq!(c1, c2);
    }

    #[test]
    fn compute_output_checksum_format() {
        let checksum = compute_output_checksum(b"test");
        assert_eq!(checksum.len(), 16);
        assert!(checksum.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn compute_output_checksum_different_data() {
        let c1 = compute_output_checksum(b"hello");
        let c2 = compute_output_checksum(b"world");
        assert_ne!(c1, c2);
    }

    #[test]
    fn compute_output_checksum_empty() {
        let checksum = compute_output_checksum(b"");
        assert_eq!(checksum.len(), 16);
    }

    #[test]
    fn storm_result_debug() {
        let result = StormResult {
            passed: true,
            total_resizes: 10,
            total_bytes: 5000,
            duration_ms: 100,
            flicker_analysis: None,
            sequence_checksum: "abc".to_string(),
            output_checksum: "def".to_string(),
            jsonl: String::new(),
            errors: Vec::new(),
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("StormResult"));
        assert!(debug.contains("passed: true"));
    }

    #[test]
    fn burst_events_have_sequential_indices() {
        let config = StormConfig::default()
            .with_seed(42)
            .with_pattern(StormPattern::Burst { count: 10 });
        let storm = ResizeStorm::new(config);
        for (i, event) in storm.events().iter().enumerate() {
            assert_eq!(event.index, i, "event at position {i} has wrong index");
        }
    }

    #[test]
    fn sweep_midpoint_interpolation() {
        let config = StormConfig::default().with_pattern(StormPattern::Sweep {
            start_width: 80,
            start_height: 20,
            end_width: 120,
            end_height: 40,
            steps: 3,
        });
        let storm = ResizeStorm::new(config);
        let events = storm.events();

        assert_eq!(events[0].width, 80);
        assert_eq!(events[0].height, 20);
        // Midpoint: t=0.5 → 100, 30
        assert_eq!(events[1].width, 100);
        assert_eq!(events[1].height, 30);
        assert_eq!(events[2].width, 120);
        assert_eq!(events[2].height, 40);
    }

    #[test]
    fn sweep_delay_is_average_of_range() {
        let config = StormConfig::default()
            .with_delay_range(10, 50)
            .with_pattern(StormPattern::Sweep {
                start_width: 80,
                start_height: 24,
                end_width: 160,
                end_height: 48,
                steps: 3,
            });
        let storm = ResizeStorm::new(config);
        // Sweep uses (min + max) / 2 = (10 + 50) / 2 = 30
        for event in storm.events() {
            assert_eq!(event.delay_ms, 30);
        }
    }

    #[test]
    fn mixed_events_have_correct_count() {
        for count in [4, 12, 40, 100] {
            let config = StormConfig::default()
                .with_seed(42)
                .with_pattern(StormPattern::Mixed { count });
            let storm = ResizeStorm::new(config);
            assert_eq!(
                storm.events().len(),
                count,
                "mixed pattern should produce exactly {count} events"
            );
        }
    }

    #[test]
    fn logger_log_complete_fields() {
        let mut logger = StormLogger::new("run-123");
        logger.log_complete("fail", 25, 10000, "xyz789");
        let jsonl = logger.to_jsonl();
        assert!(jsonl.contains(r#""event":"storm_complete""#));
        assert!(jsonl.contains(r#""outcome":"fail""#));
        assert!(jsonl.contains(r#""total_resizes":25"#));
        assert!(jsonl.contains(r#""total_bytes":10000"#));
        assert!(jsonl.contains(r#""checksum":"xyz789""#));
        assert!(jsonl.contains(r#""duration_ms":"#));
    }

    #[test]
    fn logger_log_start_includes_pattern_and_event_count() {
        let config = StormConfig::default()
            .with_seed(7)
            .with_case_name("start_case")
            .with_pattern(StormPattern::Burst { count: 3 });
        let storm = ResizeStorm::new(config);
        let mut logger = StormLogger::new(storm.run_id());
        let caps = TerminalCapabilities::default();

        logger.log_start(&storm, &caps);
        let jsonl = logger.to_jsonl();

        assert!(jsonl.contains(r#""event":"storm_start""#));
        assert!(jsonl.contains(r#""case":"start_case""#));
        assert!(jsonl.contains(r#""pattern":"burst""#));
        assert!(jsonl.contains(r#""event_count":3"#));
        assert!(jsonl.contains(r#""capabilities":"#));
    }

    #[test]
    fn logger_log_resize_uses_event_jsonl_shape() {
        let mut logger = StormLogger::new("run-resize");
        let event = ResizeEvent::new(111, 37, 12, 4);
        logger.log_resize(&event);

        let jsonl = logger.to_jsonl();
        assert!(jsonl.contains(r#""event":"storm_resize""#));
        assert!(jsonl.contains(r#""idx":4"#));
        assert!(jsonl.contains(r#""width":111"#));
        assert!(jsonl.contains(r#""height":37"#));
        assert!(jsonl.contains(r#""delay_ms":12"#));
        assert!(jsonl.contains(r#""elapsed_ms":"#));
    }

    #[test]
    fn logger_write_to_file_roundtrip() {
        let mut logger = StormLogger::new("run-file");
        logger.log_error("disk test");

        let path = std::env::temp_dir().join(format!(
            "resize_storm_logger_{}_{}.jsonl",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        logger
            .write_to_file(&path)
            .expect("write_to_file should succeed");
        let body = std::fs::read_to_string(&path).expect("file should be readable");
        let _ = std::fs::remove_file(&path);

        assert!(body.contains(r#""event":"storm_error""#));
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn terminal_capabilities_to_json_escapes_special_chars() {
        let caps = TerminalCapabilities {
            term: "xterm\"weird".to_string(),
            colorterm: "line1\nline2".to_string(),
            no_color: false,
            in_mux: true,
            mux_name: Some("tmux\\session".to_string()),
            sync_output: true,
        };

        let json = caps.to_json();
        assert!(json.contains(r#""term":"xterm\"weird""#));
        assert!(json.contains(r#""colorterm":"line1\nline2""#));
        assert!(json.contains(r#""mux_name":"tmux\\session""#));
    }

    #[test]
    fn recorded_storm_record_copies_config_events_and_checksum() {
        let config = StormConfig::default()
            .with_seed(123)
            .with_case_name("record-copy")
            .with_pattern(StormPattern::Burst { count: 6 });
        let storm = ResizeStorm::new(config);
        let recorded = RecordedStorm::record(&storm);

        assert_eq!(recorded.config.seed, 123);
        assert_eq!(recorded.config.case_name, "record-copy");
        assert_eq!(recorded.events, storm.events);
        assert_eq!(recorded.sequence_checksum, storm.sequence_checksum());
        assert!(recorded.expected_output_checksum.is_none());
    }

    #[test]
    fn compute_output_checksum_binary_payload_is_stable() {
        let bytes = [0_u8, 255, 17, 42, b'\n', b'"', b'\\'];
        let first = compute_output_checksum(&bytes);
        let second = compute_output_checksum(&bytes);
        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
    }
}
