#![forbid(unsafe_code)]

//! Step-based WASM program runner for FrankenTUI.
//!
//! [`StepProgram`] drives an [`ftui_runtime::program::Model`] through
//! init / event / update / view / present cycles without threads or blocking.
//! The host (JavaScript) controls the event loop:
//!
//! 1. Push events via [`StepProgram::push_event`].
//! 2. Advance time via [`StepProgram::advance_time`].
//! 3. Call [`StepProgram::step`] to process one batch of events and render.
//! 4. Read the rendered buffer via [`StepProgram::take_outputs`].
//!
//! # Example
//!
//! ```ignore
//! use ftui_web::step_program::StepProgram;
//! use ftui_core::event::Event;
//! use core::time::Duration;
//!
//! let mut prog = StepProgram::new(MyModel::default(), 80, 24);
//! prog.init().unwrap();
//!
//! // Host-driven frame loop
//! prog.push_event(Event::Tick);
//! prog.advance_time(Duration::from_millis(16));
//! let result = prog.step().unwrap();
//!
//! if result.rendered {
//!     let outputs = prog.take_outputs();
//!     // Send outputs.last_buffer to the renderer...
//! }
//! ```

use core::time::Duration;

use ftui_backend::{BackendClock, BackendEventSource, BackendPresenter};
use ftui_core::event::Event;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::program::{Cmd, Model};

use crate::{WebBackend, WebBackendError, WebOutputs};

/// Run grapheme-pool GC every N rendered frames in host-driven WASM mode.
const POOL_GC_INTERVAL_FRAMES: u64 = 256;

/// Result of a single [`StepProgram::step`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepResult {
    /// Whether the program is still running (false after `Cmd::Quit`).
    pub running: bool,
    /// Whether a frame was rendered during this step.
    pub rendered: bool,
    /// Number of events processed during this step.
    pub events_processed: u32,
    /// Current frame index (monotonically increasing).
    pub frame_idx: u64,
}

/// Host-driven, non-blocking program runner for WASM.
///
/// Wraps a [`Model`] and a [`WebBackend`], providing a step-based execution
/// model suitable for `wasm32-unknown-unknown`. No threads, no blocking, no
/// `std::time::Instant` — all I/O and time are host-driven.
///
/// # Lifecycle
///
/// 1. [`StepProgram::new`] — create with model and initial terminal size.
/// 2. [`StepProgram::init`] — call once to initialize the model and render the first frame.
/// 3. [`StepProgram::step`] — call repeatedly from the host event loop (e.g., `requestAnimationFrame`).
/// 4. Read outputs after each step via [`StepProgram::take_outputs`].
pub struct StepProgram<M: Model> {
    model: M,
    backend: WebBackend,
    pool: GraphemePool,
    running: bool,
    initialized: bool,
    dirty: bool,
    frame_idx: u64,
    tick_rate: Option<Duration>,
    last_tick: Duration,
    width: u16,
    height: u16,
    prev_buffer: Option<Buffer>,
}

impl<M: Model> StepProgram<M> {
    /// Create a new step program with the given model and initial terminal size.
    #[must_use]
    pub fn new(model: M, width: u16, height: u16) -> Self {
        Self {
            model,
            backend: WebBackend::new(width, height),
            pool: GraphemePool::new(),
            running: true,
            initialized: false,
            dirty: true,
            frame_idx: 0,
            tick_rate: None,
            last_tick: Duration::ZERO,
            width,
            height,
            prev_buffer: None,
        }
    }

    /// Create a step program with an existing [`WebBackend`].
    #[must_use]
    pub fn with_backend(model: M, mut backend: WebBackend) -> Self {
        let (width, height) = backend.events_mut().size().unwrap_or((80, 24));
        Self {
            model,
            backend,
            pool: GraphemePool::new(),
            running: true,
            initialized: false,
            dirty: true,
            frame_idx: 0,
            tick_rate: None,
            last_tick: Duration::ZERO,
            width,
            height,
            prev_buffer: None,
        }
    }

    /// Initialize the model and render the first frame.
    ///
    /// Must be called exactly once before [`step`](Self::step).
    /// Calls `Model::init()`, executes returned commands, and presents
    /// the initial frame.
    pub fn init(&mut self) -> Result<(), WebBackendError> {
        assert!(!self.initialized, "StepProgram::init() called twice");
        self.initialized = true;
        let cmd = self.model.init();
        self.execute_cmd(cmd);
        if self.running {
            self.render_frame()?;
        }
        Ok(())
    }

    /// Process one batch of pending events, handle ticks, and render if dirty.
    ///
    /// This is the main entry point for the host event loop. Call this after
    /// pushing events and advancing time.
    ///
    /// Returns [`StepResult`] describing what happened during the step.
    pub fn step(&mut self) -> Result<StepResult, WebBackendError> {
        assert!(self.initialized, "StepProgram::step() called before init()");

        if !self.running {
            return Ok(StepResult {
                running: false,
                rendered: false,
                events_processed: 0,
                frame_idx: self.frame_idx,
            });
        }

        // 1. Process all pending events.
        let mut events_processed: u32 = 0;
        while let Some(event) = self.backend.events.read_event()? {
            events_processed += 1;
            self.handle_event(event);
            if !self.running {
                break;
            }
        }

        // 2. Handle tick if tick_rate is set and enough time has elapsed.
        if self.running
            && let Some(rate) = self.tick_rate
        {
            let now = self.backend.clock.now_mono();
            if now.saturating_sub(self.last_tick) >= rate {
                self.last_tick = now;
                let msg = M::Message::from(Event::Tick);
                let cmd = self.model.update(msg);
                self.dirty = true;
                self.execute_cmd(cmd);
            }
        }

        // 3. Render if dirty.
        let rendered = if self.running && self.dirty {
            self.render_frame()?;
            true
        } else {
            false
        };

        Ok(StepResult {
            running: self.running,
            rendered,
            events_processed,
            frame_idx: self.frame_idx,
        })
    }

    /// Push a terminal event into the event queue.
    ///
    /// Events are processed on the next [`step`](Self::step) call.
    pub fn push_event(&mut self, event: Event) {
        // Handle resize events immediately to update internal size tracking.
        if let Event::Resize { width, height } = &event {
            self.width = *width;
            self.height = *height;
            self.backend.events_mut().set_size(*width, *height);
        }
        self.backend.events_mut().push_event(event);
    }

    /// Advance the deterministic clock by `dt`.
    pub fn advance_time(&mut self, dt: Duration) {
        self.backend.clock_mut().advance(dt);
    }

    /// Set the deterministic clock to an absolute time.
    pub fn set_time(&mut self, now: Duration) {
        self.backend.clock_mut().set(now);
    }

    /// Resize the terminal.
    ///
    /// Pushes a `Resize` event and updates the backend size. The resize
    /// is processed on the next [`step`](Self::step) call.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.push_event(Event::Resize { width, height });
    }

    /// Take the captured outputs (rendered buffer, logs), leaving empty defaults.
    pub fn take_outputs(&mut self) -> WebOutputs {
        self.backend.presenter_mut().take_outputs()
    }

    /// Read the captured outputs without consuming them.
    pub fn outputs(&self) -> &WebOutputs {
        self.backend.presenter.outputs()
    }

    /// Access the model.
    pub fn model(&self) -> &M {
        &self.model
    }

    /// Mutably access the model.
    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// Access the backend.
    pub fn backend(&self) -> &WebBackend {
        &self.backend
    }

    /// Mutably access the backend.
    pub fn backend_mut(&mut self) -> &mut WebBackend {
        &mut self.backend
    }

    /// Whether the program is still running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether the program has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Current frame index.
    pub fn frame_idx(&self) -> u64 {
        self.frame_idx
    }

    /// Current terminal dimensions.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Current tick rate, if any.
    pub fn tick_rate(&self) -> Option<Duration> {
        self.tick_rate
    }

    /// Access the grapheme pool (needed for deterministic checksumming).
    pub fn pool(&self) -> &GraphemePool {
        &self.pool
    }

    // --- Private helpers ---

    fn handle_event(&mut self, event: Event) {
        if let Event::Resize { width, height } = &event {
            self.width = *width;
            self.height = *height;
            // Invalidate diff baseline — sizes may differ.
            self.prev_buffer = None;
        }
        let msg = M::Message::from(event);
        let cmd = self.model.update(msg);
        self.dirty = true;
        self.execute_cmd(cmd);
    }

    fn render_frame(&mut self) -> Result<(), WebBackendError> {
        let mut frame = Frame::new(self.width, self.height, &mut self.pool);
        self.model.view(&mut frame);

        let buf = frame.buffer;
        let diff = self
            .prev_buffer
            .as_ref()
            .map(|prev| BufferDiff::compute(prev, &buf));
        let full_repaint = self.prev_buffer.is_none();

        // Clone buf into prev_buffer for next frame's diff, then move the
        // original into the presenter's owned path (avoids a second clone).
        self.prev_buffer = Some(buf.clone());
        self.backend
            .presenter_mut()
            .present_ui_owned(buf, diff.as_ref(), full_repaint);

        self.dirty = false;
        self.frame_idx += 1;
        if self.frame_idx.is_multiple_of(POOL_GC_INTERVAL_FRAMES) {
            let buffers: Vec<&Buffer> = self.prev_buffer.iter().collect();
            self.pool.gc(&buffers);
        }
        Ok(())
    }

    fn execute_cmd(&mut self, cmd: Cmd<M::Message>) {
        match cmd {
            Cmd::None => {}
            Cmd::Quit => {
                self.running = false;
            }
            Cmd::Msg(m) => {
                let cmd = self.model.update(m);
                self.execute_cmd(cmd);
            }
            Cmd::Batch(cmds) => {
                for c in cmds {
                    self.execute_cmd(c);
                    if !self.running {
                        break;
                    }
                }
            }
            Cmd::Sequence(cmds) => {
                for c in cmds {
                    self.execute_cmd(c);
                    if !self.running {
                        break;
                    }
                }
            }
            Cmd::Tick(duration) => {
                self.tick_rate = Some(duration);
            }
            Cmd::Log(text) => {
                let _ = self.backend.presenter_mut().write_log(&text);
            }
            Cmd::Task(_spec, f) => {
                // WASM has no threads — execute tasks synchronously.
                let msg = f();
                let cmd = self.model.update(msg);
                self.execute_cmd(cmd);
            }
            Cmd::SetMouseCapture(enabled) => {
                let mut features = self.backend.events_mut().features();
                features.mouse_capture = enabled;
                let _ = self.backend.events_mut().set_features(features);
            }
            Cmd::SaveState | Cmd::RestoreState => {
                // No persistence in WASM (yet).
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_render::cell::Cell;
    use ftui_render::drawing::Draw;
    use pretty_assertions::assert_eq;

    // ---- Test model ----

    struct Counter {
        value: i32,
        initialized: bool,
    }

    #[derive(Debug)]
    enum CounterMsg {
        Increment,
        Decrement,
        Reset,
        Quit,
        LogValue,
        BatchIncrement(usize),
        SpawnTask,
    }

    impl From<Event> for CounterMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Key(k) if k.code == KeyCode::Char('+') => CounterMsg::Increment,
                Event::Key(k) if k.code == KeyCode::Char('-') => CounterMsg::Decrement,
                Event::Key(k) if k.code == KeyCode::Char('r') => CounterMsg::Reset,
                Event::Key(k) if k.code == KeyCode::Char('q') => CounterMsg::Quit,
                Event::Tick => CounterMsg::Increment,
                _ => CounterMsg::Increment,
            }
        }
    }

    impl Model for Counter {
        type Message = CounterMsg;

        fn init(&mut self) -> Cmd<Self::Message> {
            self.initialized = true;
            Cmd::none()
        }

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                CounterMsg::Increment => {
                    self.value += 1;
                    Cmd::none()
                }
                CounterMsg::Decrement => {
                    self.value -= 1;
                    Cmd::none()
                }
                CounterMsg::Reset => {
                    self.value = 0;
                    Cmd::none()
                }
                CounterMsg::Quit => Cmd::quit(),
                CounterMsg::LogValue => Cmd::log(format!("value={}", self.value)),
                CounterMsg::BatchIncrement(n) => {
                    let cmds: Vec<_> = (0..n).map(|_| Cmd::msg(CounterMsg::Increment)).collect();
                    Cmd::batch(cmds)
                }
                CounterMsg::SpawnTask => Cmd::task(|| CounterMsg::Increment),
            }
        }

        fn view(&self, frame: &mut Frame) {
            let text = format!("Count: {}", self.value);
            for (i, c) in text.chars().enumerate() {
                if (i as u16) < frame.width() {
                    frame.buffer.set_raw(i as u16, 0, Cell::from_char(c));
                }
            }
        }
    }

    /// Test model that emits a new combining-mark grapheme each frame.
    ///
    /// Used to verify periodic grapheme-pool GC in `StepProgram`.
    struct GraphemeChurn {
        value: u32,
    }

    impl Model for GraphemeChurn {
        type Message = CounterMsg;

        fn init(&mut self) -> Cmd<Self::Message> {
            Cmd::none()
        }

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            if let CounterMsg::Increment = msg {
                self.value = self.value.wrapping_add(1);
            }
            Cmd::none()
        }

        fn view(&self, frame: &mut Frame) {
            let base = char::from_u32(0x4e00 + (self.value % 2048)).unwrap_or('字');
            let text = format!("{base}\u{0301}");
            frame.print_text(0, 0, &text, Cell::default());
        }
    }

    fn key_event(c: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    fn new_counter(value: i32) -> Counter {
        Counter {
            value,
            initialized: false,
        }
    }

    fn new_grapheme_churn() -> GraphemeChurn {
        GraphemeChurn { value: 0 }
    }

    // ---- Construction and lifecycle ----

    #[test]
    fn new_creates_uninitialized_program() {
        let prog = StepProgram::new(new_counter(0), 80, 24);
        assert!(!prog.is_initialized());
        assert!(prog.is_running());
        assert_eq!(prog.size(), (80, 24));
        assert_eq!(prog.frame_idx(), 0);
        assert!(prog.tick_rate().is_none());
    }

    #[test]
    fn init_initializes_model_and_renders_first_frame() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        assert!(prog.is_initialized());
        assert!(prog.model().initialized);
        assert_eq!(prog.frame_idx(), 1); // First frame rendered.

        let outputs = prog.outputs();
        assert!(outputs.last_buffer.is_some());
        assert!(outputs.last_full_repaint_hint); // First frame is full repaint.
        assert_eq!(outputs.last_patches.len(), 1);
        let stats = outputs
            .last_patch_stats
            .expect("patch stats should be captured");
        assert_eq!(stats.patch_count, 1);
        assert_eq!(stats.dirty_cells, 80 * 24);
    }

    #[test]
    #[should_panic(expected = "init() called twice")]
    fn double_init_panics() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        prog.init().unwrap();
    }

    #[test]
    #[should_panic(expected = "step() called before init()")]
    fn step_before_init_panics() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        let _ = prog.step();
    }

    // ---- Event processing ----

    #[test]
    fn step_processes_pushed_events() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.push_event(key_event('+'));
        prog.push_event(key_event('+'));
        prog.push_event(key_event('+'));
        let result = prog.step().unwrap();

        assert!(result.running);
        assert!(result.rendered);
        assert_eq!(result.events_processed, 3);
        assert_eq!(prog.model().value, 3);
    }

    #[test]
    fn step_with_no_events_does_not_render() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        // Take initial outputs.
        prog.take_outputs();

        let result = prog.step().unwrap();
        assert!(result.running);
        assert!(!result.rendered);
        assert_eq!(result.events_processed, 0);
    }

    #[test]
    fn quit_event_stops_program() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.push_event(key_event('+'));
        prog.push_event(key_event('q'));
        prog.push_event(key_event('+')); // Should not be processed.
        let result = prog.step().unwrap();

        assert!(!result.running);
        assert!(!prog.is_running());
        assert_eq!(prog.model().value, 1); // Only first '+' processed.
    }

    #[test]
    fn step_after_quit_returns_immediately() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.push_event(key_event('q'));
        prog.step().unwrap();

        // Further steps do nothing.
        prog.push_event(key_event('+'));
        let result = prog.step().unwrap();
        assert!(!result.running);
        assert!(!result.rendered);
        assert_eq!(result.events_processed, 0);
        assert_eq!(prog.model().value, 0);
    }

    // ---- Resize ----

    #[test]
    fn resize_updates_dimensions() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.resize(120, 40);
        prog.step().unwrap();

        assert_eq!(prog.size(), (120, 40));
    }

    #[test]
    fn resize_produces_correctly_sized_buffer() {
        let mut prog = StepProgram::new(new_counter(42), 80, 24);
        prog.init().unwrap();

        prog.resize(40, 10);
        prog.step().unwrap();

        let outputs = prog.outputs();
        let buf = outputs.last_buffer.as_ref().unwrap();
        assert_eq!(buf.width(), 40);
        assert_eq!(buf.height(), 10);
    }

    // ---- Tick handling ----

    #[test]
    fn tick_fires_when_rate_elapsed() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        // Schedule tick at 100ms intervals.
        prog.push_event(key_event('+')); // Will map to Increment, but we use send for ScheduleTick.
        prog.step().unwrap();

        // Manually set tick rate (since our test model doesn't emit ScheduleTick from events).
        prog.model_mut().value = 0;
        // Directly use the Cmd to schedule ticks through a dedicated message.
        prog.execute_cmd(Cmd::tick(Duration::from_millis(100)));
        prog.dirty = false; // Reset dirty so we can detect tick-triggered renders.

        // Advance less than tick rate — no tick.
        prog.advance_time(Duration::from_millis(50));
        let result = prog.step().unwrap();
        assert_eq!(prog.model().value, 0);
        assert!(!result.rendered);

        // Advance past tick rate — tick fires.
        prog.advance_time(Duration::from_millis(60));
        let result = prog.step().unwrap();
        assert_eq!(prog.model().value, 1); // Tick -> Increment.
        assert!(result.rendered);
    }

    #[test]
    fn tick_uses_deterministic_clock() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();
        prog.execute_cmd(Cmd::tick(Duration::from_millis(100)));

        // Set absolute time to trigger tick.
        prog.set_time(Duration::from_millis(200));
        prog.step().unwrap();
        assert_eq!(prog.model().value, 1);

        // Advance to next tick boundary.
        prog.set_time(Duration::from_millis(350));
        prog.step().unwrap();
        assert_eq!(prog.model().value, 2);
    }

    // ---- Command execution ----

    #[test]
    fn log_command_captures_to_presenter() {
        let mut prog = StepProgram::new(new_counter(5), 80, 24);
        prog.init().unwrap();

        // LogValue emits Cmd::Log("value=5").
        prog.execute_cmd(Cmd::msg(CounterMsg::LogValue));

        let outputs = prog.outputs();
        assert_eq!(outputs.logs, vec!["value=5"]);
    }

    #[test]
    fn batch_command_executes_all() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.execute_cmd(Cmd::msg(CounterMsg::BatchIncrement(5)));
        assert_eq!(prog.model().value, 5);
    }

    #[test]
    fn task_executes_synchronously() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.execute_cmd(Cmd::msg(CounterMsg::SpawnTask));
        assert_eq!(prog.model().value, 1); // Task returns Increment.
    }

    #[test]
    fn set_mouse_capture_updates_features() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        prog.execute_cmd(Cmd::set_mouse_capture(true));
        assert!(prog.backend().events.features().mouse_capture);

        prog.execute_cmd(Cmd::set_mouse_capture(false));
        assert!(!prog.backend().events.features().mouse_capture);
    }

    // ---- Rendering ----

    #[test]
    fn rendered_buffer_reflects_model_state() {
        let mut prog = StepProgram::new(new_counter(42), 80, 24);
        prog.init().unwrap();

        let outputs = prog.outputs();
        let buf = outputs.last_buffer.as_ref().unwrap();

        // "Count: 42"
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('C'));
        assert_eq!(buf.get(7, 0).unwrap().content.as_char(), Some('4'));
        assert_eq!(buf.get(8, 0).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn subsequent_renders_produce_diffs() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        // First frame is full repaint.
        let outputs = prog.take_outputs();
        assert!(outputs.last_full_repaint_hint);

        // Second frame after an event should not be full repaint.
        prog.push_event(key_event('+'));
        prog.step().unwrap();

        let outputs = prog.outputs();
        assert!(!outputs.last_full_repaint_hint);
        assert!(!outputs.last_patches.is_empty());
        let stats = outputs
            .last_patch_stats
            .expect("patch stats should be captured");
        assert!(stats.patch_count >= 1);
        assert!(stats.dirty_cells >= 1);
    }

    #[test]
    fn take_outputs_clears_state() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        let outputs = prog.take_outputs();
        assert!(outputs.last_buffer.is_some());

        // After take, outputs should be empty.
        let outputs = prog.outputs();
        assert!(outputs.last_buffer.is_none());
        assert!(outputs.logs.is_empty());
    }

    // ---- Determinism ----

    #[test]
    fn identical_inputs_produce_identical_outputs() {
        fn run_scenario() -> (i32, u64, Vec<Option<char>>) {
            let mut prog = StepProgram::new(new_counter(0), 20, 1);
            prog.init().unwrap();

            prog.push_event(key_event('+'));
            prog.push_event(key_event('+'));
            prog.push_event(key_event('-'));
            prog.push_event(key_event('+'));
            prog.step().unwrap();

            let outputs = prog.outputs();
            let buf = outputs.last_buffer.as_ref().unwrap();
            let chars: Vec<Option<char>> = (0..20)
                .map(|x| buf.get(x, 0).and_then(|c| c.content.as_char()))
                .collect();

            (prog.model().value, prog.frame_idx(), chars)
        }

        let (v1, f1, c1) = run_scenario();
        let (v2, f2, c2) = run_scenario();
        let (v3, f3, c3) = run_scenario();

        assert_eq!(v1, v2);
        assert_eq!(v2, v3);
        assert_eq!(v1, 2); // +1+1-1+1 = 2
        assert_eq!(f1, f2);
        assert_eq!(f2, f3);
        assert_eq!(c1, c2);
        assert_eq!(c2, c3);
    }

    // ---- with_backend constructor ----

    #[test]
    fn with_backend_uses_provided_backend() {
        let mut backend = WebBackend::new(100, 50);
        backend.clock_mut().set(Duration::from_secs(10));

        let prog = StepProgram::with_backend(new_counter(0), backend);
        assert_eq!(prog.size(), (100, 50));
    }

    // ---- Multi-step scenario ----

    #[test]
    fn multi_step_interaction() {
        let mut prog = StepProgram::new(new_counter(0), 80, 24);
        prog.init().unwrap();

        // Frame 1: increment twice.
        prog.push_event(key_event('+'));
        prog.push_event(key_event('+'));
        let r1 = prog.step().unwrap();
        assert_eq!(r1.events_processed, 2);
        assert!(r1.rendered);
        assert_eq!(prog.model().value, 2);

        // Frame 2: decrement once.
        prog.push_event(key_event('-'));
        let r2 = prog.step().unwrap();
        assert_eq!(r2.events_processed, 1);
        assert_eq!(prog.model().value, 1);

        // Frame 3: no events.
        let r3 = prog.step().unwrap();
        assert_eq!(r3.events_processed, 0);
        assert!(!r3.rendered);

        // Frame indices are monotonic.
        assert!(r2.frame_idx > r1.frame_idx);
        assert_eq!(r3.frame_idx, r2.frame_idx); // No render, same index.
    }

    #[test]
    fn periodic_pool_gc_bounds_grapheme_growth() {
        let mut prog = StepProgram::new(new_grapheme_churn(), 8, 1);
        prog.init().unwrap();
        prog.execute_cmd(Cmd::tick(Duration::from_millis(1)));

        let mut peak_pool_len = prog.pool().len();
        for _ in 0..2000 {
            prog.advance_time(Duration::from_millis(1));
            let _ = prog.step().unwrap();
            peak_pool_len = peak_pool_len.max(prog.pool().len());
        }

        let final_pool_len = prog.pool().len();
        assert!(
            peak_pool_len <= (POOL_GC_INTERVAL_FRAMES as usize).saturating_add(2),
            "peak grapheme pool length should stay bounded by GC interval (peak={peak_pool_len})"
        );
        assert!(
            final_pool_len <= (POOL_GC_INTERVAL_FRAMES as usize).saturating_add(2),
            "final grapheme pool length should stay bounded by GC interval (final={final_pool_len})"
        );
    }
}
