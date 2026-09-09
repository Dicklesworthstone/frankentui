#![forbid(unsafe_code)]

//! Step-based program runner for WASM targets.
//!
//! [`WasmRunner`] drives an ftui [`Model`] without threads, blocking polls, or
//! OS-level I/O. Instead, the host (JavaScript) delivers events via
//! `push_event` and calls `step` / `render` from its own animation loop.
//!
//! The execution model is:
//! ```text
//! JS animation frame
//!   → push_event(Event)        // keyboard, mouse, resize
//!   → step(now)                // drain events, fire ticks, run model.update
//!   → render()                 // if dirty: model.view → Buffer + optional Diff
//!   → present the output       // apply patches to WebGPU / canvas
//! ```
//!
//! Deterministic record/replay: all inputs go through the event queue with
//! monotonic timestamps from the host clock (`performance.now()`), so replaying
//! the same event stream produces identical frames.

use crate::program::{Cmd, Model};
use ftui_core::event::Event;
use ftui_render::buffer::Buffer;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use std::collections::VecDeque;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Outcome of a single [`WasmRunner::step`] call.
#[derive(Debug, Clone, Copy, Default)]
pub struct StepResult {
    /// Number of queued events processed in this step.
    pub events_processed: u32,
    /// Whether a tick was delivered to the model.
    pub tick_fired: bool,
    /// Whether the model's view is dirty (render needed).
    pub dirty: bool,
    /// Whether the model issued `Cmd::Quit`.
    pub quit: bool,
}

/// Rendered frame output from [`WasmRunner::render`].
pub struct RenderedFrame<'a> {
    /// The full rendered buffer.
    pub buffer: &'a Buffer,
    /// Diff against the previous frame (`None` on first render or after resize).
    pub diff: Option<BufferDiff>,
    /// Sequential frame index (starts at 0).
    pub frame_idx: u64,
}

// ---------------------------------------------------------------------------
// WasmRunner
// ---------------------------------------------------------------------------

/// Step-based program runner for WASM (no threads, no blocking).
///
/// Accepts an ftui [`Model`] and drives it through explicit `step` / `render`
/// calls controlled by the JavaScript host.
pub struct WasmRunner<M: Model> {
    model: M,
    pool: GraphemePool,

    /// Current rendered buffer.
    current: Buffer,
    /// Previous buffer for diffing.
    previous: Option<Buffer>,

    running: bool,
    dirty: bool,
    initialized: bool,

    width: u16,
    height: u16,
    frame_idx: u64,

    /// Tick interval requested by the model (via `Cmd::Tick`).
    tick_rate: Option<Duration>,
    /// Monotonic timestamp of the last tick delivery.
    last_tick_at: Duration,

    /// Buffered events from the host.
    event_queue: VecDeque<Event>,

    /// Log messages emitted via `Cmd::Log`.
    logs: Vec<String>,
}

impl<M: Model> WasmRunner<M> {
    /// Create a new runner with the given model and initial grid size.
    ///
    /// The model is not initialized until [`init`](Self::init) is called.
    #[must_use]
    pub fn new(model: M, width: u16, height: u16) -> Self {
        Self {
            model,
            pool: GraphemePool::new(),
            current: Buffer::new(width, height),
            previous: None,
            running: true,
            dirty: true, // First frame is always dirty.
            initialized: false,
            width,
            height,
            frame_idx: 0,
            tick_rate: None,
            last_tick_at: Duration::ZERO,
            event_queue: VecDeque::new(),
            logs: Vec::new(),
        }
    }

    /// Initialize the model by calling `Model::init()`.
    ///
    /// Must be called exactly once before `step` / `render`. Returns the
    /// result of executing the init command.
    pub fn init(&mut self) -> StepResult {
        let cmd = self.model.init();
        self.initialized = true;
        self.dirty = true;
        let mut result = StepResult {
            dirty: true,
            ..Default::default()
        };
        self.execute_cmd(cmd, &mut result);
        result
    }

    // -- Event delivery -----------------------------------------------------

    /// Buffer a single event for processing on the next `step`.
    pub fn push_event(&mut self, event: Event) {
        self.event_queue.push_back(event);
    }

    /// Buffer multiple events for processing on the next `step`.
    pub fn push_events(&mut self, events: impl IntoIterator<Item = Event>) {
        self.event_queue.extend(events);
    }

    // -- Step ---------------------------------------------------------------

    /// Process all buffered events and fire a tick if due.
    ///
    /// `now` is the monotonic timestamp from the host clock (e.g.
    /// `performance.now()` converted to `Duration`).
    ///
    /// Returns a [`StepResult`] summarizing what happened.
    pub fn step(&mut self, now: Duration) -> StepResult {
        if !self.running || !self.initialized {
            return StepResult {
                quit: !self.running,
                ..Default::default()
            };
        }

        let mut result = StepResult::default();

        // Drain all buffered events.
        while let Some(event) = self.event_queue.pop_front() {
            if !self.running {
                break;
            }
            self.handle_event(event, &mut result);
            result.events_processed += 1;
        }

        // Tick check.
        if let Some(rate) = self.tick_rate
            && now.saturating_sub(self.last_tick_at) >= rate
        {
            self.last_tick_at = now;
            let msg = M::Message::from(Event::Tick);
            let cmd = self.model.update(msg);
            self.dirty = true;
            result.tick_fired = true;
            self.execute_cmd(cmd, &mut result);
        }

        result.dirty = self.dirty;
        result.quit = !self.running;
        result
    }

    /// Process a single event immediately (without buffering).
    pub fn step_event(&mut self, event: Event) -> StepResult {
        if !self.running || !self.initialized {
            return StepResult {
                quit: !self.running,
                ..Default::default()
            };
        }

        let mut result = StepResult::default();
        self.handle_event(event, &mut result);
        result.events_processed = 1;
        result.dirty = self.dirty;
        result.quit = !self.running;
        result
    }

    // -- Render -------------------------------------------------------------

    /// Render the current frame if dirty.
    ///
    /// Returns `Some(RenderedFrame)` with the buffer and optional diff, or
    /// `None` if the view is clean (no events since last render).
    pub fn render(&mut self) -> Option<RenderedFrame<'_>> {
        if !self.dirty {
            return None;
        }
        Some(self.force_render())
    }

    /// Render the current frame unconditionally.
    pub fn force_render(&mut self) -> RenderedFrame<'_> {
        let mut frame = Frame::new(self.width, self.height, &mut self.pool);
        self.model.view(&mut frame);

        // Compute diff against previous buffer.
        let diff = self
            .previous
            .as_ref()
            .map(|prev| BufferDiff::compute(prev, &frame.buffer));

        // Rotate buffers.
        self.previous = Some(std::mem::replace(&mut self.current, frame.buffer));

        self.dirty = false;
        let idx = self.frame_idx;
        self.frame_idx += 1;

        RenderedFrame {
            buffer: &self.current,
            diff,
            frame_idx: idx,
        }
    }

    // -- Resize -------------------------------------------------------------

    /// Resize the grid. Marks the view dirty and invalidates the diff baseline.
    pub fn resize(&mut self, width: u16, height: u16) {
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        self.current = Buffer::new(width, height);
        self.previous = None; // Force full repaint.
        self.dirty = true;

        // Deliver resize event to the model.
        if self.running && self.initialized {
            let msg = M::Message::from(Event::Resize { width, height });
            let cmd = self.model.update(msg);
            let mut result = StepResult::default();
            self.execute_cmd(cmd, &mut result);
        }
    }

    // -- Accessors ----------------------------------------------------------

    /// Whether the program is still running (no `Cmd::Quit` received).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether the view needs rendering.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether `init()` has been called.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Current grid dimensions.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Sequential frame index (incremented on each render).
    #[must_use]
    pub fn frame_idx(&self) -> u64 {
        self.frame_idx
    }

    /// Current tick rate, if set by the model.
    #[must_use]
    pub fn tick_rate(&self) -> Option<Duration> {
        self.tick_rate
    }

    /// Number of buffered events awaiting processing.
    #[must_use]
    pub fn pending_events(&self) -> usize {
        self.event_queue.len()
    }

    /// Reference to the model.
    #[inline]
    #[must_use]
    pub fn model(&self) -> &M {
        &self.model
    }

    /// Mutable reference to the model.
    #[inline]
    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// Drain and return accumulated log messages.
    #[inline]
    pub fn drain_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    /// Reference to accumulated log messages.
    #[inline]
    #[must_use]
    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    /// Reference to the most recently rendered buffer.
    #[inline]
    #[must_use]
    pub fn current_buffer(&self) -> &Buffer {
        &self.current
    }

    // -- Internal -----------------------------------------------------------

    fn handle_event(&mut self, event: Event, result: &mut StepResult) {
        // Handle resize events specially: update our dimensions.
        if let Event::Resize { width, height } = event
            && (width != self.width || height != self.height)
        {
            self.width = width;
            self.height = height;
            self.current = Buffer::new(width, height);
            self.previous = None;
        }

        let msg = M::Message::from(event);
        let cmd = self.model.update(msg);
        self.dirty = true;
        self.execute_cmd(cmd, result);
    }

    fn execute_cmd(&mut self, cmd: Cmd<M::Message>, result: &mut StepResult) {
        match cmd {
            Cmd::None => {}
            Cmd::Quit => {
                self.running = false;
                result.quit = true;
            }
            Cmd::Msg(m) => {
                let cmd = self.model.update(m);
                self.execute_cmd(cmd, result);
            }
            Cmd::Batch(cmds) => {
                for c in cmds {
                    if !self.running {
                        break;
                    }
                    self.execute_cmd(c, result);
                }
            }
            Cmd::Sequence(cmds) => {
                for c in cmds {
                    if !self.running {
                        break;
                    }
                    self.execute_cmd(c, result);
                }
            }
            Cmd::Tick(duration) => {
                self.tick_rate = Some(duration);
            }
            Cmd::Log { text, mode } => {
                self.logs
                    .push(ftui_render::sanitize::sanitize_with(&text, mode).into_owned());
            }
            Cmd::Task(_, f) => {
                // Execute synchronously (no threads in WASM).
                let msg = f();
                let cmd = self.model.update(msg);
                self.execute_cmd(cmd, result);
            }
            Cmd::SetMouseCapture(_) => {
                // No-op: mouse capture is managed by the JS host.
            }
            Cmd::SaveState | Cmd::RestoreState => {
                // No-op: state persistence is managed by the JS host
                // (localStorage / IndexedDB).
            }
            Cmd::SetTickStrategy(_) => {
                // No-op: tick strategy is managed by the host runtime.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_render::cell::Cell;

    // -- Test model ---------------------------------------------------------

    struct Counter {
        value: i32,
    }

    #[derive(Debug)]
    #[allow(dead_code)]
    enum CounterMsg {
        Increment,
        Decrement,
        Quit,
        ScheduleTick,
        LogValue,
    }

    impl From<Event> for CounterMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Key(k) if k.code == KeyCode::Char('+') => CounterMsg::Increment,
                Event::Key(k) if k.code == KeyCode::Char('-') => CounterMsg::Decrement,
                Event::Key(k) if k.code == KeyCode::Char('q') => CounterMsg::Quit,
                _ => CounterMsg::Increment,
            }
        }
    }

    impl Model for Counter {
        type Message = CounterMsg;

        fn init(&mut self) -> Cmd<Self::Message> {
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
                CounterMsg::Quit => Cmd::quit(),
                CounterMsg::ScheduleTick => Cmd::tick(Duration::from_millis(100)),
                CounterMsg::LogValue => Cmd::log(format!("value={}", self.value)),
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

    fn key_event(c: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    // -- Tests --------------------------------------------------------------

    #[test]
    fn init_marks_dirty() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        let result = runner.init();
        assert!(result.dirty);
        assert!(runner.is_initialized());
    }

    #[test]
    fn step_event_updates_model() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();

        let r = runner.step_event(key_event('+'));
        assert_eq!(runner.model().value, 1);
        assert!(r.dirty);
        assert_eq!(r.events_processed, 1);
    }

    #[test]
    fn buffered_events_drain_on_step() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();

        runner.push_event(key_event('+'));
        runner.push_event(key_event('+'));
        runner.push_event(key_event('+'));

        let r = runner.step(Duration::ZERO);
        assert_eq!(r.events_processed, 3);
        assert_eq!(runner.model().value, 3);
        assert_eq!(runner.pending_events(), 0);
    }

    #[test]
    fn quit_stops_processing() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();

        runner.push_event(key_event('+'));
        runner.push_event(key_event('q'));
        runner.push_event(key_event('+'));

        let r = runner.step(Duration::ZERO);
        assert!(r.quit);
        assert!(!runner.is_running());
        assert_eq!(runner.model().value, 1);
    }

    #[test]
    fn render_produces_buffer() {
        let mut runner = WasmRunner::new(Counter { value: 42 }, 80, 24);
        runner.init();

        let frame = runner.render().expect("should be dirty after init");
        assert_eq!(frame.frame_idx, 0);
        // First render has no diff (no previous buffer).
        assert!(frame.diff.is_none());
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('C'));
    }

    #[test]
    fn render_returns_none_when_clean() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();

        runner.render(); // Consume dirty.
        assert!(runner.render().is_none());
    }

    #[test]
    fn second_render_has_diff() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();

        runner.render(); // First frame, no diff.
        runner.step_event(key_event('+'));

        let frame = runner.render().expect("dirty after event");
        assert_eq!(frame.frame_idx, 1);
        // Should have a diff since we have a previous buffer.
        assert!(frame.diff.is_some());
    }

    #[test]
    fn resize_invalidates_diff_baseline() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();
        runner.render();

        runner.resize(100, 40);
        assert!(runner.is_dirty());
        assert_eq!(runner.size(), (100, 40));

        let frame = runner.render().expect("dirty after resize");
        // No diff after resize (baseline invalidated).
        assert!(frame.diff.is_none());
    }

    #[test]
    fn tick_fires_when_due() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();
        runner.render();

        // Schedule tick at 100ms.
        runner.step_event(Event::Key(KeyEvent {
            code: KeyCode::Char('t'),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        }));
        // 't' maps to Increment (default), so override:
        // We need a model that emits Cmd::Tick. Let's just set tick_rate directly.

        // Actually, let's test by sending a message.
        runner.model_mut().value = 0;
        // Force a tick rate.
        let cmd: Cmd<CounterMsg> = Cmd::tick(Duration::from_millis(100));
        let mut result = StepResult::default();
        runner.execute_cmd(cmd, &mut result);

        // Step at t=50ms: no tick.
        let r = runner.step(Duration::from_millis(50));
        assert!(!r.tick_fired);

        // Step at t=100ms: tick fires.
        let r = runner.step(Duration::from_millis(100));
        assert!(r.tick_fired);
    }

    #[test]
    fn logs_accumulate() {
        let mut runner = WasmRunner::new(Counter { value: 5 }, 80, 24);
        runner.init();

        runner.step_event(key_event('+'));
        let cmd: Cmd<CounterMsg> = Cmd::log("hello");
        let mut result = StepResult::default();
        runner.execute_cmd(cmd, &mut result);

        assert_eq!(runner.logs(), &["hello"]);

        let drained = runner.drain_logs();
        assert_eq!(drained, &["hello"]);
        assert!(runner.logs().is_empty());
    }

    #[test]
    fn log_modes_capture_policy_text_without_terminal_formatting() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();
        let payload = "a\x1b[31mb\x1b[2Jc\x1b]0;secret\x07d\n";
        let mut result = StepResult::default();
        runner.execute_cmd(
            Cmd::sequence(vec![
                Cmd::log(payload),
                Cmd::log_sgr_only(payload),
                Cmd::log_raw(payload),
                Cmd::quit(),
                Cmd::log("unreachable"),
            ]),
            &mut result,
        );
        assert!(result.quit);
        assert_eq!(runner.logs(), &["abcd\n", "a\x1b[31mbcd\n", payload]);
    }

    #[test]
    fn deterministic_replay() {
        fn run_scenario() -> Vec<Option<char>> {
            let mut runner = WasmRunner::new(Counter { value: 0 }, 20, 1);
            runner.init();

            runner.push_event(key_event('+'));
            runner.push_event(key_event('+'));
            runner.push_event(key_event('-'));
            runner.push_event(key_event('+'));
            runner.step(Duration::ZERO);

            let frame = runner.render().unwrap();
            (0..20)
                .map(|x| frame.buffer.get(x, 0).and_then(|c| c.content.as_char()))
                .collect()
        }

        let r1 = run_scenario();
        let r2 = run_scenario();
        let r3 = run_scenario();
        assert_eq!(r1, r2);
        assert_eq!(r2, r3);
    }

    #[test]
    fn events_after_quit_ignored() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();

        runner.step_event(key_event('q'));
        assert!(!runner.is_running());

        let r = runner.step_event(key_event('+'));
        assert_eq!(r.events_processed, 0);
        assert_eq!(runner.model().value, 0);
    }

    #[test]
    fn step_before_init_is_noop() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        let r = runner.step(Duration::ZERO);
        assert_eq!(r.events_processed, 0);
        assert!(!runner.is_initialized());
    }

    #[test]
    fn task_executes_synchronously() {
        struct TaskModel {
            result: Option<i32>,
        }

        #[derive(Debug)]
        enum TaskMsg {
            SpawnTask,
            SetResult(i32),
        }

        impl From<Event> for TaskMsg {
            fn from(_: Event) -> Self {
                TaskMsg::SpawnTask
            }
        }

        impl Model for TaskModel {
            type Message = TaskMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    TaskMsg::SpawnTask => Cmd::task(|| TaskMsg::SetResult(42)),
                    TaskMsg::SetResult(v) => {
                        self.result = Some(v);
                        Cmd::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut runner = WasmRunner::new(TaskModel { result: None }, 80, 24);
        runner.init();

        // Any key event maps to SpawnTask.
        runner.step_event(key_event('x'));
        assert_eq!(runner.model().result, Some(42));
    }

    #[test]
    fn resize_delivers_event_to_model() {
        struct SizeModel {
            last_size: Option<(u16, u16)>,
        }

        #[derive(Debug)]
        enum SizeMsg {
            Resize(u16, u16),
            Other,
        }

        impl From<Event> for SizeMsg {
            fn from(event: Event) -> Self {
                match event {
                    Event::Resize { width, height } => SizeMsg::Resize(width, height),
                    _ => SizeMsg::Other,
                }
            }
        }

        impl Model for SizeModel {
            type Message = SizeMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                if let SizeMsg::Resize(w, h) = msg {
                    self.last_size = Some((w, h));
                }
                Cmd::none()
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut runner = WasmRunner::new(SizeModel { last_size: None }, 80, 24);
        runner.init();
        runner.resize(120, 40);
        assert_eq!(runner.model().last_size, Some((120, 40)));
    }

    #[test]
    fn force_render_always_produces_frame() {
        let mut runner = WasmRunner::new(Counter { value: 0 }, 80, 24);
        runner.init();

        runner.render(); // Consume dirty.
        assert!(!runner.is_dirty());

        let frame = runner.force_render();
        assert_eq!(frame.frame_idx, 1);
    }
}
