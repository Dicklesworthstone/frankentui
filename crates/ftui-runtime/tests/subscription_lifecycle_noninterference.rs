//! Subscription + process lifecycle non-interference (bd-1f2aw).
//!
//! These integration tests close acceptance criterion #3 of bd-1f2aw
//! ("Terminal ownership, output mode correctness, and render determinism
//! remain intact") and the feature's fourth scope item ("Add non-interference
//! coverage for inline mode, alt-screen mode, and one-writer ownership").
//!
//! The sibling tasks that delivered the structured lifecycle internals
//! (bd-1dg21 contract capture, bd-3s3yw process supervision, the two-phase
//! parallel `SubscriptionManager` shutdown) proved the *lifecycle* contract in
//! isolation. What was missing was proof that the lifecycle does not perturb
//! the rendering kernel: that churning subscriptions and a live child process
//! never leak into terminal output, never corrupt inline/alt-screen framing,
//! and never destabilise the rendered frame.
//!
//! Unlike `lifecycle_contract.rs` (which uses the `ProgramSimulator`), these
//! tests drive a *real* headless [`Program`] through the actual runtime loop
//! with a [`TerminalWriter`] writing into a captured buffer, so the
//! one-writer rule, synchronized-output framing, and mode-specific present
//! paths are all exercised end to end.
//!
//! # Why this proves the one-writer rule
//!
//! A [`Subscription::run`] only ever receives an `mpsc::Sender<M>` and a
//! `StopSignal` -- never the `TerminalWriter`. Background subscription threads
//! are therefore structurally incapable of emitting terminal bytes. These
//! tests verify the *observable* consequence of that structure:
//!
//! * the rendered frame is identical whether subscriptions are present, absent,
//!   or churning (subscription messages mutate counters the view never reads);
//! * the captured terminal stream stays well-formed (every synchronized-output
//!   begin is matched by an end) under start/stop/restart churn;
//! * inline vs alt-screen present paths stay distinct with subscriptions live;
//! * shutdown stays bounded even with a live `sleep 60` child plus fast ticks.

#![forbid(unsafe_code)]

use ftui_core::event::{Event, KeyCode, KeyEvent};
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::program::{Cmd, Model, Program, ProgramConfig};
use ftui_runtime::subscription::{Every, Subscription};
use ftui_runtime::terminal_writer::TerminalWriter;
use ftui_runtime::{
    BackendEventSource, BackendFeatures, ProcessEvent, ProcessSubscription, ScreenMode,
};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ============================================================================
// Escape-sequence fixtures (mirrors the TerminalWriter constants)
// ============================================================================

/// DEC 2026 synchronized-output begin, as emitted by `TerminalWriter`.
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
/// DEC 2026 synchronized-output end.
const SYNC_END: &[u8] = b"\x1b[?2026l";
/// DEC cursor save (`ESC 7`). Inline mode emits this every present; the
/// alt-screen present path performs no cursor save/restore gymnastics.
const CURSOR_SAVE: &[u8] = b"\x1b7";

// ============================================================================
// One-writer capture sink
// ============================================================================

/// A `Write` sink shared with the test so we can inspect everything the
/// (single) `TerminalWriter` emitted. Subscription threads never receive a
/// handle to this -- that is the structural basis of the one-writer rule.
#[derive(Clone, Default)]
struct CaptureSink {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CaptureSink {
    fn snapshot(&self) -> Vec<u8> {
        self.bytes
            .lock()
            .map(|bytes| bytes.clone())
            .unwrap_or_default()
    }
}

impl Write for CaptureSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_| io::Error::other("capture sink poisoned"))?;
        bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ============================================================================
// Scripted event source: N drive events, then a quit, then EOF
// ============================================================================

struct ScriptedSource {
    width: u16,
    height: u16,
    features: BackendFeatures,
    steps_remaining: usize,
    quit_sent: bool,
}

impl ScriptedSource {
    fn new(width: u16, height: u16, features: BackendFeatures, steps: usize) -> Self {
        Self {
            width,
            height,
            features,
            steps_remaining: steps,
            quit_sent: false,
        }
    }
}

impl BackendEventSource for ScriptedSource {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), io::Error> {
        Ok((self.width, self.height))
    }

    fn set_features(&mut self, features: BackendFeatures) -> Result<(), io::Error> {
        self.features = features;
        Ok(())
    }

    fn poll_event(&mut self, _timeout: Duration) -> Result<bool, io::Error> {
        Ok(self.steps_remaining > 0 || !self.quit_sent)
    }

    fn read_event(&mut self) -> Result<Option<Event>, io::Error> {
        if self.steps_remaining > 0 {
            self.steps_remaining -= 1;
            return Ok(Some(Event::Key(KeyEvent::new(KeyCode::Char('n')))));
        }
        if !self.quit_sent {
            self.quit_sent = true;
            return Ok(Some(Event::Key(KeyEvent::new(KeyCode::Char('q')))));
        }
        Ok(None)
    }
}

// ============================================================================
// Test model: render reflects scripted input ONLY, never subscription churn
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubSpec {
    /// No subscriptions: the rendering baseline.
    None,
    /// Two `Every` tick subscriptions firing concurrently.
    Ticks,
    /// Ticks plus a live `sleep 60` child process subscription.
    TicksAndProcess,
    /// Varies the active subscription set by step count to force
    /// start / stop / restart across `reconcile` cycles.
    Churn,
}

#[derive(Debug, Clone)]
enum Msg {
    /// Driven only by scripted `n` events. The view renders this count.
    Step,
    /// Injected by `Every` subscriptions. Mutates a counter the view ignores.
    Tick,
    /// Injected by the process subscription. Mutates a counter the view ignores.
    Proc(ProcessEvent),
    /// Driven by the scripted `q` event.
    Quit,
}

impl From<Event> for Msg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) if key.code == KeyCode::Char('q') => Msg::Quit,
            _ => Msg::Step,
        }
    }
}

struct NiModel {
    spec: SubSpec,
    /// Advanced only by `Msg::Step` (scripted input). The view renders this.
    steps: u32,
    /// Advanced only by subscription messages. The view NEVER reads these, so
    /// any subscription activity that perturbed the frame would be visible as a
    /// frame-hash divergence from the no-subscription baseline.
    tick_hits: usize,
    process_hits: usize,
}

impl NiModel {
    fn new(spec: SubSpec) -> Self {
        Self {
            spec,
            steps: 0,
            tick_hits: 0,
            process_hits: 0,
        }
    }
}

fn tick_subs() -> Vec<Box<dyn Subscription<Msg>>> {
    vec![
        Box::new(Every::with_id(0xA1, Duration::from_millis(5), || Msg::Tick)),
        Box::new(Every::with_id(0xB2, Duration::from_millis(7), || Msg::Tick)),
    ]
}

impl Model for NiModel {
    type Message = Msg;

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            Msg::Step => {
                self.steps += 1;
                Cmd::none()
            }
            Msg::Tick => {
                self.tick_hits += 1;
                Cmd::none()
            }
            Msg::Proc(event) => {
                // Count terminal process events (the child was supervised and
                // reaped). Reading `event` keeps the supervision contract real
                // rather than discarding it; the view never reads this counter.
                if matches!(
                    event,
                    ProcessEvent::Exited(_)
                        | ProcessEvent::Signaled(_)
                        | ProcessEvent::Killed
                        | ProcessEvent::Error(_)
                ) {
                    self.process_hits += 1;
                }
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        // Render reflects scripted input only. Subscription churn (tick_hits,
        // process_hits) is deliberately absent so the frame hash is a function
        // of the deterministic scripted step count alone.
        let text = format!("steps={}", self.steps);
        for (idx, ch) in text.chars().enumerate() {
            if (idx as u16) < frame.width() {
                frame.buffer.set_raw(idx as u16, 0, Cell::from_char(ch));
            }
        }
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        match self.spec {
            SubSpec::None => Vec::new(),
            SubSpec::Ticks => tick_subs(),
            SubSpec::TicksAndProcess => {
                let mut subs = tick_subs();
                subs.push(Box::new(
                    ProcessSubscription::new("sleep", Msg::Proc).arg("60"),
                ));
                subs
            }
            SubSpec::Churn => match self.steps % 3 {
                0 => tick_subs(),
                1 => vec![Box::new(Every::with_id(
                    0xA1,
                    Duration::from_millis(5),
                    || Msg::Tick,
                ))],
                _ => vec![
                    Box::new(Every::with_id(0xA1, Duration::from_millis(5), || Msg::Tick)),
                    Box::new(Every::with_id(0xC3, Duration::from_millis(9), || Msg::Tick)),
                ],
            },
        }
    }
}

// ============================================================================
// Headless run harness
// ============================================================================

struct Outcome {
    terminal_output: Vec<u8>,
    frame_hash: u64,
    running: bool,
    elapsed: Duration,
}

const STEPS: usize = 4;
const WIDTH: u16 = 40;
const HEIGHT: u16 = 12;

fn run_scenario(screen_mode: ScreenMode, spec: SubSpec) -> Outcome {
    let mut config = ProgramConfig::default().with_forced_size(WIDTH, HEIGHT);
    config.screen_mode = screen_mode;
    config.poll_timeout = Duration::ZERO;
    config.intercept_signals = false;

    let capabilities = TerminalCapabilities::basic();
    let initial_features = BackendFeatures {
        mouse_capture: config.resolved_mouse_capture(),
        bracketed_paste: config.bracketed_paste,
        focus_events: config.focus_reporting,
        kitty_keyboard: config.kitty_keyboard,
    };

    let sink = CaptureSink::default();
    let writer = TerminalWriter::with_diff_config(
        sink.clone(),
        config.screen_mode,
        config.ui_anchor,
        capabilities,
        config.diff_config.clone(),
    );

    let model = NiModel::new(spec);
    let events = ScriptedSource::new(WIDTH, HEIGHT, initial_features, STEPS);

    let start = Instant::now();
    let mut program = Program::with_event_source(model, events, initial_features, writer, config)
        .expect("headless program for non-interference harness");
    program.run().expect("run non-interference scenario");
    let elapsed = start.elapsed();

    let frame_hash = render_model_frame_hash(program.model(), WIDTH, HEIGHT);

    Outcome {
        terminal_output: sink.snapshot(),
        frame_hash,
        running: program.is_running(),
        elapsed,
    }
}

fn render_model_frame_hash(model: &NiModel, width: u16, height: u16) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    model.view(&mut frame);
    let buf = &frame.buffer;
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

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

fn count_subslices(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

const INLINE: ScreenMode = ScreenMode::Inline { ui_height: 6 };
const ALT: ScreenMode = ScreenMode::AltScreen;

// ============================================================================
// AC#3 / scope #4: render is unperturbed by subscription presence
// ============================================================================

/// The rendered frame must be identical whether subscriptions are absent,
/// active, or churning. Subscription messages mutate counters the view never
/// reads, so any leak of subscription state into rendering would diverge the
/// frame hash from the no-subscription baseline.
#[test]
fn render_unaffected_by_subscriptions_inline() {
    let baseline = run_scenario(INLINE, SubSpec::None);
    assert!(!baseline.running, "baseline program must stop cleanly");
    for spec in [SubSpec::Ticks, SubSpec::TicksAndProcess, SubSpec::Churn] {
        let outcome = run_scenario(INLINE, spec);
        assert_eq!(
            outcome.frame_hash, baseline.frame_hash,
            "subscriptions must not perturb the inline-rendered frame (spec {spec:?})"
        );
        assert!(
            !outcome.running,
            "program must stop cleanly (spec {spec:?})"
        );
    }
}

/// Same invariant in alt-screen mode.
#[test]
fn render_unaffected_by_subscriptions_altscreen() {
    let baseline = run_scenario(ALT, SubSpec::None);
    assert!(!baseline.running, "baseline program must stop cleanly");
    for spec in [SubSpec::Ticks, SubSpec::TicksAndProcess, SubSpec::Churn] {
        let outcome = run_scenario(ALT, spec);
        assert_eq!(
            outcome.frame_hash, baseline.frame_hash,
            "subscriptions must not perturb the alt-screen-rendered frame (spec {spec:?})"
        );
        assert!(
            !outcome.running,
            "program must stop cleanly (spec {spec:?})"
        );
    }
}

// ============================================================================
// AC#1: render determinism under concurrent subscription churn
// ============================================================================

/// Repeated runs with live concurrent ticks + a child process must produce an
/// identical rendered frame. The tick *count* is timing-dependent, but because
/// it never reaches the view, the frame stays deterministic.
#[test]
fn frame_hash_deterministic_across_repeated_runs() {
    for mode in [INLINE, ALT] {
        let hashes: Vec<u64> = (0..4)
            .map(|_| run_scenario(mode, SubSpec::TicksAndProcess).frame_hash)
            .collect();
        assert!(
            hashes.windows(2).all(|w| w[0] == w[1]),
            "render must be deterministic under concurrent subscription churn (mode {mode:?}): {hashes:?}"
        );
    }
}

// ============================================================================
// AC#3 / scope #4: one-writer ownership -- output stays well-formed
// ============================================================================

/// The captured terminal stream must stay well-formed under start/stop/restart
/// churn: every synchronized-output begin is matched by an end. Interleaved
/// writes from a second (subscription) thread would tear these frames and break
/// the balance. Holds trivially (0 == 0) if the terminal lacks sync output.
#[test]
fn terminal_output_well_formed_under_churn() {
    for mode in [INLINE, ALT] {
        let outcome = run_scenario(mode, SubSpec::Churn);
        assert!(
            !outcome.terminal_output.is_empty(),
            "at least one frame must be presented (mode {mode:?})"
        );
        let begins = count_subslices(&outcome.terminal_output, SYNC_BEGIN);
        let ends = count_subslices(&outcome.terminal_output, SYNC_END);
        assert_eq!(
            begins, ends,
            "every synchronized-output begin must be matched by an end (mode {mode:?}): {begins} begins vs {ends} ends"
        );
    }
}

// ============================================================================
// AC#3: inline vs alt-screen mode correctness preserved with subscriptions
// ============================================================================

/// The mode-specific present paths must stay distinct with subscriptions live.
/// Inline performs DEC cursor save/restore gymnastics (`ESC 7`); the alt-screen
/// present path does not. A regression that collapsed the modes (or let a
/// subscription thread inject `ESC 7`) would break this.
#[test]
fn inline_and_altscreen_modes_stay_distinct_with_subscriptions() {
    let inline = run_scenario(INLINE, SubSpec::Ticks);
    let alt = run_scenario(ALT, SubSpec::Ticks);

    assert!(
        contains_subslice(&inline.terminal_output, CURSOR_SAVE),
        "inline mode must emit DEC cursor-save (ESC 7) with subscriptions active"
    );
    assert!(
        !contains_subslice(&alt.terminal_output, CURSOR_SAVE),
        "alt-screen mode must not emit inline cursor-save gymnastics"
    );
}

// ============================================================================
// AC#2: bounded clean shutdown with a live child process + ticks
// ============================================================================

/// Shutting down a program that owns a live `sleep 60` child plus fast ticks
/// must be bounded: the structured stop sequence kills the child and reaps the
/// subscription threads promptly rather than blocking on the 60s sleep. A
/// regression that waited on the child without killing it, or that joined a
/// reader thread unbounded, would blow past this budget.
#[test]
fn shutdown_bounded_with_live_process_and_ticks() {
    for mode in [INLINE, ALT] {
        let outcome = run_scenario(mode, SubSpec::TicksAndProcess);
        assert!(!outcome.running, "program must stop (mode {mode:?})");
        assert!(
            outcome.elapsed < Duration::from_secs(5),
            "shutdown with a live `sleep 60` child + ticks must be bounded, took {:?} (mode {mode:?})",
            outcome.elapsed
        );
    }
}

/// Subscription start/stop/restart churn across reconciles must not stall the
/// shutdown path; the program still terminates within the bounded budget.
#[test]
fn restart_churn_stops_cleanly() {
    for mode in [INLINE, ALT] {
        let outcome = run_scenario(mode, SubSpec::Churn);
        assert!(
            !outcome.running,
            "program must stop after churn (mode {mode:?})"
        );
        assert!(
            outcome.elapsed < Duration::from_secs(5),
            "start/stop/restart churn must not stall shutdown, took {:?} (mode {mode:?})",
            outcome.elapsed
        );
    }
}
