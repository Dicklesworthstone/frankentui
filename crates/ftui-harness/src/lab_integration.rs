#![forbid(unsafe_code)]

//! FrankenLab integration: deterministic model simulation with LabScenario.
//!
//! Bridges [`LabScenario`] (seed-controlled
//! scheduling, JSONL logging, tracing spans) with
//! [`ProgramSimulator`] (headless
//! model execution, frame capture).
//!
//! # Design
//!
//! [`Lab`] is the entry point. It creates a [`LabSession`] that wraps a
//! `ProgramSimulator<M>` with deterministic time, structured logging, and
//! frame-checksum recording for replay verification.
//!
//! # Example
//!
//! ```ignore
//! use ftui_harness::lab_integration::{Lab, LabConfig};
//!
//! let config = LabConfig::new("my_test", "theme_switch", 42)
//!     .viewport(80, 24)
//!     .time_step_ms(16);
//!
//! let run = Lab::run_scenario(config, MyModel::new(), |session| {
//!     session.init();
//!     session.send(MyMsg::SwitchTheme);
//!     session.tick();
//!     session.capture_frame();
//!     session.send(MyMsg::SwitchTheme);
//!     session.tick();
//!     session.capture_frame();
//! });
//!
//! assert!(run.result.deterministic);
//! assert_eq!(run.output.frame_count, 2);
//! // Replay: same seed → identical checksums
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use crate::determinism::{JsonValue, LabScenario, LabScenarioRun, TestJsonlLogger};
use ftui_core::event::Event;
use ftui_render::buffer::Buffer;
use ftui_runtime::program::Model;
use ftui_runtime::simulator::ProgramSimulator;
use tracing::info_span;

/// Global counter for recordings created.
static LAB_RECORDINGS_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Global counter for replays executed.
static LAB_REPLAYS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Read the total number of recordings created in-process.
#[must_use]
pub fn lab_recordings_total() -> u64 {
    LAB_RECORDINGS_TOTAL.load(Ordering::Relaxed)
}

/// Read the total number of replays executed in-process.
#[must_use]
pub fn lab_replays_total() -> u64 {
    LAB_REPLAYS_TOTAL.load(Ordering::Relaxed)
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for a FrankenLab scenario run.
#[derive(Debug, Clone)]
pub struct LabConfig {
    /// Prefix for JSONL logger and run IDs.
    pub prefix: String,
    /// Scenario name (used in tracing spans and JSONL).
    pub scenario_name: String,
    /// Deterministic seed.
    pub seed: u64,
    /// Viewport width for frame captures.
    pub viewport_width: u16,
    /// Viewport height for frame captures.
    pub viewport_height: u16,
    /// Time step in milliseconds for the deterministic clock.
    pub time_step_ms: u64,
    /// Whether to log each captured frame's checksum to JSONL.
    pub log_frame_checksums: bool,
}

impl LabConfig {
    /// Create a new configuration with defaults.
    ///
    /// Defaults: 80x24 viewport, 16ms time step, frame checksum logging on.
    pub fn new(prefix: &str, scenario_name: &str, seed: u64) -> Self {
        Self {
            prefix: prefix.to_string(),
            scenario_name: scenario_name.to_string(),
            seed,
            viewport_width: 80,
            viewport_height: 24,
            time_step_ms: 16,
            log_frame_checksums: true,
        }
    }

    /// Set the viewport dimensions for frame captures.
    #[must_use]
    pub fn viewport(mut self, width: u16, height: u16) -> Self {
        self.viewport_width = width;
        self.viewport_height = height;
        self
    }

    /// Set the deterministic time step in milliseconds.
    #[must_use]
    pub fn time_step_ms(mut self, ms: u64) -> Self {
        self.time_step_ms = ms;
        self
    }

    /// Enable or disable JSONL logging of frame checksums.
    #[must_use]
    pub fn log_frame_checksums(mut self, enabled: bool) -> Self {
        self.log_frame_checksums = enabled;
        self
    }
}

// ============================================================================
// Session (mutable handle passed to scenario closures)
// ============================================================================

/// A frame checksum record for replay verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameRecord {
    /// Frame index (0-based).
    pub index: usize,
    /// Deterministic timestamp when the frame was captured.
    pub timestamp_ms: u64,
    /// FNV-1a checksum of the frame buffer cells.
    pub checksum: u64,
}

/// Record of an injected event for ordering verification.
#[derive(Debug, Clone)]
pub struct EventRecord {
    /// Deterministic timestamp when the event was injected.
    pub timestamp_ms: u64,
    /// Sequential index of this event.
    pub sequence: u64,
    /// Human-readable label for the event kind.
    pub kind: String,
}

/// Active FrankenLab session wrapping a `ProgramSimulator`.
///
/// Provides deterministic time injection, structured event logging, and
/// frame-checksum recording. All operations are logged to JSONL via the
/// underlying [`TestJsonlLogger`].
pub struct LabSession<M: Model> {
    sim: ProgramSimulator<M>,
    logger: TestJsonlLogger,
    viewport_width: u16,
    viewport_height: u16,
    log_frame_checksums: bool,
    frame_records: Vec<FrameRecord>,
    event_log: Vec<EventRecord>,
    tick_count: u64,
    anomaly_count: u64,
    last_event_ms: u64,
}

impl<M: Model> LabSession<M> {
    /// Initialize the model (calls `Model::init()`).
    ///
    /// Should be called once before injecting events or capturing frames.
    pub fn init(&mut self) {
        self.sim.init();
        self.logger.log(
            "lab.session.init",
            &[(
                "viewport",
                JsonValue::raw(format!(
                    "[{},{}]",
                    self.viewport_width, self.viewport_height
                )),
            )],
        );
    }

    /// Send a message directly to the model.
    pub fn send(&mut self, msg: M::Message) {
        let now_ms = self.logger.fixture().now_ms();
        self.check_time_ordering(now_ms, "send");
        let seq = self.event_log.len() as u64;
        self.event_log.push(EventRecord {
            timestamp_ms: now_ms,
            sequence: seq,
            kind: "message".to_string(),
        });
        self.sim.send(msg);
    }

    /// Inject a terminal event into the model.
    pub fn inject_event(&mut self, event: Event) {
        let now_ms = self.logger.fixture().now_ms();
        self.check_time_ordering(now_ms, "inject_event");
        let seq = self.event_log.len() as u64;
        let kind = event_kind_label(&event);
        self.event_log.push(EventRecord {
            timestamp_ms: now_ms,
            sequence: seq,
            kind,
        });
        self.sim.inject_event(event);
    }

    /// Inject multiple terminal events in order.
    pub fn inject_events(&mut self, events: &[Event]) {
        for event in events {
            self.inject_event(event.clone());
        }
    }

    /// Simulate a tick event (deterministic time advance).
    ///
    /// Injects `Event::Tick` into the model. The deterministic clock advances
    /// by `time_step_ms` for each call to `now_ms()`.
    pub fn tick(&mut self) {
        let now_ms = self.logger.fixture().now_ms();
        self.check_time_ordering(now_ms, "tick");
        self.tick_count += 1;
        let seq = self.event_log.len() as u64;
        self.event_log.push(EventRecord {
            timestamp_ms: now_ms,
            sequence: seq,
            kind: "tick".to_string(),
        });
        self.sim.inject_event(Event::Tick);
    }

    /// Capture the current frame at the configured viewport dimensions.
    ///
    /// Records a checksum for replay verification and optionally logs it
    /// to JSONL.
    pub fn capture_frame(&mut self) -> &Buffer {
        let w = self.viewport_width;
        let h = self.viewport_height;
        self.capture_frame_inner(w, h)
    }

    /// Capture a frame at custom dimensions (overriding the configured viewport).
    pub fn capture_frame_at(&mut self, width: u16, height: u16) -> &Buffer {
        self.capture_frame_inner(width, height)
    }

    fn capture_frame_inner(&mut self, width: u16, height: u16) -> &Buffer {
        let now_ms = self.logger.fixture().now_ms();
        let buf = self.sim.capture_frame(width, height);
        let checksum = fnv1a_buffer(buf);
        let index = self.frame_records.len();

        self.frame_records.push(FrameRecord {
            index,
            timestamp_ms: now_ms,
            checksum,
        });

        if self.log_frame_checksums {
            self.logger.log(
                "lab.frame",
                &[
                    ("frame_idx", JsonValue::u64(index as u64)),
                    ("checksum", JsonValue::str(format!("{checksum:016x}"))),
                    ("timestamp_ms", JsonValue::u64(now_ms)),
                    ("width", JsonValue::u64(width as u64)),
                    ("height", JsonValue::u64(height as u64)),
                ],
            );
        }

        self.sim.last_frame().expect("frame just captured")
    }

    /// Access the underlying model.
    pub fn model(&self) -> &M {
        self.sim.model()
    }

    /// Access the underlying model mutably.
    pub fn model_mut(&mut self) -> &mut M {
        self.sim.model_mut()
    }

    /// Check if the simulated program is still running.
    pub fn is_running(&self) -> bool {
        self.sim.is_running()
    }

    /// Get all frame checksum records.
    pub fn frame_records(&self) -> &[FrameRecord] {
        &self.frame_records
    }

    /// Get all event records (for ordering verification).
    pub fn event_log(&self) -> &[EventRecord] {
        &self.event_log
    }

    /// Number of ticks injected.
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Number of scheduling anomalies detected.
    pub fn anomaly_count(&self) -> u64 {
        self.anomaly_count
    }

    /// All captured frame buffers.
    pub fn frames(&self) -> &[Buffer] {
        self.sim.frames()
    }

    /// Most recently captured frame.
    pub fn last_frame(&self) -> Option<&Buffer> {
        self.sim.last_frame()
    }

    /// Logs emitted via `Cmd::Log`.
    pub fn logs(&self) -> &[String] {
        self.sim.logs()
    }

    /// Underlying simulator command log.
    pub fn command_log(&self) -> &[ftui_runtime::simulator::CmdRecord] {
        self.sim.command_log()
    }

    /// Access the grapheme pool for text extraction.
    pub fn pool(&self) -> &ftui_render::grapheme_pool::GraphemePool {
        self.sim.pool()
    }

    /// Deterministic monotonic time from the fixture.
    pub fn now_ms(&self) -> u64 {
        self.logger.fixture().now_ms()
    }

    /// Log a custom info event via the JSONL logger.
    pub fn log_info(&self, event: &str, fields: &[(&str, JsonValue)]) {
        self.logger.log(event, fields);
    }

    /// Log a warning (scheduling anomaly or custom) event.
    pub fn log_warn(&self, anomaly: &str, detail: &str) {
        self.logger.log(
            "lab.session.warn",
            &[
                ("anomaly", JsonValue::str(anomaly)),
                ("detail", JsonValue::str(detail)),
            ],
        );
    }

    // ── Internal ─────────────────────────────────────────────────────

    fn check_time_ordering(&mut self, now_ms: u64, operation: &str) {
        if now_ms < self.last_event_ms {
            self.anomaly_count += 1;
            self.logger.log(
                "lab.session.warn",
                &[
                    ("anomaly", JsonValue::str("time_ordering")),
                    (
                        "detail",
                        JsonValue::str(format!(
                            "{operation}: time went backwards ({now_ms} < {})",
                            self.last_event_ms
                        )),
                    ),
                ],
            );
        }
        self.last_event_ms = now_ms;
    }

    fn into_output(self) -> LabOutput {
        LabOutput {
            frame_count: self.frame_records.len(),
            frame_records: self.frame_records,
            event_count: self.event_log.len(),
            event_log: self.event_log,
            tick_count: self.tick_count,
            anomaly_count: self.anomaly_count,
        }
    }
}

// ============================================================================
// Output
// ============================================================================

/// Summary output from a FrankenLab scenario run.
#[derive(Debug, Clone)]
pub struct LabOutput {
    /// Number of frames captured.
    pub frame_count: usize,
    /// Frame checksum records for replay verification.
    pub frame_records: Vec<FrameRecord>,
    /// Number of events injected.
    pub event_count: usize,
    /// Event log for ordering verification.
    pub event_log: Vec<EventRecord>,
    /// Number of ticks injected.
    pub tick_count: u64,
    /// Number of scheduling anomalies detected.
    pub anomaly_count: u64,
}

// ============================================================================
// Lab entry point
// ============================================================================

/// FrankenLab — deterministic model testing harness.
///
/// Combines [`LabScenario`] (seed, logging, tracing spans, metrics) with
/// [`ProgramSimulator`] (headless model execution) into a single API.
pub struct Lab;

impl Lab {
    /// Run a deterministic scenario with a model.
    ///
    /// Creates a [`LabScenario`] for the outer tracing span (including
    /// `lab.scenario` with `scenario_name`, `seed`, `event_count`,
    /// `duration_us` fields) and `lab_scenarios_run_total` metric counter.
    ///
    /// The closure receives a [`LabSession`] that wraps a `ProgramSimulator`
    /// with deterministic time injection, JSONL logging, and frame checksums.
    pub fn run_scenario<M: Model>(
        config: LabConfig,
        model: M,
        run: impl FnOnce(&mut LabSession<M>),
    ) -> LabScenarioRun<LabOutput> {
        let scenario = LabScenario::new_with(
            &config.prefix,
            &config.scenario_name,
            config.seed,
            true, // always deterministic
            config.time_step_ms,
        );

        // LabScenario::run() handles the outer span + start/end JSONL +
        // lab_scenarios_run_total counter. Inside, we create a session-level
        // logger with the same determinism settings for frame/event logging.
        scenario.run(|_ctx| {
            let mut session_logger = TestJsonlLogger::new_with(
                &format!("{}_session", config.prefix),
                config.seed,
                true,
                config.time_step_ms,
            );
            session_logger.add_context_str("scenario_name", &config.scenario_name);

            let mut session = LabSession {
                sim: ProgramSimulator::new(model),
                logger: session_logger,
                viewport_width: config.viewport_width,
                viewport_height: config.viewport_height,
                log_frame_checksums: config.log_frame_checksums,
                frame_records: Vec::new(),
                event_log: Vec::new(),
                tick_count: 0,
                anomaly_count: 0,
                last_event_ms: 0,
            };

            run(&mut session);
            session.into_output()
        })
    }

    /// Verify determinism with a custom scenario closure.
    ///
    /// Runs `scenario_fn` twice with the same seed and model, asserting
    /// frame checksum equality.
    ///
    /// # Panics
    ///
    /// Panics if frame counts differ or any checksum mismatches.
    pub fn assert_deterministic_with<M, MF, SF>(
        config: LabConfig,
        model_factory: MF,
        scenario_fn: SF,
    ) -> LabOutput
    where
        M: Model,
        MF: Fn() -> M,
        SF: Fn(&mut LabSession<M>),
    {
        let run1 = Self::run_scenario(config.clone(), model_factory(), |s| scenario_fn(s));
        let run2 = Self::run_scenario(config, model_factory(), |s| scenario_fn(s));

        assert_eq!(
            run1.output.frame_count, run2.output.frame_count,
            "frame count mismatch between identical-seed runs"
        );

        for (i, (a, b)) in run1
            .output
            .frame_records
            .iter()
            .zip(run2.output.frame_records.iter())
            .enumerate()
        {
            assert_eq!(
                a.checksum, b.checksum,
                "frame {i} checksum mismatch: run1={:016x}, run2={:016x}",
                a.checksum, b.checksum
            );
        }

        run1.output
    }
}

// ============================================================================
// Recording / Replay
// ============================================================================

/// A captured recording of a deterministic scenario run.
///
/// Contains the configuration, frame checksums, and event log from a single
/// run. Can be replayed with [`Lab::replay`] to verify determinism.
#[derive(Debug, Clone)]
pub struct Recording {
    /// Configuration used for this recording.
    pub config: LabConfig,
    /// Scenario metadata from the recording run.
    pub scenario_name: String,
    /// Seed used for recording.
    pub seed: u64,
    /// Frame checksum records captured during the recording.
    pub frame_records: Vec<FrameRecord>,
    /// Event log captured during the recording.
    pub event_log: Vec<EventRecord>,
    /// Number of ticks in the recorded scenario.
    pub tick_count: u64,
    /// Run identifier for the recording.
    pub run_id: String,
}

/// Result of replaying a recording against a new model instance.
#[derive(Debug, Clone)]
pub struct ReplayResult {
    /// Whether the replay matched the recording (no divergence).
    pub matched: bool,
    /// Frame checksum records from the replay run.
    pub replay_frame_records: Vec<FrameRecord>,
    /// Index of the first divergent frame, if any.
    pub first_divergence: Option<usize>,
    /// Descriptive summary of any divergence found.
    pub divergence_detail: Option<String>,
    /// Number of frames compared.
    pub frames_compared: usize,
}

impl Lab {
    /// Record a deterministic scenario run.
    ///
    /// Executes the scenario closure and captures frame checksums and event
    /// ordering into a [`Recording`] that can later be replayed with
    /// [`Lab::replay`].
    ///
    /// Emits a `lab.record` tracing span.
    pub fn record<M: Model>(
        config: LabConfig,
        model: M,
        run: impl FnOnce(&mut LabSession<M>),
    ) -> Recording {
        let _span = info_span!(
            "lab.record",
            scenario_name = config.scenario_name.as_str(),
            seed = config.seed,
        )
        .entered();

        let scenario_run = Self::run_scenario(config.clone(), model, |session| {
            session.log_info(
                "lab.record.start",
                &[
                    ("scenario_name", JsonValue::str(&config.scenario_name)),
                    ("seed", JsonValue::u64(config.seed)),
                ],
            );
            run(session);
            session.log_info(
                "lab.record.stop",
                &[
                    (
                        "frame_count",
                        JsonValue::u64(session.frame_records().len() as u64),
                    ),
                    (
                        "event_count",
                        JsonValue::u64(session.event_log().len() as u64),
                    ),
                ],
            );
        });

        LAB_RECORDINGS_TOTAL.fetch_add(1, Ordering::Relaxed);

        Recording {
            scenario_name: scenario_run.result.scenario_name.clone(),
            seed: scenario_run.result.seed,
            run_id: scenario_run.result.run_id.clone(),
            frame_records: scenario_run.output.frame_records,
            event_log: scenario_run.output.event_log,
            tick_count: scenario_run.output.tick_count,
            config,
        }
    }

    /// Replay a recording with a new model instance.
    ///
    /// Re-runs the same scenario closure with the same seed/config from the
    /// recording and compares frame checksums. Returns a [`ReplayResult`]
    /// indicating whether the replay matched.
    ///
    /// Emits a `lab.replay` tracing span. Logs WARN for any divergence.
    pub fn replay<M: Model>(
        recording: &Recording,
        model: M,
        run: impl FnOnce(&mut LabSession<M>),
    ) -> ReplayResult {
        let _span = info_span!(
            "lab.replay",
            scenario_name = recording.scenario_name.as_str(),
            seed = recording.seed,
            recording_run_id = recording.run_id.as_str(),
        )
        .entered();

        let replay_run = Self::run_scenario(recording.config.clone(), model, |session| {
            session.log_info(
                "lab.replay.start",
                &[
                    ("scenario_name", JsonValue::str(&recording.scenario_name)),
                    ("seed", JsonValue::u64(recording.seed)),
                    ("recording_run_id", JsonValue::str(&recording.run_id)),
                    (
                        "expected_frames",
                        JsonValue::u64(recording.frame_records.len() as u64),
                    ),
                ],
            );
            run(session);
        });

        LAB_REPLAYS_TOTAL.fetch_add(1, Ordering::Relaxed);

        let replay_frames = &replay_run.output.frame_records;
        let recorded_frames = &recording.frame_records;

        let frames_compared = recorded_frames.len().min(replay_frames.len());
        let mut first_divergence = None;
        let mut divergence_detail = None;

        // Check frame count match
        if recorded_frames.len() != replay_frames.len() {
            first_divergence = Some(frames_compared);
            divergence_detail = Some(format!(
                "frame count mismatch: recorded={}, replayed={}",
                recorded_frames.len(),
                replay_frames.len()
            ));
        }

        // Check individual frame checksums
        for i in 0..frames_compared {
            if recorded_frames[i].checksum != replay_frames[i].checksum {
                if first_divergence.is_none() {
                    first_divergence = Some(i);
                    divergence_detail = Some(format!(
                        "frame {i} checksum mismatch: recorded={:016x}, replayed={:016x}",
                        recorded_frames[i].checksum, replay_frames[i].checksum
                    ));
                }
                break;
            }
        }

        let matched = first_divergence.is_none();

        ReplayResult {
            matched,
            replay_frame_records: replay_run.output.frame_records,
            first_divergence,
            divergence_detail,
            frames_compared,
        }
    }

    /// Record and immediately replay, asserting determinism.
    ///
    /// Convenience method: runs the scenario twice (once to record, once to
    /// replay) and panics if any frame checksum diverges.
    ///
    /// # Panics
    ///
    /// Panics on any divergence between recording and replay.
    pub fn assert_replay_deterministic<M, MF, SF>(
        config: LabConfig,
        model_factory: MF,
        scenario_fn: SF,
    ) -> Recording
    where
        M: Model,
        MF: Fn() -> M,
        SF: Fn(&mut LabSession<M>),
    {
        let recording = Self::record(config, model_factory(), |s| scenario_fn(s));
        let result = Self::replay(&recording, model_factory(), |s| scenario_fn(s));

        if !result.matched {
            let detail = result
                .divergence_detail
                .unwrap_or_else(|| "unknown divergence".to_string());
            panic!(
                "replay diverged from recording (seed={}, scenario={}): {}",
                recording.seed, recording.scenario_name, detail
            );
        }

        recording
    }
}

/// Assert that two [`LabOutput`]s are frame-identical.
///
/// Compares frame counts and all checksums. Panics with a descriptive
/// message on the first mismatch.
pub fn assert_outputs_match(a: &LabOutput, b: &LabOutput) {
    assert_eq!(
        a.frame_count, b.frame_count,
        "frame count mismatch: a={}, b={}",
        a.frame_count, b.frame_count
    );
    for (i, (fa, fb)) in a
        .frame_records
        .iter()
        .zip(b.frame_records.iter())
        .enumerate()
    {
        assert_eq!(
            fa.checksum, fb.checksum,
            "frame {i} checksum mismatch: a={:016x}, b={:016x}",
            fa.checksum, fb.checksum
        );
    }
}

// ============================================================================
// FNV-1a checksum (matches trace_replay.rs)
// ============================================================================

/// FNV-1a checksum over buffer cell data (delegates to [`ftui_render::buffer::buffer_checksum`]).
#[inline]
fn fnv1a_buffer(buf: &Buffer) -> u64 {
    ftui_render::buffer::buffer_checksum(buf)
}

/// Human-readable label for an event kind.
fn event_kind_label(event: &Event) -> String {
    match event {
        Event::Key(k) => format!("key:{:?}", k.code),
        Event::Resize { width, height } => format!("resize:{width}x{height}"),
        Event::Mouse(m) => format!("mouse:{:?}", m.kind),
        Event::Tick => "tick".to_string(),
        _ => "other".to_string(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_render::frame::Frame;

    // ── Test model ───────────────────────────────────────────────────

    struct Counter {
        value: i32,
    }

    #[derive(Debug)]
    enum CounterMsg {
        Increment,
        Decrement,
        Tick,
        Quit,
    }

    impl From<Event> for CounterMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Key(k) if k.code == KeyCode::Char('+') => CounterMsg::Increment,
                Event::Key(k) if k.code == KeyCode::Char('-') => CounterMsg::Decrement,
                Event::Key(k) if k.code == KeyCode::Char('q') => CounterMsg::Quit,
                Event::Tick => CounterMsg::Tick,
                _ => CounterMsg::Tick,
            }
        }
    }

    impl Model for Counter {
        type Message = CounterMsg;

        fn init(&mut self) -> ftui_runtime::program::Cmd<Self::Message> {
            ftui_runtime::program::Cmd::none()
        }

        fn update(&mut self, msg: Self::Message) -> ftui_runtime::program::Cmd<Self::Message> {
            match msg {
                CounterMsg::Increment => {
                    self.value += 1;
                    ftui_runtime::program::Cmd::none()
                }
                CounterMsg::Decrement => {
                    self.value -= 1;
                    ftui_runtime::program::Cmd::none()
                }
                CounterMsg::Tick => ftui_runtime::program::Cmd::none(),
                CounterMsg::Quit => ftui_runtime::program::Cmd::quit(),
            }
        }

        fn view(&self, frame: &mut Frame) {
            let text = format!("Count: {}", self.value);
            for (i, c) in text.chars().enumerate() {
                if (i as u16) < frame.width() {
                    use ftui_render::cell::Cell;
                    frame.buffer.set_raw(i as u16, 0, Cell::from_char(c));
                }
            }
        }
    }

    fn key_event(c: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    // ── Tests ────────────────────────────────────────────────────────

    #[test]
    fn run_scenario_basic() {
        let config = LabConfig::new("test", "basic", 42).viewport(20, 5);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.send(CounterMsg::Increment);
            s.capture_frame();
        });

        assert_eq!(run.output.frame_count, 1);
        assert!(run.result.deterministic);
        assert_eq!(run.result.seed, 42);
    }

    #[test]
    fn deterministic_checksums_across_runs() {
        let checksums1 = {
            let config = LabConfig::new("test", "det_check", 99).viewport(20, 5);
            let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
                s.init();
                for _ in 0..5 {
                    s.send(CounterMsg::Increment);
                    s.tick();
                    s.capture_frame();
                }
            });
            run.output
                .frame_records
                .iter()
                .map(|f| f.checksum)
                .collect::<Vec<_>>()
        };

        let checksums2 = {
            let config = LabConfig::new("test", "det_check", 99).viewport(20, 5);
            let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
                s.init();
                for _ in 0..5 {
                    s.send(CounterMsg::Increment);
                    s.tick();
                    s.capture_frame();
                }
            });
            run.output
                .frame_records
                .iter()
                .map(|f| f.checksum)
                .collect::<Vec<_>>()
        };

        assert_eq!(checksums1, checksums2);
    }

    #[test]
    fn different_seeds_produce_different_metadata() {
        let run1 = Lab::run_scenario(
            LabConfig::new("test", "seed_diff", 1),
            Counter { value: 0 },
            |s| {
                s.init();
                s.capture_frame();
            },
        );

        let run2 = Lab::run_scenario(
            LabConfig::new("test", "seed_diff", 2),
            Counter { value: 0 },
            |s| {
                s.init();
                s.capture_frame();
            },
        );

        assert_ne!(run1.result.run_id, run2.result.run_id);
        assert_ne!(run1.result.seed, run2.result.seed);
    }

    #[test]
    fn event_ordering_is_tracked() {
        let config = LabConfig::new("test", "event_order", 42).viewport(20, 5);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.tick();
            s.inject_event(key_event('+'));
            s.capture_frame();
        });

        assert_eq!(run.output.event_count, 3);
        let log = &run.output.event_log;
        assert_eq!(log[0].kind, "message");
        assert_eq!(log[1].kind, "tick");
        assert_eq!(log[2].kind, "key:Char('+')");
        // Timestamps should be monotonically non-decreasing
        for w in log.windows(2) {
            assert!(w[1].timestamp_ms >= w[0].timestamp_ms);
        }
    }

    #[test]
    fn tick_count_is_tracked() {
        let config = LabConfig::new("test", "tick_count", 42);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.tick();
            s.tick();
            s.tick();
            s.capture_frame();
        });

        assert_eq!(run.output.tick_count, 3);
    }

    #[test]
    fn model_access_works() {
        let config = LabConfig::new("test", "model_access", 42);
        Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.send(CounterMsg::Increment);
            assert_eq!(s.model().value, 2);
        });
    }

    #[test]
    fn quit_stops_session() {
        let config = LabConfig::new("test", "quit_test", 42);
        Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.send(CounterMsg::Quit);
            assert!(!s.is_running());
        });
    }

    #[test]
    fn custom_viewport_in_frame() {
        let config = LabConfig::new("test", "custom_viewport", 42).viewport(40, 10);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.capture_frame_at(100, 50);
            s.capture_frame(); // uses default 40x10
        });

        assert_eq!(run.output.frame_count, 2);
    }

    #[test]
    fn no_anomalies_in_normal_usage() {
        let config = LabConfig::new("test", "no_anomalies", 42);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            for _ in 0..10 {
                s.send(CounterMsg::Increment);
                s.tick();
                s.capture_frame();
            }
        });

        assert_eq!(run.output.anomaly_count, 0);
    }

    #[test]
    fn frame_checksums_change_with_state() {
        let config = LabConfig::new("test", "checksum_changes", 42).viewport(20, 5);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.capture_frame(); // value=0
            s.send(CounterMsg::Increment);
            s.capture_frame(); // value=1
        });

        assert_eq!(run.output.frame_count, 2);
        let records = &run.output.frame_records;
        assert_ne!(
            records[0].checksum, records[1].checksum,
            "different model states should produce different checksums"
        );
    }

    #[test]
    fn assert_deterministic_with_custom_scenario() {
        let config = LabConfig::new("test", "det_custom", 42).viewport(20, 5);
        let output = Lab::assert_deterministic_with(
            config,
            || Counter { value: 0 },
            |s| {
                s.init();
                for i in 0..5 {
                    if i % 2 == 0 {
                        s.send(CounterMsg::Increment);
                    } else {
                        s.send(CounterMsg::Decrement);
                    }
                    s.capture_frame();
                }
            },
        );
        assert_eq!(output.frame_count, 5);
    }

    #[test]
    fn inject_events_batch() {
        let config = LabConfig::new("test", "inject_batch", 42).viewport(20, 5);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.inject_events(&[key_event('+'), key_event('+'), key_event('-')]);
            s.capture_frame();
        });

        assert_eq!(run.output.event_count, 3);
    }

    #[test]
    fn log_info_and_warn_accessible() {
        let config = LabConfig::new("test", "logging", 42);
        Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.log_info("custom.event", &[("key", JsonValue::str("value"))]);
            s.log_warn("test_anomaly", "simulated warning");
        });
        // Verify these don't panic.
    }

    #[test]
    fn fnv1a_buffer_deterministic() {
        let buf1 = {
            let mut b = Buffer::new(5, 3);
            b.set_raw(0, 0, ftui_render::cell::Cell::from_char('A'));
            b.set_raw(1, 0, ftui_render::cell::Cell::from_char('B'));
            b
        };
        let buf2 = {
            let mut b = Buffer::new(5, 3);
            b.set_raw(0, 0, ftui_render::cell::Cell::from_char('A'));
            b.set_raw(1, 0, ftui_render::cell::Cell::from_char('B'));
            b
        };
        assert_eq!(fnv1a_buffer(&buf1), fnv1a_buffer(&buf2));

        // Different content → different checksum
        let buf3 = {
            let mut b = Buffer::new(5, 3);
            b.set_raw(0, 0, ftui_render::cell::Cell::from_char('X'));
            b
        };
        assert_ne!(fnv1a_buffer(&buf1), fnv1a_buffer(&buf3));
    }

    #[test]
    fn multi_seed_determinism_100_seeds() {
        for seed in 0..100 {
            let checksums1 = {
                let config = LabConfig::new("test", "multi_seed", seed).viewport(20, 5);
                let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
                    s.init();
                    s.send(CounterMsg::Increment);
                    s.tick();
                    s.capture_frame();
                });
                run.output
                    .frame_records
                    .iter()
                    .map(|f| f.checksum)
                    .collect::<Vec<_>>()
            };
            let checksums2 = {
                let config = LabConfig::new("test", "multi_seed", seed).viewport(20, 5);
                let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
                    s.init();
                    s.send(CounterMsg::Increment);
                    s.tick();
                    s.capture_frame();
                });
                run.output
                    .frame_records
                    .iter()
                    .map(|f| f.checksum)
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                checksums1, checksums2,
                "seed {seed}: checksums diverged between runs"
            );
        }
    }

    #[test]
    fn scenario_metadata_is_correct() {
        let config = LabConfig::new("meta_test", "my_scenario", 777)
            .viewport(40, 10)
            .time_step_ms(8);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.tick();
            s.capture_frame();
        });

        assert_eq!(run.result.scenario_name, "my_scenario");
        assert_eq!(run.result.seed, 777);
        assert!(run.result.deterministic);
        assert!(run.result.run_total >= 1);
    }

    #[test]
    fn event_timestamps_advance_with_time_step() {
        let config = LabConfig::new("test", "ts_advance", 42).time_step_ms(100);
        let run = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment); // consumes 1 now_ms() call
            s.send(CounterMsg::Increment); // consumes 1 now_ms() call
            s.send(CounterMsg::Increment); // consumes 1 now_ms() call
        });

        let log = &run.output.event_log;
        assert_eq!(log.len(), 3);
        // Each send() calls now_ms() which advances the clock by time_step_ms
        // DeterminismFixture::now_ms() returns fetch_add(step) + step
        // So first call returns step, second returns 2*step, etc.
        assert!(log[0].timestamp_ms <= log[1].timestamp_ms);
        assert!(log[1].timestamp_ms <= log[2].timestamp_ms);
    }

    #[test]
    fn session_now_ms_advances() {
        let config = LabConfig::new("test", "now_ms", 42).time_step_ms(50);
        Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            let t0 = s.now_ms();
            let t1 = s.now_ms();
            assert!(t1 > t0, "now_ms should advance: t0={t0}, t1={t1}");
        });
    }

    #[test]
    fn command_log_accessible() {
        let config = LabConfig::new("test", "cmd_log", 42);
        Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.send(CounterMsg::Quit);
            let log = s.command_log();
            assert!(!log.is_empty());
        });
    }

    // ── Recording / Replay tests ─────────────────────────────────────

    #[test]
    fn record_captures_frame_checksums() {
        let config = LabConfig::new("test", "record_basic", 42).viewport(20, 5);
        let recording = Lab::record(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.capture_frame();
            s.send(CounterMsg::Increment);
            s.capture_frame();
        });

        assert_eq!(recording.frame_records.len(), 2);
        assert_eq!(recording.seed, 42);
        assert_eq!(recording.scenario_name, "record_basic");
        assert!(!recording.run_id.is_empty());
    }

    #[test]
    fn record_captures_event_log() {
        let config = LabConfig::new("test", "record_events", 42);
        let recording = Lab::record(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.tick();
            s.inject_event(key_event('+'));
        });

        assert_eq!(recording.event_log.len(), 3);
        assert_eq!(recording.tick_count, 1);
    }

    #[test]
    fn replay_matches_recording() {
        let config = LabConfig::new("test", "replay_match", 42).viewport(20, 5);
        let recording = Lab::record(config, Counter { value: 0 }, |s| {
            s.init();
            for _ in 0..5 {
                s.send(CounterMsg::Increment);
                s.tick();
                s.capture_frame();
            }
        });

        let result = Lab::replay(&recording, Counter { value: 0 }, |s| {
            s.init();
            for _ in 0..5 {
                s.send(CounterMsg::Increment);
                s.tick();
                s.capture_frame();
            }
        });

        assert!(result.matched, "replay should match recording");
        assert_eq!(result.frames_compared, 5);
        assert!(result.first_divergence.is_none());
        assert!(result.divergence_detail.is_none());
    }

    #[test]
    fn replay_detects_frame_count_mismatch() {
        let config = LabConfig::new("test", "replay_count_diff", 42).viewport(20, 5);
        let recording = Lab::record(config, Counter { value: 0 }, |s| {
            s.init();
            s.capture_frame();
            s.capture_frame();
            s.capture_frame();
        });

        // Replay with fewer frames
        let result = Lab::replay(&recording, Counter { value: 0 }, |s| {
            s.init();
            s.capture_frame();
        });

        assert!(!result.matched);
        assert!(result.first_divergence.is_some());
        let detail = result.divergence_detail.unwrap();
        assert!(
            detail.contains("frame count mismatch"),
            "expected frame count mismatch message, got: {detail}"
        );
    }

    #[test]
    fn replay_detects_checksum_mismatch() {
        let config = LabConfig::new("test", "replay_checksum_diff", 42).viewport(20, 5);
        let recording = Lab::record(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment); // value=1
            s.capture_frame();
        });

        // Replay with different state → different checksum
        let result = Lab::replay(&recording, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.send(CounterMsg::Increment); // value=2 (diverges)
            s.capture_frame();
        });

        assert!(!result.matched);
        assert_eq!(result.first_divergence, Some(0));
        let detail = result.divergence_detail.unwrap();
        assert!(
            detail.contains("checksum mismatch"),
            "expected checksum mismatch message, got: {detail}"
        );
    }

    #[test]
    fn assert_replay_deterministic_passes() {
        let config = LabConfig::new("test", "replay_det", 42).viewport(20, 5);
        let recording = Lab::assert_replay_deterministic(
            config,
            || Counter { value: 0 },
            |s| {
                s.init();
                for _ in 0..5 {
                    s.send(CounterMsg::Increment);
                    s.tick();
                    s.capture_frame();
                }
            },
        );

        assert_eq!(recording.frame_records.len(), 5);
    }

    #[test]
    #[should_panic(expected = "replay diverged")]
    fn assert_replay_deterministic_panics_on_divergence() {
        // This test uses a model that behaves differently on each creation,
        // simulated by using different initial values.
        let call_count = std::sync::atomic::AtomicU32::new(0);
        let config = LabConfig::new("test", "replay_diverge", 42).viewport(20, 5);

        Lab::assert_replay_deterministic(
            config,
            || {
                let n = call_count.fetch_add(1, Ordering::Relaxed);
                // First model starts at 0, second at 100 → different frames
                Counter {
                    value: (n * 100) as i32,
                }
            },
            |s| {
                s.init();
                s.capture_frame();
            },
        );
    }

    #[test]
    fn recording_counters_increment() {
        let before_record = lab_recordings_total();
        let before_replay = lab_replays_total();

        let config = LabConfig::new("test", "counters", 42).viewport(10, 3);
        let recording = Lab::record(config, Counter { value: 0 }, |s| {
            s.init();
            s.capture_frame();
        });

        assert!(
            lab_recordings_total() > before_record,
            "lab_recordings_total should increment"
        );

        Lab::replay(&recording, Counter { value: 0 }, |s| {
            s.init();
            s.capture_frame();
        });

        assert!(
            lab_replays_total() > before_replay,
            "lab_replays_total should increment"
        );
    }

    #[test]
    fn assert_outputs_match_passes_for_identical() {
        let config = LabConfig::new("test", "output_match", 42).viewport(20, 5);
        let run1 = Lab::run_scenario(config.clone(), Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.capture_frame();
        });
        let run2 = Lab::run_scenario(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.capture_frame();
        });

        assert_outputs_match(&run1.output, &run2.output);
    }

    #[test]
    #[should_panic(expected = "checksum mismatch")]
    fn assert_outputs_match_panics_on_difference() {
        let config1 = LabConfig::new("test", "output_diff", 42).viewport(20, 5);
        let config2 = LabConfig::new("test", "output_diff", 42).viewport(20, 5);
        let run1 = Lab::run_scenario(config1, Counter { value: 0 }, |s| {
            s.init();
            s.capture_frame(); // value=0
        });
        let run2 = Lab::run_scenario(config2, Counter { value: 5 }, |s| {
            s.init();
            s.capture_frame(); // value=5
        });

        assert_outputs_match(&run1.output, &run2.output);
    }

    #[test]
    fn replay_100_seeds_all_match() {
        for seed in 0..100 {
            let config = LabConfig::new("test", "replay_100", seed).viewport(20, 5);
            let recording = Lab::record(config, Counter { value: 0 }, |s| {
                s.init();
                s.send(CounterMsg::Increment);
                s.tick();
                s.capture_frame();
            });
            let result = Lab::replay(&recording, Counter { value: 0 }, |s| {
                s.init();
                s.send(CounterMsg::Increment);
                s.tick();
                s.capture_frame();
            });
            assert!(
                result.matched,
                "seed {seed}: replay diverged from recording"
            );
        }
    }

    #[test]
    fn recording_config_is_preserved() {
        let config = LabConfig::new("prefix123", "scenario456", 789)
            .viewport(100, 50)
            .time_step_ms(33);
        let recording = Lab::record(config, Counter { value: 0 }, |s| {
            s.init();
            s.capture_frame();
        });

        assert_eq!(recording.config.prefix, "prefix123");
        assert_eq!(recording.config.scenario_name, "scenario456");
        assert_eq!(recording.config.seed, 789);
        assert_eq!(recording.config.viewport_width, 100);
        assert_eq!(recording.config.viewport_height, 50);
        assert_eq!(recording.config.time_step_ms, 33);
    }

    #[test]
    fn replay_result_carries_replay_frames() {
        let config = LabConfig::new("test", "replay_frames", 42).viewport(20, 5);
        let recording = Lab::record(config, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.capture_frame();
            s.send(CounterMsg::Increment);
            s.capture_frame();
        });
        let result = Lab::replay(&recording, Counter { value: 0 }, |s| {
            s.init();
            s.send(CounterMsg::Increment);
            s.capture_frame();
            s.send(CounterMsg::Increment);
            s.capture_frame();
        });

        assert!(result.matched);
        assert_eq!(result.replay_frame_records.len(), 2);
        // Replay frames should match recording frames
        for (rec, rep) in recording
            .frame_records
            .iter()
            .zip(result.replay_frame_records.iter())
        {
            assert_eq!(rec.checksum, rep.checksum);
        }
    }
}
