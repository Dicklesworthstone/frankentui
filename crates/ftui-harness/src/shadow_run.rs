#![forbid(unsafe_code)]

//! Shadow-run comparison harness for validating execution-path equivalence.
//!
//! Runs the same [`Model`] and event sequence through two independent
//! [`LabSession`] instances (baseline vs candidate) and compares frame
//! checksums, event counts, and timing. This is the primary mechanism for
//! proving that a runtime migration (e.g., threading → Asupersync executor)
//! preserves rendering determinism.
//!
//! # Design
//!
//! A [`ShadowRun`] takes two [`LabConfig`]s (baseline and candidate), a
//! model factory, and a scenario closure. It executes the scenario twice—
//! once per lane—under deterministic seeds, then compares the frame records.
//! All comparison evidence is emitted to JSONL via [`TestJsonlLogger`].
//!
//! # Example
//!
//! ```ignore
//! use ftui_harness::shadow_run::{ShadowRun, ShadowRunConfig, ShadowVerdict};
//!
//! let config = ShadowRunConfig::new("migration_test", "tick_counter", 42)
//!     .viewport(80, 24);
//!
//! let result = ShadowRun::compare(config, || MyModel::new(), |session| {
//!     session.init();
//!     session.tick();
//!     session.capture_frame();
//! });
//!
//! assert_eq!(result.verdict, ShadowVerdict::Match);
//! ```

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::determinism::{JsonValue, TestJsonlLogger};
use crate::lab_integration::{FrameRecord, Lab, LabConfig, LabOutput, LabSession};
use ftui_runtime::evidence_sink::EvidenceSinkConfig;
use ftui_runtime::program::{
    EffectQueueConfig, HeadlessEventSource, Model, Program, ProgramConfig, RolloutPolicy,
    RuntimeLane, TaskExecutorBackend, TerminalPresenter,
};
use tracing::info_span;

/// Global counter for shadow runs executed.
static SHADOW_RUNS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Read the total number of shadow runs executed in-process.
#[must_use]
pub fn shadow_runs_total() -> u64 {
    SHADOW_RUNS_TOTAL.load(Ordering::Relaxed)
}

// ============================================================================
// Effect-once record/replay log for dual-lane shadow comparison
// ============================================================================

#[derive(Debug)]
struct ShadowEffectLogInner<R> {
    recorded: Vec<R>,
    cursor: usize,
    replay_mode: bool,
    executions: usize,
}

/// Shared effect-once record/replay log for [`ShadowRun::compare_lanes`].
///
/// During the baseline lane execution, [`ShadowEffectLog::run_once`] executes
/// the side-effect closure, records its result, and increments the execution
/// counter. Before the candidate lane runs, [`ShadowEffectLog::rewind_for_replay`]
/// switches the log to replay mode so the candidate lane receives the recorded
/// outputs in order without re-executing external side effects.
#[derive(Debug, Clone)]
pub struct ShadowEffectLog<R> {
    inner: Arc<Mutex<ShadowEffectLogInner<R>>>,
}

impl<R: Clone + Send + 'static> Default for ShadowEffectLog<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Clone + Send + 'static> ShadowEffectLog<R> {
    /// Create a new empty effect log in record mode.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ShadowEffectLogInner {
                recorded: Vec::new(),
                cursor: 0,
                replay_mode: false,
                executions: 0,
            })),
        }
    }

    /// Execute `effect` during recording, or return the recorded result at the
    /// current cursor during candidate replay.
    pub fn run_once<F>(&self, effect: F) -> R
    where
        F: FnOnce() -> R,
    {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.replay_mode && guard.cursor < guard.recorded.len() {
            let idx = guard.cursor;
            guard.cursor += 1;
            return guard.recorded[idx].clone();
        }
        drop(guard);
        let value = effect();
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.executions += 1;
        guard.recorded.push(value.clone());
        guard.cursor = guard.recorded.len();
        value
    }

    /// Switch the log into replay mode and reset the cursor to the first effect.
    pub fn rewind_for_replay(&self) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.replay_mode = true;
        guard.cursor = 0;
    }

    /// Number of times an underlying side-effect closure was actually executed.
    #[must_use]
    pub fn executions(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .executions
    }

    /// Snapshot of all recorded effect outputs in execution order.
    #[must_use]
    pub fn recorded(&self) -> Vec<R> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .recorded
            .clone()
    }
}

/// Execution lane specification for [`ShadowRun::compare_lanes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowLaneSpec {
    /// Requested runtime lane.
    pub lane: RuntimeLane,
    /// Optional explicit task executor backend override.
    pub task_backend_override: Option<TaskExecutorBackend>,
}

impl ShadowLaneSpec {
    /// Construct a lane specification using the lane's default task backend.
    #[must_use]
    pub const fn new(lane: RuntimeLane) -> Self {
        Self {
            lane,
            task_backend_override: None,
        }
    }

    /// Override the task executor backend for this lane.
    #[must_use]
    pub const fn with_task_backend(mut self, backend: TaskExecutorBackend) -> Self {
        self.task_backend_override = Some(backend);
        self
    }

    /// Apply this lane specification to a [`ProgramConfig`].
    #[must_use]
    pub fn apply_to(self, config: ProgramConfig) -> ProgramConfig {
        let config = config.with_lane(self.lane);
        match self.task_backend_override {
            Some(backend) => {
                config.with_effect_queue(EffectQueueConfig::default().with_backend(backend))
            }
            None => config,
        }
    }
}

/// Headless [`Program`] type alias used by [`ShadowRun::compare_lanes`].
pub type ShadowHeadlessProgram<M> = Program<M, HeadlessEventSource, TerminalPresenter<Vec<u8>>>;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for a shadow-run comparison.
#[derive(Debug, Clone)]
pub struct ShadowRunConfig {
    /// Shared prefix for JSONL logger and run IDs.
    pub prefix: String,
    /// Scenario name used in tracing spans and JSONL.
    pub scenario_name: String,
    /// Deterministic seed (shared across both lanes).
    pub seed: u64,
    /// Viewport width for frame captures.
    pub viewport_width: u16,
    /// Viewport height for frame captures.
    pub viewport_height: u16,
    /// Time step in milliseconds for deterministic clocks.
    pub time_step_ms: u64,
    /// Label for the baseline lane (default: "baseline").
    pub baseline_label: String,
    /// Label for the candidate lane (default: "candidate").
    pub candidate_label: String,
}

impl ShadowRunConfig {
    /// Create a new shadow-run configuration with defaults.
    ///
    /// Defaults: 80×24 viewport, 16ms time step.
    pub fn new(prefix: &str, scenario_name: &str, seed: u64) -> Self {
        Self {
            prefix: prefix.to_string(),
            scenario_name: scenario_name.to_string(),
            seed,
            viewport_width: 80,
            viewport_height: 24,
            time_step_ms: 16,
            baseline_label: "baseline".to_string(),
            candidate_label: "candidate".to_string(),
        }
    }

    /// Set the viewport dimensions.
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

    /// Set custom lane labels.
    #[must_use]
    pub fn lane_labels(mut self, baseline: &str, candidate: &str) -> Self {
        self.baseline_label = baseline.to_string();
        self.candidate_label = candidate.to_string();
        self
    }

    /// Build a [`LabConfig`] for a given lane.
    fn lab_config(&self, lane: &str) -> LabConfig {
        LabConfig::new(
            &format!("{}_{}", self.prefix, lane),
            &self.scenario_name,
            self.seed,
        )
        .viewport(self.viewport_width, self.viewport_height)
        .time_step_ms(self.time_step_ms)
    }
}

// ============================================================================
// Verdict and result
// ============================================================================

/// Outcome of a shadow-run comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowVerdict {
    /// All frame checksums matched between baseline and candidate.
    Match,
    /// Frame checksums diverged at one or more positions.
    Diverged,
}

/// Per-frame comparison detail.
#[derive(Debug, Clone)]
pub struct FrameComparison {
    /// Frame index (0-based).
    pub index: usize,
    /// Baseline frame checksum.
    pub baseline_checksum: u64,
    /// Candidate frame checksum.
    pub candidate_checksum: u64,
    /// Whether this frame matched.
    pub matched: bool,
}

/// Full result of a shadow-run comparison.
#[derive(Debug, Clone)]
pub struct ShadowRunResult {
    /// Overall verdict.
    pub verdict: ShadowVerdict,
    /// Scenario name.
    pub scenario_name: String,
    /// Seed used for both lanes.
    pub seed: u64,
    /// Per-frame comparison details.
    pub frame_comparisons: Vec<FrameComparison>,
    /// Index of the first divergent frame (if any).
    pub first_divergence: Option<usize>,
    /// Number of frames compared.
    pub frames_compared: usize,
    /// Baseline lane output.
    pub baseline: LabOutput,
    /// Candidate lane output.
    pub candidate: LabOutput,
    /// Baseline lane label.
    pub baseline_label: String,
    /// Candidate lane label.
    pub candidate_label: String,
    /// Total shadow runs executed in-process (including this one).
    pub run_total: u64,
}

impl ShadowRunResult {
    /// Number of frames that diverged.
    #[must_use]
    pub fn diverged_count(&self) -> usize {
        self.frame_comparisons.iter().filter(|c| !c.matched).count()
    }

    /// Fraction of frames that matched (0.0–1.0).
    #[must_use]
    pub fn match_ratio(&self) -> f64 {
        if self.frames_compared == 0 {
            return 1.0;
        }
        let matched = self.frame_comparisons.iter().filter(|c| c.matched).count();
        matched as f64 / self.frames_compared as f64
    }
}

#[derive(Debug, Clone)]
struct RecordedShadowFile {
    frames: Vec<FrameRecord>,
    lane_label: String,
}

fn parse_recorded_shadow_file(path: &Path, default_label: &str) -> RecordedShadowFile {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut frames = Vec::new();
    let mut lane_label: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if value.get("event").and_then(|v| v.as_str()) != Some("shadow_frame") {
            continue;
        }
        if lane_label.is_none()
            && let Some(lane) = value.get("lane").and_then(|v| v.as_str())
        {
            lane_label = Some(lane.to_string());
        }
        let index = value
            .get("frame")
            .and_then(|v| v.as_u64())
            .unwrap_or(frames.len() as u64) as usize;
        let checksum = value
            .get("checksum")
            .and_then(|v| v.as_str())
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            .unwrap_or(0);
        let timestamp_ms = value.get("ts_ns").and_then(|v| v.as_u64()).unwrap_or(0) / 1_000_000;

        frames.push(FrameRecord {
            index,
            timestamp_ms,
            checksum,
        });
    }

    RecordedShadowFile {
        frames,
        lane_label: lane_label.unwrap_or_else(|| default_label.to_string()),
    }
}

// ============================================================================
// Shadow-run executor
// ============================================================================

/// Shadow-run comparison harness.
///
/// Runs the same model and event sequence through two independent LabSession
/// instances and compares their frame outputs.
pub struct ShadowRun;

impl ShadowRun {
    /// Compare two recorded `shadow_frame` JSONL evidence files emitted by
    /// `Program` under [`RolloutPolicy::Shadow`].
    ///
    /// Non-`shadow_frame` events in either file (such as `diff_decision` or
    /// `width_cache_stats`) are ignored. If all shared frames match and both
    /// files recorded the same number of `shadow_frame` events (with at least
    /// one frame when both files are empty or matching), the verdict is
    /// [`ShadowVerdict::Match`]; otherwise [`ShadowVerdict::Diverged`] with
    /// `first_divergence` pointing at the first mismatched frame index (or the
    /// length of the shorter run on frame-count mismatch).
    #[must_use]
    pub fn compare_recorded(baseline: &Path, candidate: &Path) -> ShadowRunResult {
        let base_parsed = parse_recorded_shadow_file(baseline, "baseline");
        let cand_parsed = parse_recorded_shadow_file(candidate, "candidate");

        let baseline_frames = &base_parsed.frames;
        let candidate_frames = &cand_parsed.frames;
        let frames_compared = baseline_frames.len().min(candidate_frames.len());
        let mut frame_comparisons = Vec::with_capacity(frames_compared);
        let mut first_divergence: Option<usize> = None;

        for i in 0..frames_compared {
            let matched = baseline_frames[i].checksum == candidate_frames[i].checksum;
            frame_comparisons.push(FrameComparison {
                index: i,
                baseline_checksum: baseline_frames[i].checksum,
                candidate_checksum: candidate_frames[i].checksum,
                matched,
            });
            if !matched && first_divergence.is_none() {
                first_divergence = Some(i);
            }
        }

        if baseline_frames.len() != candidate_frames.len() && first_divergence.is_none() {
            first_divergence = Some(frames_compared);
        }

        let verdict = if first_divergence.is_some() {
            ShadowVerdict::Diverged
        } else {
            ShadowVerdict::Match
        };

        let run_total = SHADOW_RUNS_TOTAL
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);

        ShadowRunResult {
            verdict,
            scenario_name: "recorded_shadow_compare".to_string(),
            seed: 0,
            frame_comparisons,
            first_divergence,
            frames_compared,
            baseline: LabOutput {
                frame_count: baseline_frames.len(),
                frame_records: base_parsed.frames,
                event_count: 0,
                event_log: Vec::new(),
                tick_count: 0,
                anomaly_count: 0,
            },
            candidate: LabOutput {
                frame_count: candidate_frames.len(),
                frame_records: cand_parsed.frames,
                event_count: 0,
                event_log: Vec::new(),
                tick_count: 0,
                anomaly_count: 0,
            },
            baseline_label: base_parsed.lane_label,
            candidate_label: cand_parsed.lane_label,
            run_total,
        }
    }

    /// Execute a real headless [`Program`] scenario across two distinct runtime
    /// lanes / task-executor backends under [`RolloutPolicy::Shadow`], rewinding
    /// the shared [`ShadowEffectLog`] between runs so external side effects
    /// execute only once, and compare the recorded `shadow_frame` evidence.
    pub fn compare_lanes<M, R, MF, SF>(
        config: ShadowRunConfig,
        baseline_lane: ShadowLaneSpec,
        candidate_lane: ShadowLaneSpec,
        effect_log: &ShadowEffectLog<R>,
        model_factory: MF,
        scenario_fn: SF,
    ) -> ShadowRunResult
    where
        M: Model,
        R: Clone + Send + 'static,
        MF: Fn(ShadowLaneSpec) -> M,
        SF: Fn(&mut ShadowHeadlessProgram<M>, ShadowLaneSpec),
    {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let baseline_path = std::env::temp_dir().join(format!(
            "ftui_shadow_lane_baseline_{}_{}_{}.jsonl",
            config.prefix, pid, nonce
        ));
        let candidate_path = std::env::temp_dir().join(format!(
            "ftui_shadow_lane_candidate_{}_{}_{}.jsonl",
            config.prefix, pid, nonce
        ));

        let build_program =
            |lane_spec: ShadowLaneSpec, evidence_path: &Path| -> ShadowHeadlessProgram<M> {
                let base_cfg = ProgramConfig::default()
                    .with_forced_size(config.viewport_width, config.viewport_height)
                    .with_rollout_policy(RolloutPolicy::Shadow)
                    .with_evidence_sink(EvidenceSinkConfig::enabled_file(evidence_path))
                    .with_signal_interception(false);
                let prog_cfg = lane_spec.apply_to(base_cfg);
                Program::new_headless(model_factory(lane_spec), prog_cfg)
                    .expect("initialize headless shadow program")
            };

        {
            let mut baseline_program = build_program(baseline_lane, &baseline_path);
            scenario_fn(&mut baseline_program, baseline_lane);
            baseline_program
                .drain_headless_tasks(Duration::from_secs(5))
                .expect("drain baseline tasks");
        }

        effect_log.rewind_for_replay();

        {
            let mut candidate_program = build_program(candidate_lane, &candidate_path);
            scenario_fn(&mut candidate_program, candidate_lane);
            candidate_program
                .drain_headless_tasks(Duration::from_secs(5))
                .expect("drain candidate tasks");
        }

        let mut result = Self::compare_recorded(&baseline_path, &candidate_path);
        result.scenario_name = config.scenario_name;
        result.seed = config.seed;
        result.baseline_label = config.baseline_label;
        result.candidate_label = config.candidate_label;
        result
    }

    /// Run a shadow comparison between baseline and candidate lanes.
    ///
    /// Both lanes execute the same `scenario_fn` with the same seed and
    /// configuration. Frame checksums are compared after both runs complete.
    ///
    /// The `model_factory` is called twice (once per lane) to produce
    /// independent model instances.
    ///
    /// # Evidence
    ///
    /// Emits structured JSONL to stderr with events:
    /// - `shadow.start`: comparison parameters
    /// - `shadow.lane.done`: per-lane summary
    /// - `shadow.frame.diverged`: each divergent frame
    /// - `shadow.verdict`: final pass/fail with statistics
    pub fn compare<M, MF, SF>(
        config: ShadowRunConfig,
        model_factory: MF,
        scenario_fn: SF,
    ) -> ShadowRunResult
    where
        M: Model,
        MF: Fn() -> M,
        SF: Fn(&mut LabSession<M>),
    {
        let _span = info_span!(
            "shadow_run",
            scenario_name = config.scenario_name.as_str(),
            seed = config.seed,
            baseline = config.baseline_label.as_str(),
            candidate = config.candidate_label.as_str(),
        )
        .entered();

        let mut logger = TestJsonlLogger::new_with(
            &format!("{}_shadow", config.prefix),
            config.seed,
            true,
            config.time_step_ms,
        );
        logger.add_context_str("scenario_name", &config.scenario_name);
        logger.add_context_str("baseline_label", &config.baseline_label);
        logger.add_context_str("candidate_label", &config.candidate_label);

        // Log start
        logger.log(
            "shadow.start",
            &[
                ("scenario_name", JsonValue::str(&config.scenario_name)),
                ("seed", JsonValue::u64(config.seed)),
                (
                    "viewport",
                    JsonValue::raw(format!(
                        "[{},{}]",
                        config.viewport_width, config.viewport_height
                    )),
                ),
            ],
        );

        // Run baseline lane
        let baseline_config = config.lab_config(&config.baseline_label);
        let baseline_run = Lab::run_scenario(baseline_config, model_factory(), |s| scenario_fn(s));

        logger.log(
            "shadow.lane.done",
            &[
                ("lane", JsonValue::str(&config.baseline_label)),
                (
                    "frame_count",
                    JsonValue::u64(baseline_run.output.frame_count as u64),
                ),
                (
                    "event_count",
                    JsonValue::u64(baseline_run.output.event_count as u64),
                ),
                ("tick_count", JsonValue::u64(baseline_run.output.tick_count)),
                (
                    "anomaly_count",
                    JsonValue::u64(baseline_run.output.anomaly_count),
                ),
            ],
        );

        // Run candidate lane
        let candidate_config = config.lab_config(&config.candidate_label);
        let candidate_run =
            Lab::run_scenario(candidate_config, model_factory(), |s| scenario_fn(s));

        logger.log(
            "shadow.lane.done",
            &[
                ("lane", JsonValue::str(&config.candidate_label)),
                (
                    "frame_count",
                    JsonValue::u64(candidate_run.output.frame_count as u64),
                ),
                (
                    "event_count",
                    JsonValue::u64(candidate_run.output.event_count as u64),
                ),
                (
                    "tick_count",
                    JsonValue::u64(candidate_run.output.tick_count),
                ),
                (
                    "anomaly_count",
                    JsonValue::u64(candidate_run.output.anomaly_count),
                ),
            ],
        );

        // Compare frame checksums
        let baseline_frames = &baseline_run.output.frame_records;
        let candidate_frames = &candidate_run.output.frame_records;
        let frames_compared = baseline_frames.len().min(candidate_frames.len());
        let mut frame_comparisons = Vec::with_capacity(frames_compared);
        let mut first_divergence: Option<usize> = None;

        for i in 0..frames_compared {
            let matched = baseline_frames[i].checksum == candidate_frames[i].checksum;
            frame_comparisons.push(FrameComparison {
                index: i,
                baseline_checksum: baseline_frames[i].checksum,
                candidate_checksum: candidate_frames[i].checksum,
                matched,
            });
            if !matched && first_divergence.is_none() {
                first_divergence = Some(i);
                logger.log(
                    "shadow.frame.diverged",
                    &[
                        ("frame_idx", JsonValue::u64(i as u64)),
                        (
                            "baseline_checksum",
                            JsonValue::str(format!("{:016x}", baseline_frames[i].checksum)),
                        ),
                        (
                            "candidate_checksum",
                            JsonValue::str(format!("{:016x}", candidate_frames[i].checksum)),
                        ),
                    ],
                );
            }
        }

        // Handle frame count mismatch (also counts as divergence)
        if baseline_frames.len() != candidate_frames.len() && first_divergence.is_none() {
            first_divergence = Some(frames_compared);
        }

        let verdict = if first_divergence.is_some() {
            ShadowVerdict::Diverged
        } else {
            ShadowVerdict::Match
        };

        let diverged_count = frame_comparisons.iter().filter(|c| !c.matched).count();

        // Log verdict
        logger.log(
            "shadow.verdict",
            &[
                (
                    "verdict",
                    JsonValue::str(match verdict {
                        ShadowVerdict::Match => "match",
                        ShadowVerdict::Diverged => "diverged",
                    }),
                ),
                ("frames_compared", JsonValue::u64(frames_compared as u64)),
                ("diverged_count", JsonValue::u64(diverged_count as u64)),
                (
                    "baseline_frames",
                    JsonValue::u64(baseline_frames.len() as u64),
                ),
                (
                    "candidate_frames",
                    JsonValue::u64(candidate_frames.len() as u64),
                ),
            ],
        );

        let run_total = SHADOW_RUNS_TOTAL
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);

        ShadowRunResult {
            verdict,
            scenario_name: config.scenario_name,
            seed: config.seed,
            frame_comparisons,
            first_divergence,
            frames_compared,
            baseline: baseline_run.output,
            candidate: candidate_run.output,
            baseline_label: config.baseline_label,
            candidate_label: config.candidate_label,
            run_total,
        }
    }

    /// Assert that baseline and candidate produce identical frames.
    ///
    /// Convenience wrapper around [`compare`](Self::compare) that panics
    /// on divergence with a diagnostic message.
    ///
    /// # Panics
    ///
    /// Panics if any frame checksum diverges between lanes.
    pub fn assert_match<M, MF, SF>(
        config: ShadowRunConfig,
        model_factory: MF,
        scenario_fn: SF,
    ) -> ShadowRunResult
    where
        M: Model,
        MF: Fn() -> M,
        SF: Fn(&mut LabSession<M>),
    {
        let result = Self::compare(config, model_factory, scenario_fn);
        if result.verdict == ShadowVerdict::Diverged {
            let diverged = result.diverged_count();
            let first = result
                .first_divergence
                .map(|i| format!("frame {i}"))
                .unwrap_or_else(|| "frame count mismatch".to_string());
            panic!(
                "shadow-run divergence: {} of {} frames diverged, first at {}",
                diverged, result.frames_compared, first
            );
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::Event;
    use ftui_render::frame::Frame;
    use ftui_runtime::program::{Cmd, Model};
    use std::sync::atomic::AtomicUsize;

    // Minimal counter model for testing.
    struct Counter {
        value: u64,
    }

    #[derive(Debug, Clone)]
    enum CounterMsg {
        Increment,
        Quit,
    }

    impl From<Event> for CounterMsg {
        fn from(e: Event) -> Self {
            match e {
                Event::Tick => CounterMsg::Increment,
                _ => CounterMsg::Quit,
            }
        }
    }

    impl Model for Counter {
        type Message = CounterMsg;

        fn update(&mut self, msg: CounterMsg) -> Cmd<CounterMsg> {
            match msg {
                CounterMsg::Increment => {
                    self.value += 1;
                    Cmd::none()
                }
                CounterMsg::Quit => Cmd::quit(),
            }
        }

        fn view(&self, frame: &mut Frame) {
            use ftui_core::geometry::Rect;
            use ftui_widgets::paragraph::Paragraph;
            let text = format!("Count: {}", self.value);
            let area = Rect::new(0, 0, frame.width(), 1);
            Paragraph::new(text).render(area, frame);
        }
    }

    // Helper trait for rendering in view
    use ftui_widgets::Widget;

    fn write_temp_shadow_jsonl(tag: &str, lines: &[String]) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ftui_shadow_test_{tag}_{}_{nonce}.jsonl",
            std::process::id()
        ));
        let mut content = lines.join("\n");
        content.push('\n');
        std::fs::write(&path, content).expect("write temp shadow jsonl");
        path
    }

    fn shadow_frame_line(frame: usize, checksum: u64, lane: &str) -> String {
        format!(
            r#"{{"schema_version":"ftui-evidence-v2","event":"shadow_frame","frame":{frame},"checksum":"{checksum:016x}","lane":"{lane}","requested_lane":"{lane}","rollout_policy":"shadow","task_backend":"spawned","cols":80,"rows":24,"ts_ns":{}}}"#,
            (frame as u64 + 1) * 16_000_000
        )
    }

    #[test]
    fn compare_recorded_matches_identical_runs() {
        let lines: Vec<String> = (0..10)
            .map(|i| shadow_frame_line(i, 0x1000_0000_0000_0000 + i as u64, "structured"))
            .collect();
        let a = write_temp_shadow_jsonl("identical_a", &lines);
        let b = write_temp_shadow_jsonl("identical_b", &lines);

        let result = ShadowRun::compare_recorded(&a, &b);
        if result.verdict != ShadowVerdict::Match {
            eprintln!(
                "baseline:\n{}\ncandidate:\n{}",
                std::fs::read_to_string(&a).unwrap_or_default(),
                std::fs::read_to_string(&b).unwrap_or_default()
            );
        }
        assert_eq!(result.verdict, ShadowVerdict::Match);
        assert_eq!(result.frames_compared, 10);
        assert_eq!(result.first_divergence, None);
        assert_eq!(result.diverged_count(), 0);
    }

    #[test]
    fn compare_recorded_reports_first_divergent_frame() {
        let base_lines: Vec<String> = (0..10)
            .map(|i| shadow_frame_line(i, 0x2000_0000_0000_0000 + i as u64, "structured"))
            .collect();
        let mut cand_lines = base_lines.clone();
        cand_lines[7] = shadow_frame_line(7, 0xdead_beef_cafe_babe, "asupersync");

        let a = write_temp_shadow_jsonl("div_a", &base_lines);
        let b = write_temp_shadow_jsonl("div_b", &cand_lines);

        let result = ShadowRun::compare_recorded(&a, &b);
        assert_eq!(result.verdict, ShadowVerdict::Diverged);
        assert_eq!(result.frames_compared, 10);
        assert_eq!(result.first_divergence, Some(7));
        assert_eq!(result.diverged_count(), 1);
    }

    #[test]
    fn compare_recorded_ignores_non_shadow_events() {
        let mut base_lines = Vec::new();
        let mut cand_lines = Vec::new();
        for i in 0..6 {
            base_lines.push(format!(
                r#"{{"schema_version":"ftui-evidence-v2","event":"diff_decision","frame_idx":{i},"strategy":"dirty_rows"}}"#
            ));
            base_lines.push(shadow_frame_line(
                i,
                0x3333_0000_0000_0000 + i as u64,
                "structured",
            ));
            cand_lines.push(shadow_frame_line(
                i,
                0x3333_0000_0000_0000 + i as u64,
                "asupersync",
            ));
            cand_lines.push(
                r#"{"schema_version":"ftui-evidence-v2","event":"width_cache_stats","hits":10,"misses":1}"#
                    .to_string(),
            );
        }

        let a = write_temp_shadow_jsonl("ignore_non_shadow_a", &base_lines);
        let b = write_temp_shadow_jsonl("ignore_non_shadow_b", &cand_lines);

        let result = ShadowRun::compare_recorded(&a, &b);
        assert_eq!(result.verdict, ShadowVerdict::Match);
        assert_eq!(result.frames_compared, 6);
        assert_eq!(result.first_divergence, None);
    }

    #[test]
    fn compare_recorded_length_mismatch_is_reported() {
        let base_lines: Vec<String> = (0..10)
            .map(|i| shadow_frame_line(i, 0x4444_0000_0000_0000 + i as u64, "structured"))
            .collect();
        let cand_lines: Vec<String> = base_lines[..8].to_vec();

        let a = write_temp_shadow_jsonl("len_a", &base_lines);
        let b = write_temp_shadow_jsonl("len_b", &cand_lines);

        let result = ShadowRun::compare_recorded(&a, &b);
        assert_eq!(result.verdict, ShadowVerdict::Diverged);
        assert_eq!(result.frames_compared, 8);
        assert_eq!(result.first_divergence, Some(8));
    }

    struct TaskDrivenModel {
        effect_log: ShadowEffectLog<u64>,
        raw_effect_calls: Arc<AtomicUsize>,
        divergent_bias: u64,
        accumulated: u64,
        step: u64,
    }

    #[derive(Debug, Clone)]
    enum TaskDrivenMsg {
        TriggerTask,
        TaskDone(u64),
        Noop,
    }

    impl From<Event> for TaskDrivenMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Tick => Self::TriggerTask,
                _ => Self::Noop,
            }
        }
    }

    impl Model for TaskDrivenModel {
        type Message = TaskDrivenMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                TaskDrivenMsg::TriggerTask => {
                    self.step += 1;
                    let step = self.step;
                    let bias = self.divergent_bias;
                    let effect_log = self.effect_log.clone();
                    let raw_calls = Arc::clone(&self.raw_effect_calls);
                    Cmd::task(move || {
                        let recorded = effect_log.run_once(move || {
                            raw_calls.fetch_add(1, Ordering::SeqCst);
                            step * 10
                        });
                        TaskDrivenMsg::TaskDone(recorded + bias)
                    })
                }
                TaskDrivenMsg::TaskDone(val) => {
                    self.accumulated += val;
                    Cmd::none()
                }
                TaskDrivenMsg::Noop => Cmd::none(),
            }
        }

        fn view(&self, frame: &mut Frame) {
            use ftui_core::geometry::Rect;
            use ftui_widgets::paragraph::Paragraph;
            let text = format!("Accumulated: {} (step {})", self.accumulated, self.step);
            let area = Rect::new(0, 0, frame.width(), 1);
            Paragraph::new(text).render(area, frame);
        }
    }

    #[test]
    fn compare_lanes_spawned_vs_effect_queue_matches_and_runs_effects_once() {
        let effect_log = ShadowEffectLog::new();
        let raw_effect_calls = Arc::new(AtomicUsize::new(0));
        let baseline_spec = ShadowLaneSpec::new(RuntimeLane::Structured)
            .with_task_backend(TaskExecutorBackend::Spawned);
        let candidate_spec = ShadowLaneSpec::new(RuntimeLane::Structured)
            .with_task_backend(TaskExecutorBackend::EffectQueue);
        let config = ShadowRunConfig::new("lane_cmp", "spawned_vs_queued", 42)
            .lane_labels("spawned", "queued");

        let result = ShadowRun::compare_lanes(
            config,
            baseline_spec,
            candidate_spec,
            &effect_log,
            |_spec| TaskDrivenModel {
                effect_log: effect_log.clone(),
                raw_effect_calls: Arc::clone(&raw_effect_calls),
                divergent_bias: 0,
                accumulated: 0,
                step: 0,
            },
            |program, spec| {
                let expected_backend = match spec.task_backend_override {
                    Some(TaskExecutorBackend::Spawned) => "spawned",
                    Some(TaskExecutorBackend::EffectQueue) => "queued",
                    _ => "asupersync",
                };
                assert_eq!(program.task_backend_name(), expected_backend);
                program
                    .init_headless(Duration::from_secs(5))
                    .expect("init headless");
                for _ in 0..4 {
                    program
                        .step_headless_event(Event::Tick, Duration::from_secs(5))
                        .expect("step tick event");
                }
            },
        );

        assert_eq!(result.verdict, ShadowVerdict::Match);
        assert_eq!(result.frames_compared, 5);
        assert_eq!(result.first_divergence, None);
        assert_eq!(
            raw_effect_calls.load(Ordering::SeqCst),
            4,
            "external side effects must execute only during baseline and replay during candidate"
        );
        assert_eq!(effect_log.executions(), 4);
        assert_eq!(effect_log.recorded(), vec![10, 20, 30, 40]);
    }

    #[test]
    fn compare_lanes_detects_divergent_candidate_executor() {
        let effect_log = ShadowEffectLog::new();
        let raw_effect_calls = Arc::new(AtomicUsize::new(0));
        let baseline_spec = ShadowLaneSpec::new(RuntimeLane::Structured)
            .with_task_backend(TaskExecutorBackend::Spawned);
        let candidate_spec = ShadowLaneSpec::new(RuntimeLane::Structured)
            .with_task_backend(TaskExecutorBackend::EffectQueue);
        let config = ShadowRunConfig::new("lane_div", "divergent_candidate", 42)
            .lane_labels("spawned", "queued");

        let result = ShadowRun::compare_lanes(
            config,
            baseline_spec,
            candidate_spec,
            &effect_log,
            |spec| TaskDrivenModel {
                effect_log: effect_log.clone(),
                raw_effect_calls: Arc::clone(&raw_effect_calls),
                divergent_bias: if spec == candidate_spec { 1 } else { 0 },
                accumulated: 0,
                step: 0,
            },
            |program, _spec| {
                program
                    .init_headless(Duration::from_secs(5))
                    .expect("init headless");
                for _ in 0..3 {
                    program
                        .step_headless_event(Event::Tick, Duration::from_secs(5))
                        .expect("step tick event");
                }
            },
        );

        assert_eq!(result.verdict, ShadowVerdict::Diverged);
        // Frame 0 (init) matches; frame 1 (after first task completion) diverges.
        assert_eq!(result.first_divergence, Some(1));
        assert_eq!(raw_effect_calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn shadow_run_identical_models_match() {
        let config = ShadowRunConfig::new("test_shadow", "counter_match", 42);
        let result = ShadowRun::compare(
            config,
            || Counter { value: 0 },
            |session| {
                session.init();
                session.tick();
                session.capture_frame();
                session.tick();
                session.capture_frame();
            },
        );
        assert_eq!(result.verdict, ShadowVerdict::Match);
        assert_eq!(result.frames_compared, 2);
        assert_eq!(result.diverged_count(), 0);
        assert!((result.match_ratio() - 1.0).abs() < f64::EPSILON);
        assert!(result.first_divergence.is_none());
    }

    #[test]
    fn shadow_run_assert_match_succeeds_for_identical() {
        let config = ShadowRunConfig::new("test_assert", "counter_assert", 42);
        let result = ShadowRun::assert_match(
            config,
            || Counter { value: 0 },
            |session| {
                session.init();
                session.tick();
                session.capture_frame();
            },
        );
        assert_eq!(result.verdict, ShadowVerdict::Match);
    }

    #[test]
    fn shadow_run_config_custom_labels() {
        let config = ShadowRunConfig::new("test_labels", "label_test", 7)
            .lane_labels("threading", "asupersync");
        assert_eq!(config.baseline_label, "threading");
        assert_eq!(config.candidate_label, "asupersync");
    }

    #[test]
    fn shadow_run_config_viewport() {
        let config = ShadowRunConfig::new("test_vp", "vp_test", 0)
            .viewport(120, 40)
            .time_step_ms(8);
        assert_eq!(config.viewport_width, 120);
        assert_eq!(config.viewport_height, 40);
        assert_eq!(config.time_step_ms, 8);
    }

    #[test]
    fn shadow_runs_total_increments() {
        let before = shadow_runs_total();
        let config = ShadowRunConfig::new("test_total", "total_test", 1);
        let _ = ShadowRun::compare(
            config,
            || Counter { value: 0 },
            |session| {
                session.init();
                session.capture_frame();
            },
        );
        assert!(shadow_runs_total() > before);
    }

    #[test]
    fn lab_assert_outputs_match_succeeds_for_identical() {
        let config = ShadowRunConfig::new("test_outputs", "outputs_test", 99);
        let result = ShadowRun::compare(
            config,
            || Counter { value: 0 },
            |session| {
                session.init();
                session.tick();
                session.capture_frame();
            },
        );
        // Both outputs came from identical runs so they should match.
        crate::lab_integration::assert_outputs_match(&result.baseline, &result.candidate);
    }

    #[test]
    fn match_ratio_empty_frames() {
        let result = ShadowRunResult {
            verdict: ShadowVerdict::Match,
            scenario_name: "empty".to_string(),
            seed: 0,
            frame_comparisons: vec![],
            first_divergence: None,
            frames_compared: 0,
            baseline: LabOutput {
                frame_count: 0,
                frame_records: vec![],
                event_count: 0,
                event_log: vec![],
                tick_count: 0,
                anomaly_count: 0,
            },
            candidate: LabOutput {
                frame_count: 0,
                frame_records: vec![],
                event_count: 0,
                event_log: vec![],
                tick_count: 0,
                anomaly_count: 0,
            },
            baseline_label: "baseline".to_string(),
            candidate_label: "candidate".to_string(),
            run_total: 1,
        };
        assert!((result.match_ratio() - 1.0).abs() < f64::EPSILON);
    }
}
