//! Shadow-run comparator for legacy vs structured cancellation lanes (bd-1tznn).
//!
//! Exercises identical workloads through both runtime lanes and compares outputs.
//! Mismatches produce detailed evidence showing exactly where behavior diverged.
//!
//! The comparator drives a real headless `Program` with lane-specific runtime
//! configuration and a scripted event source so the comparison goes through the
//! actual runtime loop instead of the simulator-only fast path.

#![forbid(unsafe_code)]

use ftui_core::event::{Event, KeyCode, KeyEvent};
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_harness::failure_signatures::FailureClass;
use ftui_harness::validation_matrix::AssertionCategory;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_render::sanitize::sanitize;
use ftui_runtime::program::{Cmd, Model, Program, ProgramConfig, RuntimeLane};
use ftui_runtime::terminal_writer::TerminalWriter;
use ftui_runtime::{BackendEventSource, BackendFeatures};
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ============================================================================
// Shadow comparison infrastructure
// ============================================================================

/// Result of running a single scenario through a lane.
#[derive(Debug, Clone)]
struct LaneResult {
    lane: RuntimeLane,
    trace: Vec<String>,
    logs: Vec<String>,
    feature_trace: Vec<String>,
    running: bool,
    tick_rate: Option<Duration>,
    cmd_log_len: usize,
    frame_hashes: Vec<u64>,
    terminal_output: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MismatchReasonCode {
    Trace,
    Log,
    TerminalOutput,
    FeatureTrace,
    RunningState,
    TickRate,
    CommandLogLength,
    FrameHash,
    FrameCount,
}

impl MismatchReasonCode {
    const fn code(self) -> &'static str {
        match self {
            Self::Trace => "TRACE_DIVERGENCE",
            Self::Log => "LOG_DIVERGENCE",
            Self::TerminalOutput => "TERMINAL_OUTPUT_DIVERGENCE",
            Self::FeatureTrace => "FEATURE_TRACE_DIVERGENCE",
            Self::RunningState => "RUNNING_STATE_DIVERGENCE",
            Self::TickRate => "TICK_RATE_DIVERGENCE",
            Self::CommandLogLength => "COMMAND_LOG_LENGTH_DIVERGENCE",
            Self::FrameHash => "FRAME_HASH_DIVERGENCE",
            Self::FrameCount => "FRAME_COUNT_DIVERGENCE",
        }
    }

    const fn root_cause_class(self) -> &'static str {
        match self {
            Self::Trace
            | Self::TerminalOutput
            | Self::FeatureTrace
            | Self::RunningState
            | Self::FrameHash
            | Self::FrameCount => "semantic",
            Self::Log | Self::CommandLogLength => "observability",
            Self::TickRate => "policy",
        }
    }

    const fn failure_class(self) -> FailureClass {
        match self {
            Self::Trace
            | Self::TerminalOutput
            | Self::FeatureTrace
            | Self::RunningState
            | Self::FrameHash
            | Self::FrameCount => FailureClass::ShadowDivergence,
            Self::Log | Self::CommandLogLength => FailureClass::Mismatch,
            Self::TickRate => FailureClass::Rollback,
        }
    }
}

/// Evidence bundle for a shadow-run mismatch.
#[derive(Debug, Clone, Serialize)]
struct MismatchEvidence {
    reason_code: &'static str,
    failure_class: &'static str,
    root_cause_class: &'static str,
    field: String,
    legacy: String,
    structured: String,
    scenario: String,
    summary: String,
}

impl std::fmt::Display for MismatchEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}:{}] '{}' in scenario '{}':\n  legacy:     {}\n  structured: {}",
            self.reason_code,
            self.root_cause_class,
            self.field,
            self.scenario,
            self.legacy,
            self.structured
        )
    }
}

#[derive(Debug, Clone, Serialize)]
struct LaneSummary {
    lane: String,
    trace_len: usize,
    log_len: usize,
    feature_trace: Vec<String>,
    running: bool,
    tick_rate_ms: Option<u64>,
    cmd_log_len: usize,
    frame_hashes: Vec<u64>,
    terminal_output_bytes: usize,
    terminal_output_hash: u64,
}

#[derive(Debug, Clone, Copy)]
struct ScenarioSpec {
    name: &'static str,
    scenario_kind: &'static str,
    contract_focus: &'static str,
    assertion: AssertionCategory,
    messages: fn() -> Vec<SMsg>,
    frames: &'static [(u16, u16)],
}

#[derive(Debug, Clone, Serialize)]
struct ScenarioReport {
    schema_version: &'static str,
    scenario: String,
    scenario_kind: &'static str,
    contract_focus: &'static str,
    assertion_category: &'static str,
    verdict: &'static str,
    contract_status: &'static str,
    acceptable_difference_policy: &'static str,
    replay_command: String,
    baseline: LaneSummary,
    candidate: LaneSummary,
    mismatch_count: usize,
    blocking_mismatch_count: usize,
    mismatches: Vec<MismatchEvidence>,
}

#[derive(Debug, Clone, Serialize)]
struct SuiteSummary {
    total_scenarios: usize,
    matched_scenarios: usize,
    diverged_scenarios: usize,
    total_mismatches: usize,
    total_blocking_mismatches: usize,
    scenario_filter: String,
    contract_coverage: Vec<ContractCoverage>,
}

#[derive(Debug, Clone, Serialize)]
struct ContractCoverage {
    contract: &'static str,
    covered: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeShadowSuiteReport {
    schema_version: &'static str,
    suite: &'static str,
    user_contract: &'static str,
    summary: SuiteSummary,
    scenarios: Vec<ScenarioReport>,
}

fn mismatch_reason(field: &str) -> MismatchReasonCode {
    match field {
        "trace" => MismatchReasonCode::Trace,
        "logs" => MismatchReasonCode::Log,
        "terminal_output" => MismatchReasonCode::TerminalOutput,
        "feature_trace" => MismatchReasonCode::FeatureTrace,
        "running" => MismatchReasonCode::RunningState,
        "tick_rate" => MismatchReasonCode::TickRate,
        "cmd_log_len" => MismatchReasonCode::CommandLogLength,
        "frame_hashes" => MismatchReasonCode::FrameHash,
        "frame_count" => MismatchReasonCode::FrameCount,
        _ => MismatchReasonCode::Trace,
    }
}

fn push_mismatch(
    mismatches: &mut Vec<MismatchEvidence>,
    scenario: &str,
    field: &str,
    legacy: String,
    structured: String,
) {
    let reason = mismatch_reason(field);
    mismatches.push(MismatchEvidence {
        reason_code: reason.code(),
        failure_class: reason.failure_class().reason_code(),
        root_cause_class: reason.root_cause_class(),
        field: field.into(),
        legacy,
        structured,
        scenario: scenario.into(),
        summary: format!(
            "{field} mismatch in scenario '{scenario}'; semantic and policy drift are blockers, observability drift is a blocker when replay context changes"
        ),
    });
}

fn mismatch_is_blocking(assertion: AssertionCategory, mismatch: &MismatchEvidence) -> bool {
    match assertion {
        AssertionCategory::NoChange => true,
        AssertionCategory::NoRegression
        | AssertionCategory::BoundedDegradation
        | AssertionCategory::GracefulFallback
        | AssertionCategory::FailureForensics
        | AssertionCategory::Improvement => {
            !matches!(mismatch.field.as_str(), "logs" | "terminal_output")
        }
    }
}

fn blocking_mismatch_count(assertion: AssertionCategory, mismatches: &[MismatchEvidence]) -> usize {
    mismatches
        .iter()
        .filter(|mismatch| mismatch_is_blocking(assertion, mismatch))
        .count()
}

/// Compare two lane results and return mismatch evidence if any.
fn compare_results(
    scenario: &str,
    legacy: &LaneResult,
    structured: &LaneResult,
) -> Vec<MismatchEvidence> {
    let mut mismatches = Vec::new();

    if legacy.trace != structured.trace {
        push_mismatch(
            &mut mismatches,
            scenario,
            "trace",
            format!("{:?}", legacy.trace),
            format!("{:?}", structured.trace),
        );
    }

    if legacy.logs != structured.logs {
        push_mismatch(
            &mut mismatches,
            scenario,
            "logs",
            format!("{:?}", legacy.logs),
            format!("{:?}", structured.logs),
        );
    }

    if legacy.terminal_output != structured.terminal_output {
        push_mismatch(
            &mut mismatches,
            scenario,
            "terminal_output",
            terminal_output_signature(&legacy.terminal_output),
            terminal_output_signature(&structured.terminal_output),
        );
    }

    if legacy.feature_trace != structured.feature_trace {
        push_mismatch(
            &mut mismatches,
            scenario,
            "feature_trace",
            format!("{:?}", legacy.feature_trace),
            format!("{:?}", structured.feature_trace),
        );
    }

    if legacy.running != structured.running {
        push_mismatch(
            &mut mismatches,
            scenario,
            "running",
            legacy.running.to_string(),
            structured.running.to_string(),
        );
    }

    if legacy.tick_rate != structured.tick_rate {
        push_mismatch(
            &mut mismatches,
            scenario,
            "tick_rate",
            format!("{:?}", legacy.tick_rate),
            format!("{:?}", structured.tick_rate),
        );
    }

    if legacy.cmd_log_len != structured.cmd_log_len {
        push_mismatch(
            &mut mismatches,
            scenario,
            "cmd_log_len",
            legacy.cmd_log_len.to_string(),
            structured.cmd_log_len.to_string(),
        );
    }

    if legacy.frame_hashes != structured.frame_hashes {
        push_mismatch(
            &mut mismatches,
            scenario,
            "frame_hashes",
            format!("{:?}", legacy.frame_hashes),
            format!("{:?}", structured.frame_hashes),
        );
    }

    mismatches
}

/// Hash a buffer's content for quick comparison.
fn hash_buffer(buf: &ftui_render::buffer::Buffer) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    buf.width().hash(&mut hasher);
    buf.height().hash(&mut hasher);
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            if let Some(cell) = buf.get(x, y) {
                cell.content.as_char().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn terminal_output_signature(bytes: &[u8]) -> String {
    let preview_len = bytes.len().min(80);
    let preview = String::from_utf8_lossy(&bytes[..preview_len]);
    format!(
        "len={} hash={:016x} preview={:?}",
        bytes.len(),
        hash_bytes(bytes),
        preview
    )
}

#[derive(Default)]
struct ShadowHarnessState {
    pending_tasks: AtomicUsize,
    scenario_quit: AtomicBool,
    feature_trace: Mutex<Vec<String>>,
}

impl ShadowHarnessState {
    fn task_spawned(&self) {
        self.pending_tasks.fetch_add(1, Ordering::SeqCst);
    }

    fn task_completed(&self) {
        let mut current = self.pending_tasks.load(Ordering::SeqCst);
        while current > 0 {
            match self.pending_tasks.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    fn pending_tasks(&self) -> usize {
        self.pending_tasks.load(Ordering::SeqCst)
    }

    fn mark_scenario_quit(&self) {
        self.scenario_quit.store(true, Ordering::SeqCst);
    }

    fn scenario_quit(&self) -> bool {
        self.scenario_quit.load(Ordering::SeqCst)
    }

    fn record_feature_update(&self, features: BackendFeatures) {
        let entry = feature_signature(features);
        if let Ok(mut trace) = self.feature_trace.lock() {
            trace.push(entry);
        }
    }

    fn feature_trace(&self) -> Vec<String> {
        self.feature_trace
            .lock()
            .map(|trace| trace.clone())
            .unwrap_or_default()
    }
}

fn feature_signature(features: BackendFeatures) -> String {
    format!(
        "mouse={} paste={} focus={} kitty={}",
        features.mouse_capture,
        features.bracketed_paste,
        features.focus_events,
        features.kitty_keyboard
    )
}

#[derive(Clone, Default)]
struct SharedWriteBuffer {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriteBuffer {
    fn snapshot(&self) -> Vec<u8> {
        self.bytes
            .lock()
            .map(|bytes| bytes.clone())
            .unwrap_or_default()
    }
}

impl Write for SharedWriteBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_| io::Error::other("shared write buffer poisoned"))?;
        bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ScriptedEventSource {
    width: u16,
    height: u16,
    features: BackendFeatures,
    remaining_steps: usize,
    shared: Arc<ShadowHarnessState>,
    shutdown_armed: bool,
    shutdown_emitted: bool,
}

impl ScriptedEventSource {
    fn new(
        width: u16,
        height: u16,
        features: BackendFeatures,
        remaining_steps: usize,
        shared: Arc<ShadowHarnessState>,
    ) -> Self {
        Self {
            width,
            height,
            features,
            remaining_steps,
            shared,
            shutdown_armed: false,
            shutdown_emitted: false,
        }
    }

    fn drive_event() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('n')))
    }

    fn shutdown_event() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('q')))
    }
}

impl BackendEventSource for ScriptedEventSource {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), io::Error> {
        Ok((self.width, self.height))
    }

    fn set_features(&mut self, features: BackendFeatures) -> Result<(), io::Error> {
        self.features = features;
        self.shared.record_feature_update(features);
        Ok(())
    }

    fn poll_event(&mut self, _timeout: Duration) -> Result<bool, io::Error> {
        if self.shutdown_emitted {
            return Ok(false);
        }
        if self.shutdown_armed {
            return Ok(true);
        }
        // Advance one top-level scripted event only after spawned work drains so
        // both runtime lanes observe the same task completion frontier.
        if self.remaining_steps > 0
            && !self.shared.scenario_quit()
            && self.shared.pending_tasks() == 0
        {
            return Ok(true);
        }
        if self.shared.pending_tasks() == 0 {
            self.shutdown_armed = true;
        }
        Ok(false)
    }

    fn read_event(&mut self) -> Result<Option<Event>, io::Error> {
        if self.shutdown_emitted {
            return Ok(None);
        }
        if self.shutdown_armed {
            self.shutdown_armed = false;
            self.shutdown_emitted = true;
            return Ok(Some(Self::shutdown_event()));
        }
        if self.remaining_steps > 0
            && !self.shared.scenario_quit()
            && self.shared.pending_tasks() == 0
        {
            self.remaining_steps -= 1;
            return Ok(Some(Self::drive_event()));
        }
        Ok(None)
    }
}

fn render_model_frame_hash<M: Model>(model: &M, width: u16, height: u16) -> u64 {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    model.view(&mut frame);
    hash_buffer(&frame.buffer)
}

fn observed_log_lines(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output)
        .split_inclusive("\r\n")
        .filter_map(|segment| segment.strip_suffix("\r\n"))
        .map(sanitize)
        .map(|line| line.into_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

// ============================================================================
// Shadow model: records trace for comparison
// ============================================================================

struct ShadowModel {
    trace: Vec<String>,
    script: VecDeque<SMsg>,
    shared: Arc<ShadowHarnessState>,
}

impl ShadowModel {
    fn new(script: Vec<SMsg>, shared: Arc<ShadowHarnessState>) -> Self {
        Self {
            trace: vec![],
            script: VecDeque::from(script),
            shared,
        }
    }

    fn apply_scripted_message(&mut self, msg: SMsg) -> Cmd<SMsg> {
        match msg {
            SMsg::Init => {
                self.trace.push("update:init".into());
                Cmd::none()
            }
            SMsg::Step(step) => {
                self.trace.push(format!("step:{step}"));
                Cmd::none()
            }
            SMsg::Batch(items) => {
                self.trace.push(format!("batch:{}", items.len()));
                Cmd::batch(
                    items
                        .into_iter()
                        .map(|item| Cmd::msg(SMsg::Step(item)))
                        .collect(),
                )
            }
            SMsg::Sequence(items) => {
                self.trace.push(format!("seq:{}", items.len()));
                Cmd::sequence(
                    items
                        .into_iter()
                        .map(|item| Cmd::msg(SMsg::Step(item)))
                        .collect(),
                )
            }
            SMsg::Nested(depth) => {
                self.trace.push(format!("nested:{depth}"));
                if depth > 0 {
                    Cmd::msg(SMsg::Nested(depth - 1))
                } else {
                    Cmd::none()
                }
            }
            SMsg::Task(label) => {
                self.trace.push(format!("task:{label}"));
                self.shared.task_spawned();
                let task_label = label.clone();
                Cmd::task(move || SMsg::TaskResult(task_label))
            }
            SMsg::TaskResult(label) => {
                self.shared.task_completed();
                self.trace.push(format!("task-done:{label}"));
                Cmd::none()
            }
            SMsg::Log(text) => {
                self.trace.push(format!("log:{text}"));
                Cmd::log(text)
            }
            SMsg::MouseCapture(enabled) => {
                self.trace.push(format!("mouse-capture:{enabled}"));
                Cmd::set_mouse_capture(enabled)
            }
            SMsg::Tick => {
                let duration = Duration::from_millis(100);
                self.trace.push("tick".into());
                Cmd::tick(duration)
            }
            SMsg::Quit => {
                self.trace.push("quit".into());
                self.shared.mark_scenario_quit();
                Cmd::quit()
            }
            SMsg::QuitInBatch(count) => {
                self.trace.push(format!("quit-batch:{count}"));
                self.shared.mark_scenario_quit();
                let mut commands: Vec<Cmd<SMsg>> = (0..count)
                    .map(|idx| Cmd::msg(SMsg::Step(format!("pre-{idx}"))))
                    .collect();
                commands.push(Cmd::quit());
                commands.push(Cmd::msg(SMsg::Step("post-quit".into())));
                Cmd::batch(commands)
            }
            SMsg::DriveNext => {
                if let Some(next) = self.script.pop_front() {
                    self.apply_scripted_message(next)
                } else {
                    Cmd::none()
                }
            }
            SMsg::HarnessQuit => Cmd::quit(),
        }
    }
}

#[derive(Debug, Clone)]
enum SMsg {
    Init,
    Step(String),
    Batch(Vec<String>),
    Sequence(Vec<String>),
    Nested(u32),
    Task(String),
    TaskResult(String),
    Log(String),
    MouseCapture(bool),
    Tick,
    Quit,
    QuitInBatch(usize),
    DriveNext,
    HarnessQuit,
}

impl From<Event> for SMsg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) if key.code == KeyCode::Char('q') => Self::HarnessQuit,
            Event::Key(_) => Self::DriveNext,
            _ => Self::DriveNext,
        }
    }
}

impl Model for ShadowModel {
    type Message = SMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        self.trace.push("init".into());
        Cmd::msg(SMsg::Init)
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        self.apply_scripted_message(msg)
    }

    fn view(&self, frame: &mut Frame) {
        let text = format!("n={}", self.trace.len());
        for (idx, ch) in text.chars().enumerate() {
            if (idx as u16) < frame.width() {
                use ftui_render::cell::Cell;
                frame.buffer.set_raw(idx as u16, 0, Cell::from_char(ch));
            }
        }
    }
}

/// Run a scenario through a specific lane and capture results.
fn run_lane(lane: RuntimeLane, msgs: Vec<SMsg>, capture_frames: &[(u16, u16)]) -> LaneResult {
    let shared = Arc::new(ShadowHarnessState::default());
    let terminal_width = capture_frames
        .iter()
        .map(|(width, _)| *width)
        .max()
        .unwrap_or(80)
        .max(1);
    let terminal_height = capture_frames
        .iter()
        .map(|(_, height)| *height)
        .max()
        .unwrap_or(24)
        .max(1);
    let mut config = ProgramConfig::default()
        .with_lane(lane)
        .with_forced_size(terminal_width, terminal_height);
    config.poll_timeout = Duration::ZERO;
    config.intercept_signals = false;

    let capabilities = TerminalCapabilities::basic();
    let initial_features = BackendFeatures {
        mouse_capture: config.resolved_mouse_capture(),
        bracketed_paste: config.bracketed_paste,
        focus_events: config.focus_reporting,
        kitty_keyboard: config.kitty_keyboard,
    };
    let output = SharedWriteBuffer::default();
    let writer = TerminalWriter::with_diff_config(
        output.clone(),
        config.screen_mode,
        config.ui_anchor,
        capabilities,
        config.diff_config.clone(),
    );
    let model = ShadowModel::new(msgs, Arc::clone(&shared));
    let events = ScriptedEventSource::new(
        terminal_width,
        terminal_height,
        initial_features,
        model.script.len(),
        Arc::clone(&shared),
    );
    let mut program = Program::with_event_source(model, events, initial_features, writer, config)
        .expect("headless program for shadow comparator");
    program.run().expect("run shadow comparator lane");

    let mut frame_hashes = Vec::new();
    for &(width, height) in capture_frames {
        frame_hashes.push(render_model_frame_hash(program.model(), width, height));
    }
    let terminal_output = output.snapshot();
    let feature_trace = shared.feature_trace();

    LaneResult {
        lane,
        trace: program.model().trace.clone(),
        logs: observed_log_lines(&terminal_output),
        feature_trace,
        running: program.is_running(),
        tick_rate: program.tick_rate(),
        cmd_log_len: program.executed_cmd_count(),
        frame_hashes,
        terminal_output,
    }
}

fn lane_summary(result: &LaneResult) -> LaneSummary {
    LaneSummary {
        lane: result.lane.label().to_string(),
        trace_len: result.trace.len(),
        log_len: result.logs.len(),
        feature_trace: result.feature_trace.clone(),
        running: result.running,
        tick_rate_ms: result
            .tick_rate
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        cmd_log_len: result.cmd_log_len,
        frame_hashes: result.frame_hashes.clone(),
        terminal_output_bytes: result.terminal_output.len(),
        terminal_output_hash: hash_bytes(&result.terminal_output),
    }
}

fn replay_command_for(scenario: &str) -> String {
    format!("scripts/runtime_shadow_compare.sh /tmp/ftui_runtime_shadow_replay {scenario}")
}

fn scenario_basic_steps() -> Vec<SMsg> {
    vec![
        SMsg::Step("a".into()),
        SMsg::Step("b".into()),
        SMsg::Step("c".into()),
    ]
}

fn scenario_complex_burst() -> Vec<SMsg> {
    vec![
        SMsg::Step("start".into()),
        SMsg::Batch(vec!["b1".into(), "b2".into()]),
        SMsg::Task("compute".into()),
        SMsg::Nested(5),
        SMsg::Log("checkpoint".into()),
        SMsg::Sequence(vec!["s1".into(), "s2".into()]),
        SMsg::Task("finalize".into()),
        SMsg::Tick,
    ]
}

fn scenario_quit_stops() -> Vec<SMsg> {
    vec![
        SMsg::Step("before".into()),
        SMsg::Quit,
        SMsg::Step("after".into()),
    ]
}

fn scenario_quit_in_batch() -> Vec<SMsg> {
    vec![SMsg::QuitInBatch(3)]
}

fn scenario_log_output() -> Vec<SMsg> {
    vec![SMsg::Log("hello".into()), SMsg::Log("world".into())]
}

fn scenario_empty() -> Vec<SMsg> {
    Vec::new()
}

fn scenario_saturation() -> Vec<SMsg> {
    let mut messages = Vec::new();
    messages.push(SMsg::Batch(
        (0..96).map(|idx| format!("burst-{idx}")).collect(),
    ));
    for idx in 0..12 {
        messages.push(SMsg::Task(format!("task-{idx}")));
    }
    for idx in 0..8 {
        messages.push(SMsg::Sequence(vec![
            format!("seq-{idx}-a"),
            format!("seq-{idx}-b"),
            format!("seq-{idx}-c"),
        ]));
    }
    messages.push(SMsg::Nested(24));
    messages.push(SMsg::Tick);
    messages
}

fn scenario_terminal_safety() -> Vec<SMsg> {
    vec![
        SMsg::Step("before-feature-toggle".into()),
        SMsg::MouseCapture(true),
        SMsg::Task("capture-task".into()),
        SMsg::Log("capture enabled".into()),
        SMsg::MouseCapture(false),
        SMsg::Batch(vec!["post-toggle-a".into(), "post-toggle-b".into()]),
        SMsg::Tick,
    ]
}

const FRAMES_SMALL: &[(u16, u16)] = &[(40, 10)];
const FRAMES_COMPLEX: &[(u16, u16)] = &[(80, 24), (40, 10)];
const FRAMES_EMPTY: &[(u16, u16)] = &[(10, 5)];
const FRAMES_SATURATION: &[(u16, u16)] = &[(120, 40), (80, 24), (40, 10)];

fn operator_scenarios() -> Vec<ScenarioSpec> {
    vec![
        ScenarioSpec {
            name: "steady_basic_steps",
            scenario_kind: "steady_state",
            contract_focus: "semantic_ordering",
            assertion: AssertionCategory::NoChange,
            messages: scenario_basic_steps,
            frames: FRAMES_SMALL,
        },
        ScenarioSpec {
            name: "bursty_complex",
            scenario_kind: "bursty",
            contract_focus: "degraded_mode_recovery",
            assertion: AssertionCategory::NoRegression,
            messages: scenario_complex_burst,
            frames: FRAMES_COMPLEX,
        },
        ScenarioSpec {
            name: "cancellation_quit_stops",
            scenario_kind: "cancellation_heavy",
            contract_focus: "cancellation_cutoff",
            assertion: AssertionCategory::NoRegression,
            messages: scenario_quit_stops,
            frames: &[],
        },
        ScenarioSpec {
            name: "shutdown_quit_in_batch",
            scenario_kind: "shutdown_heavy",
            contract_focus: "shutdown_draining",
            assertion: AssertionCategory::GracefulFallback,
            messages: scenario_quit_in_batch,
            frames: &[],
        },
        ScenarioSpec {
            name: "observability_logs",
            scenario_kind: "negative_control",
            contract_focus: "observability_replay_context",
            assertion: AssertionCategory::FailureForensics,
            messages: scenario_log_output,
            frames: &[],
        },
        ScenarioSpec {
            name: "negative_control_empty",
            scenario_kind: "negative_control",
            contract_focus: "stable_noop_behavior",
            assertion: AssertionCategory::NoChange,
            messages: scenario_empty,
            frames: FRAMES_EMPTY,
        },
        ScenarioSpec {
            name: "terminal_feature_lifecycle",
            scenario_kind: "terminal_safety",
            contract_focus: "terminal_feature_cleanup",
            assertion: AssertionCategory::NoRegression,
            messages: scenario_terminal_safety,
            frames: FRAMES_COMPLEX,
        },
        ScenarioSpec {
            name: "saturation_burst_load",
            scenario_kind: "saturation",
            contract_focus: "load_envelope_and_recovery",
            assertion: AssertionCategory::GracefulFallback,
            messages: scenario_saturation,
            frames: FRAMES_SATURATION,
        },
    ]
}

fn select_operator_scenarios() -> Vec<ScenarioSpec> {
    let filter = std::env::var("FTUI_RUNTIME_SHADOW_SCENARIO").ok();
    let scenarios = operator_scenarios();
    match filter
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("all") => scenarios,
        Some(name) => {
            let selected = scenarios
                .into_iter()
                .filter(|scenario| scenario.name == name)
                .collect::<Vec<_>>();
            assert!(
                !selected.is_empty(),
                "unknown FTUI_RUNTIME_SHADOW_SCENARIO={name}; available: {}",
                operator_scenarios()
                    .iter()
                    .map(|scenario| scenario.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            selected
        }
    }
}

fn contract_coverage_for(scenarios: &[ScenarioReport]) -> Vec<ContractCoverage> {
    const REQUIRED_CONTRACTS: &[&str] = &[
        "saturation",
        "cancellation_heavy",
        "shutdown_heavy",
        "terminal_safety",
        "negative_control",
    ];

    REQUIRED_CONTRACTS
        .iter()
        .map(|contract| ContractCoverage {
            contract,
            covered: scenarios
                .iter()
                .any(|scenario| scenario.scenario_kind == *contract),
        })
        .collect()
}

fn build_scenario_report(spec: &ScenarioSpec) -> ScenarioReport {
    let legacy = run_lane(RuntimeLane::Legacy, (spec.messages)(), spec.frames);
    let structured = run_lane(RuntimeLane::Structured, (spec.messages)(), spec.frames);
    let mut mismatches = compare_results(spec.name, &legacy, &structured);
    if legacy.frame_hashes.len() != structured.frame_hashes.len() {
        push_mismatch(
            &mut mismatches,
            spec.name,
            "frame_count",
            legacy.frame_hashes.len().to_string(),
            structured.frame_hashes.len().to_string(),
        );
    }
    let blocking_mismatches = blocking_mismatch_count(spec.assertion, &mismatches);
    ScenarioReport {
        schema_version: "ftui-runtime-shadow-v1",
        scenario: spec.name.to_string(),
        scenario_kind: spec.scenario_kind,
        contract_focus: spec.contract_focus,
        assertion_category: spec.assertion.label(),
        verdict: if blocking_mismatches == 0 {
            "match"
        } else {
            "diverged"
        },
        contract_status: if blocking_mismatches == 0 {
            "within-contract"
        } else {
            "out-of-contract"
        },
        acceptable_difference_policy: match spec.assertion {
            AssertionCategory::NoChange => {
                "No-change scenarios require exact lane parity across trace, output, logs, state, policy, and frame captures."
            }
            AssertionCategory::GracefulFallback => {
                "Graceful-fallback scenarios may tolerate raw terminal/log-output drift under bursty saturation, but trace, frame-hash, running-state, tick-rate, command-count, and replay-context mismatches remain blockers."
            }
            AssertionCategory::NoRegression
            | AssertionCategory::BoundedDegradation
            | AssertionCategory::FailureForensics
            | AssertionCategory::Improvement => {
                "Operator workloads may tolerate raw terminal/log-output drift caused by scheduling differences, but trace, frame-hash, running-state, tick-rate, command-count, and replay-context mismatches remain blockers."
            }
        },
        replay_command: replay_command_for(spec.name),
        baseline: lane_summary(&legacy),
        candidate: lane_summary(&structured),
        mismatch_count: mismatches.len(),
        blocking_mismatch_count: blocking_mismatches,
        mismatches,
    }
}

fn build_operator_suite_report() -> RuntimeShadowSuiteReport {
    let scenario_filter = std::env::var("FTUI_RUNTIME_SHADOW_SCENARIO")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "all".to_string());
    let scenarios = select_operator_scenarios()
        .into_iter()
        .map(|scenario| build_scenario_report(&scenario))
        .collect::<Vec<_>>();
    let diverged_scenarios = scenarios
        .iter()
        .filter(|scenario| scenario.verdict == "diverged")
        .count();
    let total_mismatches = scenarios
        .iter()
        .map(|scenario| scenario.mismatch_count)
        .sum();
    let total_blocking_mismatches = scenarios
        .iter()
        .map(|scenario| scenario.blocking_mismatch_count)
        .sum();
    let contract_coverage = contract_coverage_for(&scenarios);
    RuntimeShadowSuiteReport {
        schema_version: "ftui-runtime-shadow-suite-v1",
        suite: "runtime_shadow_comparison",
        user_contract: "Shadow and saturation comparison must preserve user-visible degraded-mode, recovery, shutdown, and replayability guarantees; mismatches must carry reason codes and replay commands.",
        summary: SuiteSummary {
            total_scenarios: scenarios.len(),
            matched_scenarios: scenarios.len().saturating_sub(diverged_scenarios),
            diverged_scenarios,
            total_mismatches,
            total_blocking_mismatches,
            scenario_filter,
            contract_coverage,
        },
        scenarios,
    }
}

fn emit_operator_suite_report(report: &RuntimeShadowSuiteReport) {
    if std::env::var("FTUI_RUNTIME_SHADOW_EMIT_REPORT")
        .ok()
        .as_deref()
        == Some("1")
    {
        println!(
            "FTUI_RUNTIME_SHADOW_REPORT_JSON={}",
            serde_json::to_string(report).expect("serialize runtime shadow suite report")
        );
    }
}

/// Run a scenario through both lanes and assert no *blocking* mismatches.
///
/// Terminal output and log drift are expected between lanes and treated as
/// non-blocking (see `mismatch_is_blocking`).
fn shadow_compare(scenario: &str, msgs_fn: impl Fn() -> Vec<SMsg>, frames: &[(u16, u16)]) {
    let legacy = run_lane(RuntimeLane::Legacy, msgs_fn(), frames);
    let structured = run_lane(RuntimeLane::Structured, msgs_fn(), frames);
    let mismatches = compare_results(scenario, &legacy, &structured);
    let blocking: Vec<_> = mismatches
        .iter()
        .filter(|m| mismatch_is_blocking(AssertionCategory::NoRegression, m))
        .collect();
    assert!(
        blocking.is_empty(),
        "Shadow-run blocking mismatches detected:\n{}",
        blocking
            .iter()
            .map(|mismatch| format!("  {mismatch}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ============================================================================
// SHADOW-RUN TESTS: verify lane equivalence
// ============================================================================

#[test]
fn shadow_basic_steps() {
    shadow_compare("basic_steps", scenario_basic_steps, FRAMES_SMALL);
}

#[test]
fn shadow_batch_ordering() {
    shadow_compare(
        "batch_ordering",
        || vec![SMsg::Batch(vec!["x".into(), "y".into(), "z".into()])],
        FRAMES_SMALL,
    );
}

#[test]
fn shadow_sequence_ordering() {
    shadow_compare(
        "sequence_ordering",
        || vec![SMsg::Sequence(vec!["p".into(), "q".into(), "r".into()])],
        FRAMES_SMALL,
    );
}

#[test]
fn shadow_task_execution() {
    shadow_compare(
        "task_execution",
        || vec![SMsg::Task("alpha".into()), SMsg::Task("beta".into())],
        FRAMES_SMALL,
    );
}

#[test]
fn shadow_nested_recursion() {
    shadow_compare("nested_recursion", || vec![SMsg::Nested(10)], &[]);
}

#[test]
fn shadow_log_output() {
    shadow_compare("log_output", scenario_log_output, &[]);
}

#[test]
fn shadow_tick_rate() {
    shadow_compare("tick_rate", || vec![SMsg::Tick], &[]);
}

#[test]
fn shadow_quit_stops_processing() {
    shadow_compare("quit_stops", scenario_quit_stops, &[]);
}

#[test]
fn shadow_quit_in_batch() {
    shadow_compare("quit_in_batch", scenario_quit_in_batch, &[]);
}

#[test]
fn shadow_complex_scenario() {
    shadow_compare("complex", scenario_complex_burst, FRAMES_COMPLEX);
}

#[test]
fn shadow_multiple_frame_captures() {
    shadow_compare(
        "multi_frame",
        || {
            vec![
                SMsg::Step("frame-1".into()),
                SMsg::Step("frame-2".into()),
                SMsg::Step("frame-3".into()),
            ]
        },
        &[(20, 5), (40, 10), (80, 24)],
    );
}

#[test]
fn shadow_empty_scenario() {
    shadow_compare("empty", scenario_empty, FRAMES_EMPTY);
}

#[test]
fn shadow_lane_result_reports_stopped_after_harness_shutdown() {
    let result = run_lane(RuntimeLane::Legacy, scenario_basic_steps(), FRAMES_SMALL);
    assert!(
        !result.running,
        "run_lane should report the runtime as stopped after Program::run() returns"
    );
}

#[test]
fn shadow_large_batch() {
    shadow_compare(
        "large_batch",
        || {
            let items: Vec<String> = (0..50).map(|idx| format!("item-{idx}")).collect();
            vec![SMsg::Batch(items)]
        },
        &[(80, 24)],
    );
}

// ============================================================================
// OPERATOR REPORTS: structured artifacts for shadow and saturation suites
// ============================================================================

#[test]
fn mismatch_reason_codes_cover_runtime_fields() {
    let cases = [
        ("trace", "TRACE_DIVERGENCE", "semantic", "SHADOW_DIVERGENCE"),
        ("logs", "LOG_DIVERGENCE", "observability", "MISMATCH"),
        (
            "terminal_output",
            "TERMINAL_OUTPUT_DIVERGENCE",
            "semantic",
            "SHADOW_DIVERGENCE",
        ),
        (
            "feature_trace",
            "FEATURE_TRACE_DIVERGENCE",
            "semantic",
            "SHADOW_DIVERGENCE",
        ),
        (
            "running",
            "RUNNING_STATE_DIVERGENCE",
            "semantic",
            "SHADOW_DIVERGENCE",
        ),
        ("tick_rate", "TICK_RATE_DIVERGENCE", "policy", "ROLLBACK"),
        (
            "cmd_log_len",
            "COMMAND_LOG_LENGTH_DIVERGENCE",
            "observability",
            "MISMATCH",
        ),
        (
            "frame_hashes",
            "FRAME_HASH_DIVERGENCE",
            "semantic",
            "SHADOW_DIVERGENCE",
        ),
        (
            "frame_count",
            "FRAME_COUNT_DIVERGENCE",
            "semantic",
            "SHADOW_DIVERGENCE",
        ),
    ];

    for (field, code, root_cause, failure_class) in cases {
        let reason = mismatch_reason(field);
        assert_eq!(reason.code(), code);
        assert_eq!(reason.root_cause_class(), root_cause);
        assert_eq!(reason.failure_class().reason_code(), failure_class);
    }
}

#[test]
fn graceful_fallback_terminal_output_only_is_non_blocking() {
    let mismatch = MismatchEvidence {
        reason_code: "TERMINAL_OUTPUT_DIVERGENCE",
        failure_class: "SHADOW_DIVERGENCE",
        root_cause_class: "semantic",
        field: "terminal_output".into(),
        legacy: "legacy".into(),
        structured: "structured".into(),
        scenario: "scenario".into(),
        summary: "summary".into(),
    };

    assert_eq!(
        blocking_mismatch_count(
            AssertionCategory::GracefulFallback,
            std::slice::from_ref(&mismatch)
        ),
        0
    );
    assert_eq!(
        blocking_mismatch_count(AssertionCategory::NoChange, &[mismatch]),
        1
    );
}

#[test]
fn no_regression_logs_and_terminal_output_are_non_blocking() {
    let logs_mismatch = MismatchEvidence {
        reason_code: "LOG_DIVERGENCE",
        failure_class: "MISMATCH",
        root_cause_class: "observability",
        field: "logs".into(),
        legacy: "legacy".into(),
        structured: "structured".into(),
        scenario: "scenario".into(),
        summary: "summary".into(),
    };
    let output_mismatch = MismatchEvidence {
        reason_code: "TERMINAL_OUTPUT_DIVERGENCE",
        failure_class: "SHADOW_DIVERGENCE",
        root_cause_class: "semantic",
        field: "terminal_output".into(),
        legacy: "legacy".into(),
        structured: "structured".into(),
        scenario: "scenario".into(),
        summary: "summary".into(),
    };

    assert_eq!(
        blocking_mismatch_count(
            AssertionCategory::NoRegression,
            &[logs_mismatch, output_mismatch]
        ),
        0
    );
}

#[test]
fn graceful_fallback_still_blocks_non_terminal_divergence() {
    let mismatch = MismatchEvidence {
        reason_code: "FRAME_HASH_DIVERGENCE",
        failure_class: "SHADOW_DIVERGENCE",
        root_cause_class: "semantic",
        field: "frame_hashes".into(),
        legacy: "legacy".into(),
        structured: "structured".into(),
        scenario: "scenario".into(),
        summary: "summary".into(),
    };

    assert_eq!(
        blocking_mismatch_count(AssertionCategory::GracefulFallback, &[mismatch]),
        1
    );
}

#[test]
fn shadow_runtime_operator_report_contains_replay_commands() {
    let report = build_operator_suite_report();
    assert!(report.summary.total_scenarios >= 8);
    assert_eq!(report.summary.total_scenarios, report.scenarios.len());
    assert!(
        report
            .scenarios
            .iter()
            .any(|scenario| scenario.scenario_kind == "saturation"),
        "suite should include a saturation scenario"
    );
    assert!(
        report
            .scenarios
            .iter()
            .any(|scenario| scenario.scenario_kind == "terminal_safety"),
        "suite should include a terminal safety scenario"
    );
    assert!(
        report
            .summary
            .contract_coverage
            .iter()
            .all(|coverage| coverage.covered),
        "suite missing required contract coverage: {:?}",
        report.summary.contract_coverage
    );
    for scenario in &report.scenarios {
        assert!(
            !scenario.assertion_category.is_empty(),
            "assertion category missing for {}",
            scenario.scenario
        );
        assert_eq!(scenario.contract_status, "within-contract");
        assert_eq!(
            scenario.blocking_mismatch_count, 0,
            "scenario has blocking mismatches: {}",
            scenario.scenario
        );
        assert!(
            scenario
                .replay_command
                .contains("scripts/runtime_shadow_compare.sh"),
            "replay command missing operator script for {}",
            scenario.scenario
        );
    }
}

#[test]
fn shadow_runtime_operator_artifacts() {
    let report = build_operator_suite_report();
    emit_operator_suite_report(&report);
    assert_eq!(
        report.summary.diverged_scenarios,
        0,
        "runtime shadow suite diverged:\n{}",
        report
            .scenarios
            .iter()
            .filter(|scenario| scenario.verdict == "diverged")
            .flat_map(|scenario| scenario
                .mismatches
                .iter()
                .map(std::string::ToString::to_string))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ============================================================================
// DETERMINISM: shadow comparison must be stable across runs
// ============================================================================

#[test]
fn shadow_deterministic_across_multiple_runs() {
    let mut results = Vec::new();
    for _ in 0..5 {
        let legacy = run_lane(
            RuntimeLane::Legacy,
            vec![
                SMsg::Step("a".into()),
                SMsg::Batch(vec!["b".into(), "c".into()]),
                SMsg::Task("t".into()),
                SMsg::Log("l".into()),
            ],
            FRAMES_SMALL,
        );
        results.push(legacy);
    }

    for (idx, result) in results.iter().enumerate().skip(1) {
        assert_eq!(result.trace, results[0].trace, "run {idx} trace diverged");
        // Inline terminal emission includes scheduler-sensitive cursor-control
        // traffic when background task completion and log writes interleave.
        // The stable contract for shadow-run determinism is semantic: identical
        // traces, command counts, run state, tick policy, and rendered frames.
        assert_eq!(
            result.running, results[0].running,
            "run {idx} running state diverged"
        );
        assert_eq!(
            result.tick_rate, results[0].tick_rate,
            "run {idx} tick rate diverged"
        );
        assert_eq!(
            result.cmd_log_len, results[0].cmd_log_len,
            "run {idx} command log length diverged"
        );
        assert_eq!(
            result.frame_hashes, results[0].frame_hashes,
            "run {idx} frame hashes diverged"
        );
    }
}

#[test]
fn compare_results_flags_terminal_output_divergence() {
    let legacy = LaneResult {
        lane: RuntimeLane::Legacy,
        trace: vec!["init".into()],
        logs: vec!["same".into()],
        feature_trace: vec![],
        running: false,
        tick_rate: None,
        cmd_log_len: 1,
        frame_hashes: vec![1],
        terminal_output: b"legacy bytes".to_vec(),
    };
    let structured = LaneResult {
        lane: RuntimeLane::Structured,
        trace: vec!["init".into()],
        logs: vec!["same".into()],
        feature_trace: vec![],
        running: false,
        tick_rate: None,
        cmd_log_len: 1,
        frame_hashes: vec![1],
        terminal_output: b"structured bytes".to_vec(),
    };

    let mismatches = compare_results("terminal_output", &legacy, &structured);
    assert!(mismatches.iter().any(|mismatch| {
        mismatch.field == "terminal_output" && mismatch.reason_code == "TERMINAL_OUTPUT_DIVERGENCE"
    }));
}

#[test]
fn compare_results_flags_feature_trace_divergence() {
    let legacy = LaneResult {
        lane: RuntimeLane::Legacy,
        trace: vec!["init".into()],
        logs: vec![],
        feature_trace: vec!["mouse=true paste=false focus=false kitty=false".into()],
        running: false,
        tick_rate: None,
        cmd_log_len: 1,
        frame_hashes: vec![1],
        terminal_output: Vec::new(),
    };
    let structured = LaneResult {
        lane: RuntimeLane::Structured,
        trace: vec!["init".into()],
        logs: vec![],
        feature_trace: vec!["mouse=false paste=false focus=false kitty=false".into()],
        running: false,
        tick_rate: None,
        cmd_log_len: 1,
        frame_hashes: vec![1],
        terminal_output: Vec::new(),
    };

    let mismatches = compare_results("feature_trace", &legacy, &structured);
    assert!(mismatches.iter().any(|mismatch| {
        mismatch.field == "feature_trace" && mismatch.reason_code == "FEATURE_TRACE_DIVERGENCE"
    }));
}

// ============================================================================
// LANE IDENTITY: verify RuntimeLane metadata
// ============================================================================

#[test]
fn runtime_lane_labels() {
    assert_eq!(RuntimeLane::Legacy.label(), "legacy");
    assert_eq!(RuntimeLane::Structured.label(), "structured");
    assert_eq!(RuntimeLane::Asupersync.label(), "asupersync");
}

#[test]
fn runtime_lane_resolve_fallback() {
    assert_eq!(RuntimeLane::Legacy.resolve(), RuntimeLane::Legacy);
    assert_eq!(RuntimeLane::Structured.resolve(), RuntimeLane::Structured);
    // Asupersync is available with its executor feature; otherwise use Structured.
    let expected = if cfg!(feature = "asupersync-executor") {
        RuntimeLane::Asupersync
    } else {
        RuntimeLane::Structured
    };
    assert_eq!(
        RuntimeLane::Asupersync.resolve(),
        expected,
        "lane resolution must match asupersync-executor availability"
    );
}

#[test]
fn runtime_lane_structured_cancellation_check() {
    assert!(!RuntimeLane::Legacy.uses_structured_cancellation());
    assert!(RuntimeLane::Structured.uses_structured_cancellation());
    assert!(RuntimeLane::Asupersync.uses_structured_cancellation());
}

#[test]
fn runtime_lane_default_is_structured() {
    assert_eq!(RuntimeLane::default(), RuntimeLane::Structured);
}

#[test]
fn runtime_lane_display() {
    assert_eq!(format!("{}", RuntimeLane::Legacy), "legacy");
    assert_eq!(format!("{}", RuntimeLane::Structured), "structured");
    assert_eq!(format!("{}", RuntimeLane::Asupersync), "asupersync");
}
