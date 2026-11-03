//! Slow-frame liveness (frankentui#104).
//!
//! Every frame in these runs is over its frame budget: the view sleeps longer
//! than the whole budget. The runtime must still keep the screen up to date.
//! Before the fix, a frame that ran over budget was rendered and then not
//! presented, so a steadily slow view left the terminal on its last presented
//! frame (here, nothing at all) while the model kept updating.
//!
//! The captured byte stream is replayed through an independent VT emulator
//! (`vt100`) one flush at a time, and the `steps=NN` states that actually
//! reached the screen are compared with the model's own count. The test uses
//! only API that predates the fix, and it fails on the commit before the fix.

#![forbid(unsafe_code)]

use ftui_core::event::{Event, KeyCode, KeyEvent};
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_render::budget::FrameBudgetConfig;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::program::{Cmd, Model, Program, ProgramConfig};
use ftui_runtime::terminal_writer::TerminalWriter;
use ftui_runtime::{
    BackendEventSource, BackendFeatures, LoadGovernorConfig, ScreenMode, TerminalPresenter,
};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WIDTH: u16 = 32;
const HEIGHT: u16 = 4;
/// Scripted steps per run.
const STEPS: u32 = 24;
/// The whole frame budget.
const BUDGET: Duration = Duration::from_millis(1);
/// What one view call costs: always more than the whole budget.
const VIEW_COST: Duration = Duration::from_millis(4);

/// Byte capture that records where each flush ended, so the screen can be
/// replayed frame by frame.
#[derive(Clone, Default)]
struct FlushCapture {
    inner: Arc<Mutex<Capture>>,
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    flush_ends: Vec<usize>,
}

impl Write for FlushCapture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut capture = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("capture poisoned"))?;
        capture.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut capture = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("capture poisoned"))?;
        let end = capture.bytes.len();
        if capture.flush_ends.last() != Some(&end) {
            capture.flush_ends.push(end);
        }
        Ok(())
    }
}

/// One scripted `n` per main-loop iteration, then `q`. The zero-timeout poll
/// that follows each read reports nothing, which ends the drain burst, so the
/// loop renders between steps instead of draining every step at once.
struct PacedSource {
    features: BackendFeatures,
    steps_left: u32,
    quit_sent: bool,
    just_read: bool,
}

impl BackendEventSource for PacedSource {
    type Error = io::Error;

    fn size(&self) -> Result<(u16, u16), io::Error> {
        Ok((WIDTH, HEIGHT))
    }

    fn set_features(&mut self, features: BackendFeatures) -> Result<(), io::Error> {
        self.features = features;
        Ok(())
    }

    fn poll_event(&mut self, _timeout: Duration) -> Result<bool, io::Error> {
        if self.just_read {
            self.just_read = false;
            return Ok(false);
        }
        Ok(self.steps_left > 0 || !self.quit_sent)
    }

    fn read_event(&mut self) -> Result<Option<Event>, io::Error> {
        if self.steps_left > 0 {
            self.steps_left -= 1;
            self.just_read = true;
            return Ok(Some(Event::Key(KeyEvent::new(KeyCode::Char('n')))));
        }
        if !self.quit_sent {
            self.quit_sent = true;
            self.just_read = true;
            return Ok(Some(Event::Key(KeyEvent::new(KeyCode::Char('q')))));
        }
        Ok(None)
    }
}

#[derive(Debug, Clone)]
enum Msg {
    Step,
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

/// A counter whose view is always slower than the frame budget.
struct SlowCounter {
    steps: u32,
}

impl Model for SlowCounter {
    type Message = Msg;

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            Msg::Step => {
                self.steps += 1;
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        std::thread::sleep(VIEW_COST);
        // Fixed width, so every frame overwrites the whole label.
        let text = format!("steps={:02}", self.steps);
        for (idx, ch) in text.chars().enumerate() {
            if (idx as u16) < frame.width() {
                frame.buffer.set_raw(idx as u16, 0, Cell::from_char(ch));
            }
        }
    }
}

fn nonzero(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n).expect("vt100 screen size must be nonzero")
}

struct Run {
    /// The distinct `steps=NN` values row 0 showed after each flush, in order.
    shown: Vec<u32>,
    /// The model's own final count.
    model_steps: u32,
}

fn run_slow(budget: FrameBudgetConfig, governor: bool) -> Run {
    let mut config = ProgramConfig::default()
        .with_forced_size(WIDTH, HEIGHT)
        .with_budget(budget);
    config.screen_mode = ScreenMode::AltScreen;
    config.poll_timeout = Duration::ZERO;
    config.intercept_signals = false;
    config = config.with_load_governor(if governor {
        LoadGovernorConfig::enabled()
    } else {
        LoadGovernorConfig::disabled()
    });

    let features = BackendFeatures {
        mouse_capture: config.resolved_mouse_capture(),
        bracketed_paste: config.bracketed_paste,
        focus_events: config.focus_reporting,
        kitty_keyboard: config.kitty_keyboard,
    };
    let sink = FlushCapture::default();
    let writer = TerminalWriter::with_diff_config(
        sink.clone(),
        config.screen_mode,
        config.ui_anchor,
        TerminalCapabilities::basic(),
        config.diff_config.clone(),
    );
    let events = PacedSource {
        features,
        steps_left: STEPS,
        quit_sent: false,
        just_read: false,
    };
    let mut program = Program::with_event_source(
        SlowCounter { steps: 0 },
        events,
        features,
        TerminalPresenter::new(writer),
        config,
    )
    .expect("headless program for slow-frame liveness");
    program.run().expect("run slow-frame scenario");
    let model_steps = program.model().steps;
    drop(program);

    let capture = sink.inner.lock().expect("capture lock");
    let mut parser = vt100::Parser::new(nonzero(HEIGHT), nonzero(WIDTH), 0);
    let mut shown = Vec::new();
    let mut start = 0;
    for &end in &capture.flush_ends {
        parser.process(&capture.bytes[start..end]);
        start = end;
        let row0 = parser.screen().rows(0, WIDTH).next().unwrap_or_default();
        if let Some(n) = row0
            .trim_end()
            .strip_prefix("steps=")
            .and_then(|digits| digits.parse::<u32>().ok())
            && shown.last() != Some(&n)
        {
            shown.push(n);
        }
    }
    Run { shown, model_steps }
}

fn assert_increasing(shown: &[u32], context: &str) {
    assert!(
        shown.windows(2).all(|pair| pair[0] < pair[1]),
        "{context}: the screen went backwards: {shown:?}"
    );
}

/// The planted check for #104 with the default frame-budget policy (frame
/// skips allowed), with and without the load governor. Skips at SkipFrame are
/// allowed, but the screen must keep moving: it must reach the second half of
/// the run, and a run of slow frames must never leave it unchanged for more
/// than a few steps.
#[test]
fn steadily_slow_frames_keep_reaching_the_screen() {
    for governor in [true, false] {
        let context = format!("default budget policy, governor={governor}");
        let run = run_slow(FrameBudgetConfig::with_total(BUDGET), governor);
        assert_eq!(
            run.model_steps, STEPS,
            "{context}: every scripted step processed"
        );
        assert!(
            run.shown.len() >= 3,
            "{context}: slow frames froze the screen; states shown: {:?}",
            run.shown
        );
        assert_increasing(&run.shown, &context);
        let last = *run.shown.last().expect("non-empty");
        assert!(
            last >= STEPS / 2,
            "{context}: the screen stopped at steps={last} of {STEPS}: {:?}",
            run.shown
        );
    }
}

/// `FrameBudgetConfig::strict` never skips a frame, so with steadily slow
/// frames every rendered state is presented and the run ends on the model's
/// final state.
#[test]
fn strict_budget_presents_every_slow_frame() {
    for governor in [true, false] {
        let context = format!("strict budget, governor={governor}");
        let run = run_slow(FrameBudgetConfig::strict(BUDGET), governor);
        assert_eq!(
            run.model_steps, STEPS,
            "{context}: every scripted step processed"
        );
        assert_increasing(&run.shown, &context);
        assert_eq!(
            run.shown.last().copied(),
            Some(STEPS),
            "{context}: the final state must reach the screen: {:?}",
            run.shown
        );
        assert!(
            run.shown.len() as u32 >= STEPS / 2,
            "{context}: too few states reached the screen: {:?}",
            run.shown
        );
    }
}
