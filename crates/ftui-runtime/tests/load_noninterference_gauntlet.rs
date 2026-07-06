//! Load / cancellation / shutdown / terminal-safety non-interference gauntlet
//! (bd-lu69j).
//!
//! The bd-td5el load governor gave the runtime a conservative control plane:
//! classified pressure modes (`RuntimeLoadMode`), hysteresis-guarded recovery,
//! and replayable work-disposition evidence. This gauntlet is the standing
//! safety net that proves the control plane — and any future runtime
//! performance work — cannot regress user-facing semantics at the most
//! failure-prone lifecycle edges:
//!
//! * **Load spikes** — deterministic degradation storms (a frame budget the
//!   render loop can never meet) and effect-queue floods with backpressure
//!   drops must not perturb the model-visible outcome.
//! * **Cancellation races** — quitting while background tasks are in flight
//!   must stay bounded and leave the terminal stream well formed.
//! * **Shutdown draining** — the effect queue and subscriptions must drain or
//!   drop within a bounded budget, never wedging the exit path.
//! * **Terminal safety** — synchronized-output framing stays balanced and the
//!   inline vs alt-screen present paths stay distinct while the governor is
//!   actively degrading, coalescing, or dropping best-effort work.
//!
//! # What must remain invariant vs. what may differ
//!
//! Under ANY governor mode (including adversarially mis-configured policies),
//! the following are strict and asserted by this suite:
//!
//! 1. every scripted input event is processed exactly once (the final model
//!    state is a pure function of the scripted event sequence);
//! 2. the program stops cleanly and within a bounded wall-clock budget;
//! 3. the captured terminal stream is well formed (every DEC 2026
//!    synchronized-output begin has a matching end);
//! 4. the mode-specific present paths stay distinct (inline cursor-save
//!    gymnastics never leak into alt-screen output);
//! 5. governor evidence stays replayable: stable vocabulary, internally
//!    consistent flags, and a gap-free mode chain across frames.
//!
//! What MAY legitimately differ under pressure: how many frames were
//! presented, which degradation tier presented them, how many best-effort
//! background tasks were dropped by backpressure, and how many bytes the
//! presenter emitted. The gauntlet records those as per-scenario metrics
//! (JSONL on stdout, harvested by
//! `scripts/runtime_load_noninterference_gauntlet_e2e.sh`) instead of
//! asserting them.
//!
//! # Challenge & negative-control cases (AC #4)
//!
//! The suite includes adversarial governor configurations (NaN / infinite /
//! inverted policy watermarks, zero-watermark "always overloaded" policies,
//! unmeetable frame budgets with and without frame skipping) plus negative
//! controls that prove the measurement instruments themselves can detect
//! violations — a torn sync stream, a view that leaks load-dependent state,
//! and an out-of-vocabulary evidence line are each shown to be caught.

#![forbid(unsafe_code)]

use ftui_core::event::{Event, KeyCode, KeyEvent};
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::budget::FrameBudgetConfig;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::program::{Cmd, LoadGovernorPolicy, Model, Program, ProgramConfig};
use ftui_runtime::subscription::{Every, Subscription};
use ftui_runtime::terminal_writer::TerminalWriter;
use ftui_runtime::{
    BackendEventSource, BackendFeatures, EffectQueueConfig, EvidenceSinkConfig, LoadGovernorConfig,
    ScreenMode,
};
use proptest::prelude::*;
use std::io::{self, Write};
use std::path::Path;
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
/// DEC cursor restore (`ESC 8`). Must pair with every inline cursor save.
const CURSOR_RESTORE: &[u8] = b"\x1b8";

// ============================================================================
// One-writer capture sink
// ============================================================================

/// A `Write` sink shared with the test so we can inspect everything the
/// (single) `TerminalWriter` emitted. Background effect/subscription threads
/// never receive a handle to this — that is the structural basis of the
/// one-writer rule the gauntlet re-verifies under load.
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
// Scripted event source: a dense burst of N step events, then quit, then EOF
// ============================================================================

struct BurstSource {
    width: u16,
    height: u16,
    features: BackendFeatures,
    steps_remaining: usize,
    quit_sent: bool,
}

impl BurstSource {
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

impl BackendEventSource for BurstSource {
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
// Load specifications: what background pressure runs beside scripted input
// ============================================================================

/// Number of scripted step events per scenario. Every scenario must end with
/// the model having processed exactly this many steps — that is the strict
/// input-preservation invariant.
const BURST: usize = 64;
/// Short best-effort tasks spawned per step in flood scenarios.
const FLOOD_TASKS_PER_STEP: usize = 4;
/// Long-running tasks spawned by the cancellation-race scenario.
const CANCEL_RACE_TASKS: usize = 12;
/// Sleep for each cancellation-race task; long enough that tasks are still in
/// flight when the quit event lands, short enough to keep the suite fast.
const CANCEL_RACE_TASK_MS: u64 = 25;
/// Wall-clock ceiling for every scenario: shutdown must stay bounded even
/// with live tasks, ticking subscriptions, and an always-degraded governor.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadSpec {
    /// Scripted events only: the interference-free baseline.
    EventBurst,
    /// Each step spawns short best-effort tasks; the bounded effect queue
    /// drops the overflow, exercising the backpressure/drop path.
    EffectFlood,
    /// Effect flood plus two fast `Every` tick subscriptions.
    FloodWithTicks,
    /// One step spawns long tasks, then quit lands while they are in flight.
    CancelRace,
}

impl LoadSpec {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EventBurst => "event_burst",
            Self::EffectFlood => "effect_flood",
            Self::FloodWithTicks => "flood_with_ticks",
            Self::CancelRace => "cancel_race",
        }
    }

    const fn scripted_steps(self) -> usize {
        match self {
            Self::CancelRace => 1,
            _ => BURST,
        }
    }
}

// ============================================================================
// Governor variants: healthy defaults plus adversarial challenge configs
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GovVariant {
    /// Conservative default governor (enabled).
    Default,
    /// Governor disabled: legacy threshold path. Doubles as the baseline that
    /// proves enabling the governor changes nothing model-visible.
    Disabled,
    /// Unmeetable frame budget with frame-skip allowed: the render loop skips
    /// presentation aggressively while events keep draining.
    SkipStorm,
    /// Unmeetable frame budget with frame-skip forbidden: the degradation
    /// cascade rides at maximum pressure but every frame still presents.
    DegradationStorm,
    /// Adversarial policy: NaN / infinite / inverted watermarks and a zero
    /// recovery window. Normalization must make this safe, not crash.
    PathologicalPolicy,
    /// Zero watermarks + a tiny queue cap: the governor classifies hard
    /// overload on effectively every frame (always-degraded challenge).
    ZeroWatermarks,
}

impl GovVariant {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Disabled => "disabled",
            Self::SkipStorm => "skip_storm",
            Self::DegradationStorm => "degradation_storm",
            Self::PathologicalPolicy => "pathological_policy",
            Self::ZeroWatermarks => "zero_watermarks",
        }
    }

    fn apply(self, mut config: ProgramConfig) -> ProgramConfig {
        match self {
            Self::Default => config.with_load_governor(LoadGovernorConfig::enabled()),
            Self::Disabled => config.with_load_governor(LoadGovernorConfig::disabled()),
            Self::SkipStorm => {
                let mut budget = FrameBudgetConfig::with_total(Duration::from_nanos(1));
                budget.allow_frame_skip = true;
                config = config.with_budget(budget);
                config.with_load_governor(LoadGovernorConfig::enabled())
            }
            Self::DegradationStorm => {
                let mut budget = FrameBudgetConfig::with_total(Duration::from_nanos(1));
                budget.allow_frame_skip = false;
                config = config.with_budget(budget);
                config.with_load_governor(LoadGovernorConfig::enabled())
            }
            Self::PathologicalPolicy => config.with_load_governor(
                LoadGovernorConfig::enabled().with_policy(LoadGovernorPolicy {
                    stressed_queue_watermark: f64::NAN,
                    degraded_queue_watermark: f64::NEG_INFINITY,
                    recovery_queue_watermark: 42.0,
                    recovery_intervals: 0,
                    budget_overrun_soft_ratio: -1.0,
                }),
            ),
            Self::ZeroWatermarks => config.with_load_governor(
                LoadGovernorConfig::enabled().with_policy(LoadGovernorPolicy {
                    stressed_queue_watermark: 0.0,
                    degraded_queue_watermark: 0.0,
                    recovery_queue_watermark: 0.0,
                    recovery_intervals: 1,
                    budget_overrun_soft_ratio: f64::MIN_POSITIVE,
                }),
            ),
        }
    }
}

// ============================================================================
// Test model: render reflects scripted input ONLY, never load pressure
// ============================================================================

#[derive(Debug, Clone)]
enum Msg {
    /// Driven only by scripted `n` events. The view renders this count.
    Step,
    /// Injected by `Every` subscriptions. Mutates a counter the view ignores.
    Tick,
    /// Completion of a background task. Mutates a counter the view ignores.
    TaskDone,
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

struct GauntletModel {
    spec: LoadSpec,
    /// Advanced only by `Msg::Step` (scripted input). The view renders this.
    steps: u32,
    /// Advanced only by background load. The view NEVER reads these, so any
    /// load pressure that perturbed rendering would show up as a frame-hash
    /// divergence from the interference-free baseline.
    tick_hits: usize,
    task_hits: usize,
    /// Negative-control switch: when set, the view leaks load-dependent
    /// state, which MUST be detected by the frame-hash instrument.
    leak_load_state: bool,
}

impl GauntletModel {
    fn new(spec: LoadSpec) -> Self {
        Self {
            spec,
            steps: 0,
            tick_hits: 0,
            task_hits: 0,
            leak_load_state: false,
        }
    }

    fn flood_tasks(&self) -> Cmd<Msg> {
        let tasks = (0..FLOOD_TASKS_PER_STEP)
            .map(|_| {
                Cmd::task(|| {
                    std::thread::sleep(Duration::from_millis(1));
                    Msg::TaskDone
                })
            })
            .collect();
        Cmd::batch(tasks)
    }

    fn cancel_race_tasks(&self) -> Cmd<Msg> {
        let tasks = (0..CANCEL_RACE_TASKS)
            .map(|_| {
                Cmd::task(|| {
                    std::thread::sleep(Duration::from_millis(CANCEL_RACE_TASK_MS));
                    Msg::TaskDone
                })
            })
            .collect();
        Cmd::batch(tasks)
    }
}

impl Model for GauntletModel {
    type Message = Msg;

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            Msg::Step => {
                self.steps += 1;
                match self.spec {
                    LoadSpec::EventBurst => Cmd::none(),
                    LoadSpec::EffectFlood | LoadSpec::FloodWithTicks => self.flood_tasks(),
                    LoadSpec::CancelRace => self.cancel_race_tasks(),
                }
            }
            Msg::Tick => {
                self.tick_hits += 1;
                Cmd::none()
            }
            Msg::TaskDone => {
                self.task_hits += 1;
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        // Render reflects scripted input only. Load pressure (tick_hits,
        // task_hits, drops, degradation tier) is deliberately absent so the
        // frame hash is a function of the deterministic step count alone.
        // The `leak_load_state` branch exists purely as a negative control.
        let text = if self.leak_load_state {
            format!("steps={} ticks={}", self.steps, self.tick_hits)
        } else {
            format!("steps={}", self.steps)
        };
        for (idx, ch) in text.chars().enumerate() {
            if (idx as u16) < frame.width() {
                frame.buffer.set_raw(idx as u16, 0, Cell::from_char(ch));
            }
        }
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        match self.spec {
            LoadSpec::FloodWithTicks => vec![
                Box::new(Every::with_id(0xA1, Duration::from_millis(3), || Msg::Tick)),
                Box::new(Every::with_id(0xB2, Duration::from_millis(5), || Msg::Tick)),
            ],
            _ => Vec::new(),
        }
    }
}

// ============================================================================
// Headless run harness + per-scenario metrics
// ============================================================================

struct Outcome {
    terminal_output: Vec<u8>,
    frame_hash: u64,
    steps: u32,
    running: bool,
    elapsed: Duration,
}

const WIDTH: u16 = 40;
const HEIGHT: u16 = 12;
const INLINE: ScreenMode = ScreenMode::Inline { ui_height: 6 };
const ALT: ScreenMode = ScreenMode::AltScreen;

fn mode_label(mode: ScreenMode) -> &'static str {
    match mode {
        ScreenMode::Inline { .. } | ScreenMode::InlineAuto { .. } => "inline",
        ScreenMode::AltScreen => "alt",
    }
}

fn run_gauntlet(
    screen_mode: ScreenMode,
    spec: LoadSpec,
    variant: GovVariant,
    evidence_path: Option<&Path>,
) -> Outcome {
    let mut config = ProgramConfig::default().with_forced_size(WIDTH, HEIGHT);
    config.screen_mode = screen_mode;
    config.poll_timeout = Duration::ZERO;
    config.intercept_signals = false;
    // A bounded effect queue is part of the gauntlet: floods must overflow it
    // so the backpressure/drop path runs under the governor's eye.
    config = config.with_effect_queue(
        EffectQueueConfig::default()
            .with_enabled(true)
            .with_max_queue_depth(match spec {
                LoadSpec::CancelRace => CANCEL_RACE_TASKS * 2,
                _ => 8,
            }),
    );
    config = variant.apply(config);
    if let Some(path) = evidence_path {
        config = config
            .with_evidence_sink(EvidenceSinkConfig::enabled_file(path).with_flush_on_write(true));
    }

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

    let model = GauntletModel::new(spec);
    let events = BurstSource::new(WIDTH, HEIGHT, initial_features, spec.scripted_steps());

    let start = Instant::now();
    let mut program = Program::with_event_source(model, events, initial_features, writer, config)
        .expect("headless program for load non-interference gauntlet");
    program.run().expect("run load non-interference scenario");
    let elapsed = start.elapsed();

    let frame_hash = model_frame_hash(program.model(), WIDTH, HEIGHT);
    let outcome = Outcome {
        terminal_output: sink.snapshot(),
        frame_hash,
        steps: program.model().steps,
        running: program.is_running(),
        elapsed,
    };

    // Correctness verdicts and performance metrics come from the same run
    // (bead requirement). The E2E wrapper harvests these JSONL lines into the
    // replay artifact bundle.
    println!(
        "{{\"gauntlet\":\"load_noninterference\",\"scenario\":\"{}\",\"governor\":\"{}\",\"mode\":\"{}\",\"elapsed_us\":{},\"output_bytes\":{},\"steps\":{},\"stopped\":{}}}",
        spec.as_str(),
        variant.as_str(),
        mode_label(screen_mode),
        u64::try_from(outcome.elapsed.as_micros()).unwrap_or(u64::MAX),
        outcome.terminal_output.len(),
        outcome.steps,
        !outcome.running,
    );

    outcome
}

fn model_frame_hash(model: &GauntletModel, width: u16, height: u16) -> u64 {
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

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Terminal-stream well-formedness instrument: sync framing must balance and
/// inline cursor saves must pair with restores. Returns a description of the
/// violation instead of panicking so the negative control can exercise it.
fn check_stream_well_formed(output: &[u8]) -> Result<(), String> {
    let begins = count_subslices(output, SYNC_BEGIN);
    let ends = count_subslices(output, SYNC_END);
    if begins != ends {
        return Err(format!(
            "synchronized-output framing torn: {begins} begins vs {ends} ends"
        ));
    }
    let saves = count_subslices(output, CURSOR_SAVE);
    let restores = count_subslices(output, CURSOR_RESTORE);
    if saves != restores {
        return Err(format!(
            "cursor save/restore unbalanced: {saves} saves vs {restores} restores"
        ));
    }
    Ok(())
}

fn assert_strict_semantics(outcome: &Outcome, expected_steps: u32, context: &str) {
    assert!(!outcome.running, "program must stop cleanly ({context})");
    assert_eq!(
        outcome.steps, expected_steps,
        "every scripted input must be processed exactly once ({context})"
    );
    assert!(
        outcome.elapsed < SHUTDOWN_BUDGET,
        "shutdown must stay bounded, took {:?} ({context})",
        outcome.elapsed
    );
    if let Err(violation) = check_stream_well_formed(&outcome.terminal_output) {
        panic!("terminal stream violation ({context}): {violation}");
    }
}

// ============================================================================
// Governor policy normalization: property tests (unit/property gate)
// ============================================================================

proptest! {
    /// `LoadGovernorConfig::with_policy` must map ANY policy input — NaN,
    /// infinities, negatives, inverted orderings, zero recovery windows — to
    /// a safe, totally-ordered configuration. This is the unit/property gate
    /// the bead requires before the E2E scenarios are trusted: an unsafe
    /// normalization would invalidate every challenge case below.
    #[test]
    fn policy_normalization_is_total_and_ordered(
        stressed in proptest::num::f64::ANY,
        degraded in proptest::num::f64::ANY,
        recovery in proptest::num::f64::ANY,
        intervals in proptest::num::u8::ANY,
        soft_ratio in proptest::num::f64::ANY,
    ) {
        let config = LoadGovernorConfig::enabled().with_policy(LoadGovernorPolicy {
            stressed_queue_watermark: stressed,
            degraded_queue_watermark: degraded,
            recovery_queue_watermark: recovery,
            recovery_intervals: intervals,
            budget_overrun_soft_ratio: soft_ratio,
        });
        let policy = config.policy;

        prop_assert!(policy.recovery_queue_watermark.is_finite());
        prop_assert!(policy.stressed_queue_watermark.is_finite());
        prop_assert!(policy.degraded_queue_watermark.is_finite());
        prop_assert!((0.0..=1.0).contains(&policy.recovery_queue_watermark));
        prop_assert!((0.0..=1.0).contains(&policy.stressed_queue_watermark));
        prop_assert!((0.0..=1.0).contains(&policy.degraded_queue_watermark));
        prop_assert!(
            policy.recovery_queue_watermark <= policy.stressed_queue_watermark,
            "recovery watermark must not exceed stressed watermark"
        );
        prop_assert!(
            policy.stressed_queue_watermark <= policy.degraded_queue_watermark,
            "stressed watermark must not exceed degraded watermark"
        );
        prop_assert!(policy.recovery_intervals >= 1, "hysteresis window must be non-zero");
        prop_assert!(policy.budget_overrun_soft_ratio.is_finite());
        prop_assert!(policy.budget_overrun_soft_ratio > 0.0);
    }
}

// ============================================================================
// Load spikes: governor variants must not perturb model-visible state
// ============================================================================

/// Under an identical scripted event burst, every governor variant — enabled,
/// disabled, degradation storm, skip storm, pathological policy, and
/// always-overloaded watermarks — must land on the identical final model
/// state (frame hash) with every scripted input processed. The governor may
/// change HOW frames are presented; it must never change WHAT the model saw.
#[test]
fn governor_variants_preserve_model_state_under_burst() {
    for mode in [INLINE, ALT] {
        let baseline = run_gauntlet(mode, LoadSpec::EventBurst, GovVariant::Disabled, None);
        assert_strict_semantics(&baseline, BURST as u32, "baseline burst, governor disabled");

        for variant in [
            GovVariant::Default,
            GovVariant::SkipStorm,
            GovVariant::DegradationStorm,
            GovVariant::PathologicalPolicy,
            GovVariant::ZeroWatermarks,
        ] {
            let context = format!("burst variant {variant:?} mode {mode:?}");
            let outcome = run_gauntlet(mode, LoadSpec::EventBurst, variant, None);
            assert_strict_semantics(&outcome, BURST as u32, &context);
            assert_eq!(
                outcome.frame_hash, baseline.frame_hash,
                "governor variant must not perturb the model-visible frame ({context})"
            );
        }
    }
}

/// Effect floods (with backpressure drops) and ticking subscriptions must not
/// leak into the rendered model state either — best-effort work may be
/// dropped, but the frame stays a pure function of the scripted steps.
#[test]
fn effect_flood_and_ticks_do_not_perturb_model_state() {
    for mode in [INLINE, ALT] {
        let baseline = run_gauntlet(mode, LoadSpec::EventBurst, GovVariant::Disabled, None);
        for spec in [LoadSpec::EffectFlood, LoadSpec::FloodWithTicks] {
            let context = format!("spec {spec:?} mode {mode:?}");
            let outcome = run_gauntlet(mode, spec, GovVariant::Default, None);
            assert_strict_semantics(&outcome, BURST as u32, &context);
            assert_eq!(
                outcome.frame_hash, baseline.frame_hash,
                "background load must not perturb the model-visible frame ({context})"
            );
        }
    }
}

/// Challenge case: with zero watermarks and a tiny queue cap the governor
/// classifies hard overload on effectively every frame, so the run spends its
/// life in `Degraded` with a `DeferBackgroundDropBestEffort` disposition.
/// Even then, scripted input is strict work and must survive completely.
#[test]
fn always_degraded_governor_never_drops_scripted_input() {
    for mode in [INLINE, ALT] {
        let context = format!("always-degraded flood, mode {mode:?}");
        let outcome = run_gauntlet(
            mode,
            LoadSpec::FloodWithTicks,
            GovVariant::ZeroWatermarks,
            None,
        );
        assert_strict_semantics(&outcome, BURST as u32, &context);
    }
}

// ============================================================================
// Cancellation race + shutdown draining
// ============================================================================

/// Quit lands while long-running background tasks are still in flight. The
/// shutdown path must cancel/drain within the bounded budget and leave the
/// terminal stream well formed — no torn frames, no leaked writes from the
/// effect thread racing termination.
#[test]
fn cancellation_race_shutdown_bounded_and_well_formed() {
    for mode in [INLINE, ALT] {
        for variant in [GovVariant::Default, GovVariant::DegradationStorm] {
            let context = format!("cancel race, variant {variant:?}, mode {mode:?}");
            let outcome = run_gauntlet(mode, LoadSpec::CancelRace, variant, None);
            assert_strict_semantics(&outcome, 1, &context);
        }
    }
}

// ============================================================================
// Terminal safety under active degradation
// ============================================================================

/// While the governor is maximally engaged (always-degraded classification +
/// flood + ticks), the mode-specific present paths must stay distinct: inline
/// mode performs DEC cursor save/restore gymnastics, alt-screen must never
/// emit them. A collapse of the two paths under pressure — or a background
/// thread injecting bytes — would break this.
///
/// This deliberately uses `ZeroWatermarks` (governor pinned at `Degraded`
/// with a meetable frame budget) rather than `DegradationStorm`: the gauntlet
/// pinned down that an unmeetable budget suppresses presentation entirely via
/// the post-render present-skip path — zero bytes are emitted even with
/// `allow_frame_skip = false` — so a storm run has no present path to compare.
/// That suppression semantics is itself pinned by
/// `unmeetable_budget_suppresses_presentation_but_preserves_input`.
#[test]
fn terminal_mode_safety_preserved_under_always_degraded_governor() {
    let inline = run_gauntlet(
        INLINE,
        LoadSpec::FloodWithTicks,
        GovVariant::ZeroWatermarks,
        None,
    );
    assert_strict_semantics(&inline, BURST as u32, "inline always-degraded flood");
    assert!(
        !inline.terminal_output.is_empty(),
        "an always-degraded governor with a meetable budget must still present frames"
    );
    assert!(
        contains_subslice(&inline.terminal_output, CURSOR_SAVE),
        "inline mode must keep DEC cursor-save framing under load"
    );

    let alt = run_gauntlet(
        ALT,
        LoadSpec::FloodWithTicks,
        GovVariant::ZeroWatermarks,
        None,
    );
    assert_strict_semantics(&alt, BURST as u32, "alt always-degraded flood");
    assert!(
        !contains_subslice(&alt.terminal_output, CURSOR_SAVE),
        "alt-screen mode must not emit inline cursor-save gymnastics under load"
    );
}

/// Pinned discovery: when the frame budget is unmeetable, the runtime
/// suppresses presentation entirely (the post-render present-skip fires every
/// frame, regardless of `allow_frame_skip`). The user-visible contract under
/// that extreme is exactly the strict floor this gauntlet enforces: zero
/// presented bytes is an acceptable degraded-mode outcome, but dropped or
/// reordered scripted input never is, and shutdown stays bounded.
#[test]
fn unmeetable_budget_suppresses_presentation_but_preserves_input() {
    for mode in [INLINE, ALT] {
        let outcome = run_gauntlet(
            mode,
            LoadSpec::EventBurst,
            GovVariant::DegradationStorm,
            None,
        );
        assert_strict_semantics(
            &outcome,
            BURST as u32,
            &format!("unmeetable budget, mode {mode:?}"),
        );
        assert!(
            outcome.terminal_output.is_empty(),
            "an unmeetable budget suppresses presentation via the present-skip path; \
             if this starts presenting, the degraded-mode contract changed and both \
             this pin and the operator docs must be revisited (mode {mode:?})"
        );
    }
}

// ============================================================================
// Evidence ledger: replayable, stable-vocabulary governor decisions
// ============================================================================

const MODE_VOCAB: [&str; 5] = ["healthy", "stressed", "degraded", "recovered", "unsafe"];
const PRESSURE_VOCAB: [&str; 4] = ["steady_state", "soft_overload", "hard_overload", "unsafe"];
const DISPOSITION_VOCAB: [&str; 5] = [
    "admit_all",
    "coalesce_visible_defer_background",
    "defer_background_drop_best_effort",
    "readmit_after_hysteresis",
    "fail_fast_strict_guarantee",
];
const REASON_VOCAB: [&str; 12] = [
    "governor_disabled",
    "strict_semantics_violation",
    "effect_queue_drop",
    "queue_degraded_watermark",
    "budget_degradation_active",
    "queue_stressed_watermark",
    "resize_coalescing_active",
    "frame_budget_overrun",
    "recovery_hysteresis_pending",
    "steady_state",
    "recovery_hysteresis_satisfied",
    "recovered_interval_closed",
];

/// Evidence-line instrument: every governor decision must use the stable
/// vocabulary and be internally consistent. Returns the violation instead of
/// panicking so the negative control can exercise the instrument itself.
fn check_governor_evidence_line(line: &serde_json::Value) -> Result<(), String> {
    let str_field = |key: &str| -> Result<String, String> {
        line.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("missing/invalid string field `{key}`"))
    };
    let bool_field = |key: &str| -> Result<bool, String> {
        line.get(key)
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| format!("missing/invalid bool field `{key}`"))
    };

    let mode = str_field("runtime_mode")?;
    let mode_before = str_field("runtime_mode_before")?;
    let pressure = str_field("pressure_class")?;
    let disposition = str_field("work_disposition")?;
    let reason = str_field("governor_reason")?;
    let transition = bool_field("governor_transition")?;
    let strict_ok = bool_field("strict_semantics_preserved")?;

    if !MODE_VOCAB.contains(&mode.as_str()) {
        return Err(format!("runtime_mode `{mode}` outside stable vocabulary"));
    }
    if !MODE_VOCAB.contains(&mode_before.as_str()) {
        return Err(format!(
            "runtime_mode_before `{mode_before}` outside stable vocabulary"
        ));
    }
    if !PRESSURE_VOCAB.contains(&pressure.as_str()) {
        return Err(format!(
            "pressure_class `{pressure}` outside stable vocabulary"
        ));
    }
    if !DISPOSITION_VOCAB.contains(&disposition.as_str()) {
        return Err(format!(
            "work_disposition `{disposition}` outside stable vocabulary"
        ));
    }
    if !REASON_VOCAB.contains(&reason.as_str()) {
        return Err(format!(
            "governor_reason `{reason}` outside stable vocabulary"
        ));
    }
    if transition != (mode != mode_before) {
        return Err(format!(
            "governor_transition={transition} inconsistent with {mode_before} -> {mode}"
        ));
    }
    if strict_ok != (mode != "unsafe") {
        return Err(format!(
            "strict_semantics_preserved={strict_ok} inconsistent with mode {mode}"
        ));
    }
    for key in [
        "queue_in_flight",
        "queue_dropped_delta",
        "recovery_intervals_observed",
        "recovery_intervals_required",
        "deferred_work_total",
        "coalesced_work_total",
        "dropped_work_total",
    ] {
        if line.get(key).and_then(serde_json::Value::as_u64).is_none() {
            return Err(format!("missing/invalid counter field `{key}`"));
        }
    }
    Ok(())
}

/// The governor's decisions must land in the evidence ledger as replayable
/// artifacts: stable vocabulary on every line, internally consistent
/// transition flags, and a gap-free mode chain (each frame's
/// `runtime_mode_before` equals the previous frame's `runtime_mode`). This is
/// what lets an operator reconstruct WHY the runtime degraded, deferred, or
/// dropped work after the fact — the observability half of the bead.
#[test]
fn evidence_ledger_governor_decisions_are_replayable() {
    let dir = tempfile::tempdir().expect("evidence tempdir");
    let path = dir.path().join("gauntlet_evidence.jsonl");
    let outcome = run_gauntlet(
        INLINE,
        LoadSpec::EffectFlood,
        GovVariant::Default,
        Some(&path),
    );
    assert_strict_semantics(&outcome, BURST as u32, "evidence ledger run");

    let raw = std::fs::read_to_string(&path).expect("read evidence ledger");
    let decisions: Vec<serde_json::Value> = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("evidence line parses"))
        .filter(|value| {
            value.get("event").and_then(serde_json::Value::as_str) == Some("budget_decision")
        })
        .collect();

    assert!(
        !decisions.is_empty(),
        "an enabled governor must emit budget_decision evidence lines"
    );
    for (idx, line) in decisions.iter().enumerate() {
        if let Err(violation) = check_governor_evidence_line(line) {
            panic!("evidence line {idx} invalid: {violation}\nline: {line}");
        }
    }
    for pair in decisions.windows(2) {
        let prev_mode = pair[0]
            .get("runtime_mode")
            .and_then(serde_json::Value::as_str)
            .expect("prev runtime_mode");
        let next_before = pair[1]
            .get("runtime_mode_before")
            .and_then(serde_json::Value::as_str)
            .expect("next runtime_mode_before");
        assert_eq!(
            next_before, prev_mode,
            "governor mode chain must be gap-free for replay: {} then {}",
            pair[0], pair[1]
        );
    }
}

// ============================================================================
// Negative controls: the instruments themselves must detect violations
// ============================================================================

/// A gauntlet that cannot fail is worthless. Prove each measurement
/// instrument detects the violation class it exists for.
#[test]
fn negative_control_instruments_detect_violations() {
    // (a) Torn synchronized-output framing is detected.
    let mut torn = Vec::new();
    torn.extend_from_slice(SYNC_BEGIN);
    torn.extend_from_slice(b"frame one");
    torn.extend_from_slice(SYNC_END);
    torn.extend_from_slice(SYNC_BEGIN);
    torn.extend_from_slice(b"frame two, never closed");
    assert!(
        check_stream_well_formed(&torn).is_err(),
        "torn sync framing must be detected"
    );

    // (b) Unbalanced cursor save/restore is detected.
    let mut unbalanced = Vec::new();
    unbalanced.extend_from_slice(CURSOR_SAVE);
    unbalanced.extend_from_slice(b"saved but never restored");
    assert!(
        check_stream_well_formed(&unbalanced).is_err(),
        "unbalanced cursor save/restore must be detected"
    );

    // (c) A view that leaks load-dependent state diverges the frame hash.
    let clean = GauntletModel::new(LoadSpec::EventBurst);
    let mut leaky = GauntletModel::new(LoadSpec::EventBurst);
    leaky.leak_load_state = true;
    leaky.tick_hits = 1;
    assert_ne!(
        model_frame_hash(&clean, WIDTH, HEIGHT),
        model_frame_hash(&leaky, WIDTH, HEIGHT),
        "the frame-hash instrument must detect load state leaking into the view"
    );

    // (d) Out-of-vocabulary / inconsistent evidence lines are detected.
    let bogus_mode: serde_json::Value = serde_json::json!({
        "runtime_mode": "warp_speed",
        "runtime_mode_before": "healthy",
        "pressure_class": "steady_state",
        "work_disposition": "admit_all",
        "governor_reason": "steady_state",
        "governor_transition": true,
        "strict_semantics_preserved": true,
        "queue_in_flight": 0,
        "queue_dropped_delta": 0,
        "recovery_intervals_observed": 0,
        "recovery_intervals_required": 3,
        "deferred_work_total": 0,
        "coalesced_work_total": 0,
        "dropped_work_total": 0,
    });
    assert!(
        check_governor_evidence_line(&bogus_mode).is_err(),
        "out-of-vocabulary runtime_mode must be detected"
    );

    let inconsistent_transition: serde_json::Value = serde_json::json!({
        "runtime_mode": "healthy",
        "runtime_mode_before": "healthy",
        "pressure_class": "steady_state",
        "work_disposition": "admit_all",
        "governor_reason": "steady_state",
        "governor_transition": true,
        "strict_semantics_preserved": true,
        "queue_in_flight": 0,
        "queue_dropped_delta": 0,
        "recovery_intervals_observed": 0,
        "recovery_intervals_required": 3,
        "deferred_work_total": 0,
        "coalesced_work_total": 0,
        "dropped_work_total": 0,
    });
    assert!(
        check_governor_evidence_line(&inconsistent_transition).is_err(),
        "transition flag inconsistent with the mode pair must be detected"
    );
}
