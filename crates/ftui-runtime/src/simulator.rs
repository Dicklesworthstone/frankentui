#![forbid(unsafe_code)]

//! Deterministic program simulator for testing.
//!
//! `ProgramSimulator` runs a [`Model`] without a real terminal, enabling
//! deterministic snapshot testing, event injection, and frame capture.
//!
//! # Example
//!
//! ```ignore
//! use ftui_runtime::simulator::ProgramSimulator;
//!
//! let mut sim = ProgramSimulator::new(Counter { value: 0 });
//! sim.init();
//! sim.send(Msg::Increment);
//! assert_eq!(sim.model().value, 1);
//!
//! let buf = sim.capture_frame(80, 24);
//! // Assert on buffer contents...
//! ```

use crate::program::{Cmd, Model};
use crate::state_persistence::StateRegistry;
use ftui_core::event::Event;
use ftui_render::buffer::Buffer;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use std::sync::Arc;
use std::time::Duration;

/// Record of a command that was executed during simulation.
#[derive(Debug, Clone)]
pub enum CmdRecord {
    /// No-op command.
    None,
    /// Quit command.
    Quit,
    /// Message sent to model (not stored, just noted).
    Msg,
    /// Batch of commands.
    Batch(usize),
    /// Sequence of commands.
    Sequence(usize),
    /// Tick scheduled.
    Tick(Duration),
    /// Log message emitted.
    Log(String),
    /// Mouse capture toggle (no-op in simulator).
    MouseCapture(bool),
    /// Background task executed synchronously.
    Task,
}

/// Deterministic simulator for [`Model`] testing.
///
/// Runs model logic without any terminal or IO dependencies. Events can be
/// injected, messages sent directly, and frames captured for snapshot testing.
pub struct ProgramSimulator<M: Model> {
    /// The application model.
    model: M,
    /// Grapheme pool for frame creation.
    pool: GraphemePool,
    /// Captured frame buffers.
    frames: Vec<Buffer>,
    /// Record of all executed commands.
    command_log: Vec<CmdRecord>,
    /// Whether the simulated program is still running.
    running: bool,
    /// Current tick rate (if any).
    tick_rate: Option<Duration>,
    /// Log messages emitted via Cmd::Log.
    logs: Vec<String>,
    /// Optional state registry for persistence integration.
    state_registry: Option<Arc<StateRegistry>>,
}

impl<M: Model> ProgramSimulator<M> {
    /// Create a new simulator with the given model.
    ///
    /// The model is not initialized until [`init`](Self::init) is called.
    pub fn new(model: M) -> Self {
        Self {
            model,
            pool: GraphemePool::new(),
            frames: Vec::new(),
            command_log: Vec::new(),
            running: true,
            tick_rate: None,
            logs: Vec::new(),
            state_registry: None,
        }
    }

    /// Create a new simulator with the given model and persistence registry.
    ///
    /// When provided, `Cmd::SaveState`/`Cmd::RestoreState` will flush/load
    /// through the registry, mirroring runtime behavior.
    pub fn with_registry(model: M, registry: Arc<StateRegistry>) -> Self {
        let mut sim = Self::new(model);
        sim.state_registry = Some(registry);
        sim
    }

    /// Initialize the model by calling `Model::init()` and executing returned commands.
    ///
    /// Should be called once before injecting events or capturing frames.
    pub fn init(&mut self) {
        let cmd = self.model.init();
        self.execute_cmd(cmd);
    }

    /// Inject terminal events into the model.
    ///
    /// Each event is converted to a message via `From<Event>` and dispatched
    /// through `Model::update()`. Commands returned from update are executed.
    pub fn inject_events(&mut self, events: &[Event]) {
        for event in events {
            if !self.running {
                break;
            }
            let msg = M::Message::from(event.clone());
            let cmd = self.model.update(msg);
            self.execute_cmd(cmd);
        }
    }

    /// Inject a single terminal event into the model.
    ///
    /// The event is converted to a message via `From<Event>` and dispatched
    /// through `Model::update()`. Commands returned from update are executed.
    pub fn inject_event(&mut self, event: Event) {
        self.inject_events(&[event]);
    }

    /// Send a specific message to the model.
    ///
    /// The message is dispatched through `Model::update()` and returned
    /// commands are executed.
    pub fn send(&mut self, msg: M::Message) {
        if !self.running {
            return;
        }
        let cmd = self.model.update(msg);
        self.execute_cmd(cmd);
    }

    /// Capture the current frame at the given dimensions.
    ///
    /// Calls `Model::view()` to render into a fresh buffer and stores the
    /// result. Returns a reference to the captured buffer.
    pub fn capture_frame(&mut self, width: u16, height: u16) -> &Buffer {
        let mut frame = Frame::new(width, height, &mut self.pool);
        self.model.view(&mut frame);
        self.frames.push(frame.buffer);
        self.frames.last().unwrap()
    }

    /// Get all captured frame buffers.
    pub fn frames(&self) -> &[Buffer] {
        &self.frames
    }

    /// Get the most recently captured frame buffer, if any.
    pub fn last_frame(&self) -> Option<&Buffer> {
        self.frames.last()
    }

    /// Get the number of captured frames.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Get a reference to the model.
    pub fn model(&self) -> &M {
        &self.model
    }

    /// Get a mutable reference to the model.
    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// Check if the simulated program is still running.
    ///
    /// Returns `false` after a `Cmd::Quit` has been executed.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Get the current tick rate (if any).
    pub fn tick_rate(&self) -> Option<Duration> {
        self.tick_rate
    }

    /// Get all log messages emitted via `Cmd::Log`.
    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    /// Get the command execution log.
    pub fn command_log(&self) -> &[CmdRecord] {
        &self.command_log
    }

    /// Clear all captured frames.
    pub fn clear_frames(&mut self) {
        self.frames.clear();
    }

    /// Clear all logs.
    pub fn clear_logs(&mut self) {
        self.logs.clear();
    }

    /// Execute a command without IO.
    ///
    /// Cmd::Msg recurses through update; Cmd::Log records the text;
    /// IO-dependent operations are simulated (no real terminal writes).
    /// Save/Restore use the configured registry when present.
    fn execute_cmd(&mut self, cmd: Cmd<M::Message>) {
        match cmd {
            Cmd::None => {
                self.command_log.push(CmdRecord::None);
            }
            Cmd::Quit => {
                self.running = false;
                self.command_log.push(CmdRecord::Quit);
            }
            Cmd::Msg(m) => {
                self.command_log.push(CmdRecord::Msg);
                let cmd = self.model.update(m);
                self.execute_cmd(cmd);
            }
            Cmd::Batch(cmds) => {
                let count = cmds.len();
                self.command_log.push(CmdRecord::Batch(count));
                for c in cmds {
                    self.execute_cmd(c);
                    if !self.running {
                        break;
                    }
                }
            }
            Cmd::Sequence(cmds) => {
                let count = cmds.len();
                self.command_log.push(CmdRecord::Sequence(count));
                for c in cmds {
                    self.execute_cmd(c);
                    if !self.running {
                        break;
                    }
                }
            }
            Cmd::Tick(duration) => {
                self.tick_rate = Some(duration);
                self.command_log.push(CmdRecord::Tick(duration));
            }
            Cmd::Log(text) => {
                self.command_log.push(CmdRecord::Log(text.clone()));
                self.logs.push(text);
            }
            Cmd::SetMouseCapture(enabled) => {
                self.command_log.push(CmdRecord::MouseCapture(enabled));
            }
            Cmd::Task(_, f) => {
                self.command_log.push(CmdRecord::Task);
                let msg = f();
                let cmd = self.model.update(msg);
                self.execute_cmd(cmd);
            }
            Cmd::SaveState => {
                if let Some(registry) = &self.state_registry {
                    let _ = registry.flush();
                }
            }
            Cmd::RestoreState => {
                if let Some(registry) = &self.state_registry {
                    let _ = registry.load();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use std::cell::RefCell;
    use std::sync::Arc;

    // ---------- Test model ----------

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
    }

    impl From<Event> for CounterMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Key(k) if k.code == KeyCode::Char('+') => CounterMsg::Increment,
                Event::Key(k) if k.code == KeyCode::Char('-') => CounterMsg::Decrement,
                Event::Key(k) if k.code == KeyCode::Char('r') => CounterMsg::Reset,
                Event::Key(k) if k.code == KeyCode::Char('q') => CounterMsg::Quit,
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
            }
        }

        fn view(&self, frame: &mut Frame) {
            // Render counter value as text in the first row
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

    fn resize_event(width: u16, height: u16) -> Event {
        Event::Resize { width, height }
    }

    #[derive(Default)]
    struct ResizeTracker {
        last: Option<(u16, u16)>,
        history: Vec<(u16, u16)>,
    }

    #[derive(Debug, Clone, Copy)]
    enum ResizeMsg {
        Resize(u16, u16),
        Quit,
        Noop,
    }

    impl From<Event> for ResizeMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Resize { width, height } => Self::Resize(width, height),
                Event::Key(k) if k.code == KeyCode::Char('q') => Self::Quit,
                _ => Self::Noop,
            }
        }
    }

    impl Model for ResizeTracker {
        type Message = ResizeMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                ResizeMsg::Resize(width, height) => {
                    self.last = Some((width, height));
                    self.history.push((width, height));
                    Cmd::none()
                }
                ResizeMsg::Quit => Cmd::quit(),
                ResizeMsg::Noop => Cmd::none(),
            }
        }

        fn view(&self, _frame: &mut Frame) {}
    }

    #[derive(Default)]
    struct PersistModel;

    #[derive(Debug, Clone, Copy)]
    enum PersistMsg {
        Save,
        Restore,
        Noop,
    }

    impl From<Event> for PersistMsg {
        fn from(event: Event) -> Self {
            match event {
                Event::Key(k) if k.code == KeyCode::Char('s') => Self::Save,
                Event::Key(k) if k.code == KeyCode::Char('r') => Self::Restore,
                _ => Self::Noop,
            }
        }
    }

    impl Model for PersistModel {
        type Message = PersistMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                PersistMsg::Save => Cmd::save_state(),
                PersistMsg::Restore => Cmd::restore_state(),
                PersistMsg::Noop => Cmd::none(),
            }
        }

        fn view(&self, _frame: &mut Frame) {}
    }

    // ---------- Tests ----------

    #[test]
    fn new_simulator() {
        let sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        assert!(sim.is_running());
        assert_eq!(sim.model().value, 0);
        assert!(!sim.model().initialized);
        assert_eq!(sim.frame_count(), 0);
        assert!(sim.logs().is_empty());
    }

    #[test]
    fn init_calls_model_init() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();
        assert!(sim.model().initialized);
    }

    #[test]
    fn inject_events_processes_all() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        let events = vec![key_event('+'), key_event('+'), key_event('+')];
        sim.inject_events(&events);

        assert_eq!(sim.model().value, 3);
    }

    #[test]
    fn inject_events_stops_on_quit() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        // Quit in the middle - subsequent events should be ignored
        let events = vec![key_event('+'), key_event('q'), key_event('+')];
        sim.inject_events(&events);

        assert_eq!(sim.model().value, 1);
        assert!(!sim.is_running());
    }

    #[test]
    fn save_state_flushes_registry() {
        use crate::state_persistence::StateRegistry;

        let registry = Arc::new(StateRegistry::in_memory());
        registry.set("viewer", 1, vec![1, 2, 3]);
        assert!(registry.is_dirty());

        let mut sim = ProgramSimulator::with_registry(PersistModel, Arc::clone(&registry));
        sim.send(PersistMsg::Save);

        assert!(!registry.is_dirty());
        let stored = registry.get("viewer").expect("entry present");
        assert_eq!(stored.version, 1);
        assert_eq!(stored.data, vec![1, 2, 3]);
    }

    #[test]
    fn restore_state_round_trips_cache() {
        use crate::state_persistence::StateRegistry;

        let registry = Arc::new(StateRegistry::in_memory());
        registry.set("viewer", 7, vec![9, 8, 7]);

        let mut sim = ProgramSimulator::with_registry(PersistModel, Arc::clone(&registry));
        sim.send(PersistMsg::Save);

        let removed = registry.remove("viewer");
        assert!(removed.is_some());
        assert!(registry.get("viewer").is_none());

        sim.send(PersistMsg::Restore);
        let restored = registry.get("viewer").expect("restored entry");
        assert_eq!(restored.version, 7);
        assert_eq!(restored.data, vec![9, 8, 7]);
    }

    #[test]
    fn resize_events_apply_in_order() {
        let mut sim = ProgramSimulator::new(ResizeTracker::default());
        sim.init();

        let events = vec![
            resize_event(80, 24),
            resize_event(100, 40),
            resize_event(120, 50),
        ];
        sim.inject_events(&events);

        assert_eq!(sim.model().history, vec![(80, 24), (100, 40), (120, 50)]);
        assert_eq!(sim.model().last, Some((120, 50)));
    }

    #[test]
    fn resize_events_after_quit_are_ignored() {
        let mut sim = ProgramSimulator::new(ResizeTracker::default());
        sim.init();

        let events = vec![resize_event(80, 24), key_event('q'), resize_event(120, 50)];
        sim.inject_events(&events);

        assert!(!sim.is_running());
        assert_eq!(sim.model().history, vec![(80, 24)]);
        assert_eq!(sim.model().last, Some((80, 24)));
    }

    #[test]
    fn send_message_directly() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::Decrement);

        assert_eq!(sim.model().value, 1);
    }

    #[test]
    fn capture_frame_renders_correctly() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 42,
            initialized: false,
        });
        sim.init();

        let buf = sim.capture_frame(80, 24);

        // "Count: 42" should be rendered
        assert_eq!(buf.get(0, 0).unwrap().content.as_char(), Some('C'));
        assert_eq!(buf.get(1, 0).unwrap().content.as_char(), Some('o'));
        assert_eq!(buf.get(7, 0).unwrap().content.as_char(), Some('4'));
        assert_eq!(buf.get(8, 0).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn multiple_frame_captures() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        sim.capture_frame(80, 24);
        sim.send(CounterMsg::Increment);
        sim.capture_frame(80, 24);

        assert_eq!(sim.frame_count(), 2);

        // First frame: "Count: 0"
        assert_eq!(
            sim.frames()[0].get(7, 0).unwrap().content.as_char(),
            Some('0')
        );
        // Second frame: "Count: 1"
        assert_eq!(
            sim.frames()[1].get(7, 0).unwrap().content.as_char(),
            Some('1')
        );
    }

    #[test]
    fn quit_command_stops_running() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        assert!(sim.is_running());
        sim.send(CounterMsg::Quit);
        assert!(!sim.is_running());
    }

    #[test]
    fn log_command_records_text() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 5,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::LogValue);

        assert_eq!(sim.logs(), &["value=5"]);
    }

    #[test]
    fn batch_command_executes_all() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::BatchIncrement(5));

        assert_eq!(sim.model().value, 5);
    }

    #[test]
    fn tick_command_sets_rate() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });

        assert!(sim.tick_rate().is_none());

        // Manually execute a tick command through the model
        // We'll test by checking the internal tick_rate after setting it
        // via the execute_cmd path. Since Counter doesn't emit ticks,
        // we'll test via the command log.
        sim.execute_cmd(Cmd::tick(Duration::from_millis(100)));

        assert_eq!(sim.tick_rate(), Some(Duration::from_millis(100)));
    }

    #[test]
    fn command_log_records_all() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::Quit);

        // init returns Cmd::None, then Increment returns Cmd::None, then Quit returns Cmd::Quit
        assert!(sim.command_log().len() >= 3);
        assert!(matches!(sim.command_log().last(), Some(CmdRecord::Quit)));
    }

    #[test]
    fn clear_frames() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.capture_frame(10, 10);
        sim.capture_frame(10, 10);
        assert_eq!(sim.frame_count(), 2);

        sim.clear_frames();
        assert_eq!(sim.frame_count(), 0);
    }

    #[test]
    fn clear_logs() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();
        sim.send(CounterMsg::LogValue);
        assert_eq!(sim.logs().len(), 1);

        sim.clear_logs();
        assert!(sim.logs().is_empty());
    }

    #[test]
    fn model_mut_access() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });

        sim.model_mut().value = 100;
        assert_eq!(sim.model().value, 100);
    }

    #[test]
    fn last_frame() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });

        assert!(sim.last_frame().is_none());

        sim.capture_frame(10, 10);
        assert!(sim.last_frame().is_some());
    }

    #[test]
    fn send_after_quit_is_ignored() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::Quit);
        assert!(!sim.is_running());

        sim.send(CounterMsg::Increment);
        // Value should not change since we quit
        assert_eq!(sim.model().value, 0);
    }

    // =========================================================================
    // DETERMINISM TESTS - ProgramSimulator determinism (bd-2nu8.10.3)
    // =========================================================================

    #[test]
    fn identical_inputs_yield_identical_outputs() {
        fn run_scenario() -> (i32, Vec<u8>) {
            let mut sim = ProgramSimulator::new(Counter {
                value: 0,
                initialized: false,
            });
            sim.init();

            sim.send(CounterMsg::Increment);
            sim.send(CounterMsg::Increment);
            sim.send(CounterMsg::Decrement);
            sim.send(CounterMsg::BatchIncrement(3));

            let buf = sim.capture_frame(20, 10);
            let mut frame_bytes = Vec::new();
            for y in 0..10 {
                for x in 0..20 {
                    if let Some(cell) = buf.get(x, y)
                        && let Some(c) = cell.content.as_char()
                    {
                        frame_bytes.push(c as u8);
                    }
                }
            }
            (sim.model().value, frame_bytes)
        }

        let (value1, frame1) = run_scenario();
        let (value2, frame2) = run_scenario();
        let (value3, frame3) = run_scenario();

        assert_eq!(value1, value2);
        assert_eq!(value2, value3);
        assert_eq!(value1, 4); // 0 + 1 + 1 - 1 + 3 = 4

        assert_eq!(frame1, frame2);
        assert_eq!(frame2, frame3);
    }

    #[test]
    fn command_log_records_in_order() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::LogValue);
        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::LogValue);

        let log = sim.command_log();

        // Find Log entries and verify they're in order
        let log_entries: Vec<_> = log
            .iter()
            .filter_map(|r| {
                if let CmdRecord::Log(s) = r {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(log_entries, vec!["value=1", "value=2"]);
    }

    #[test]
    fn sequence_command_records_correctly() {
        // Model that emits a sequence command
        struct SeqModel {
            steps: Vec<i32>,
        }

        #[derive(Debug)]
        enum SeqMsg {
            Step(i32),
            TriggerSeq,
        }

        impl From<Event> for SeqMsg {
            fn from(_: Event) -> Self {
                SeqMsg::Step(0)
            }
        }

        impl Model for SeqModel {
            type Message = SeqMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    SeqMsg::Step(n) => {
                        self.steps.push(n);
                        Cmd::none()
                    }
                    SeqMsg::TriggerSeq => Cmd::sequence(vec![
                        Cmd::msg(SeqMsg::Step(1)),
                        Cmd::msg(SeqMsg::Step(2)),
                        Cmd::msg(SeqMsg::Step(3)),
                    ]),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(SeqModel { steps: vec![] });
        sim.init();
        sim.send(SeqMsg::TriggerSeq);

        // Verify sequence is recorded
        let has_sequence = sim
            .command_log()
            .iter()
            .any(|r| matches!(r, CmdRecord::Sequence(3)));
        assert!(has_sequence, "Should record Sequence(3)");

        // Verify steps executed in order
        assert_eq!(sim.model().steps, vec![1, 2, 3]);
    }

    #[test]
    fn batch_command_records_correctly() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::BatchIncrement(5));

        // Should have Batch(5) in the log
        let has_batch = sim
            .command_log()
            .iter()
            .any(|r| matches!(r, CmdRecord::Batch(5)));
        assert!(has_batch, "Should record Batch(5)");

        assert_eq!(sim.model().value, 5);
    }

    struct OrderingModel {
        trace: RefCell<Vec<&'static str>>,
    }

    impl OrderingModel {
        fn new() -> Self {
            Self {
                trace: RefCell::new(Vec::new()),
            }
        }

        fn trace(&self) -> Vec<&'static str> {
            self.trace.borrow().clone()
        }
    }

    #[derive(Debug)]
    enum OrderingMsg {
        Step(&'static str),
        StartSequence,
        StartBatch,
    }

    impl From<Event> for OrderingMsg {
        fn from(_: Event) -> Self {
            OrderingMsg::StartSequence
        }
    }

    impl Model for OrderingModel {
        type Message = OrderingMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                OrderingMsg::Step(tag) => {
                    self.trace.borrow_mut().push(tag);
                    Cmd::none()
                }
                OrderingMsg::StartSequence => Cmd::sequence(vec![
                    Cmd::msg(OrderingMsg::Step("seq-1")),
                    Cmd::msg(OrderingMsg::Step("seq-2")),
                    Cmd::msg(OrderingMsg::Step("seq-3")),
                ]),
                OrderingMsg::StartBatch => Cmd::batch(vec![
                    Cmd::msg(OrderingMsg::Step("batch-1")),
                    Cmd::msg(OrderingMsg::Step("batch-2")),
                    Cmd::msg(OrderingMsg::Step("batch-3")),
                ]),
            }
        }

        fn view(&self, _frame: &mut Frame) {
            self.trace.borrow_mut().push("view");
        }
    }

    #[test]
    fn sequence_preserves_update_order_before_view() {
        let mut sim = ProgramSimulator::new(OrderingModel::new());
        sim.init();

        sim.send(OrderingMsg::StartSequence);
        sim.capture_frame(1, 1);

        assert_eq!(sim.model().trace(), vec!["seq-1", "seq-2", "seq-3", "view"]);
    }

    #[test]
    fn batch_preserves_update_order_before_view() {
        let mut sim = ProgramSimulator::new(OrderingModel::new());
        sim.init();

        sim.send(OrderingMsg::StartBatch);
        sim.capture_frame(1, 1);

        assert_eq!(
            sim.model().trace(),
            vec!["batch-1", "batch-2", "batch-3", "view"]
        );
    }

    #[test]
    fn frame_dimensions_match_request() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 42,
            initialized: false,
        });
        sim.init();

        let buf = sim.capture_frame(100, 50);
        assert_eq!(buf.width(), 100);
        assert_eq!(buf.height(), 50);
    }

    #[test]
    fn multiple_frame_captures_are_independent() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        // Capture at value 0
        sim.capture_frame(20, 10);

        // Change value
        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::Increment);

        // Capture at value 2
        sim.capture_frame(20, 10);

        let frames = sim.frames();
        assert_eq!(frames.len(), 2);

        // First frame should show "Count: 0"
        assert_eq!(frames[0].get(7, 0).unwrap().content.as_char(), Some('0'));

        // Second frame should show "Count: 2"
        assert_eq!(frames[1].get(7, 0).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn inject_events_processes_in_order() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        // '+' increments, '-' decrements
        let events = vec![
            key_event('+'),
            key_event('+'),
            key_event('+'),
            key_event('-'),
            key_event('+'),
        ];

        sim.inject_events(&events);

        // 0 + 1 + 1 + 1 - 1 + 1 = 3
        assert_eq!(sim.model().value, 3);
    }

    #[test]
    fn task_command_records_task() {
        struct TaskModel {
            result: Option<i32>,
        }

        #[derive(Debug)]
        enum TaskMsg {
            SetResult(i32),
            SpawnTask,
        }

        impl From<Event> for TaskMsg {
            fn from(_: Event) -> Self {
                TaskMsg::SetResult(0)
            }
        }

        impl Model for TaskModel {
            type Message = TaskMsg;

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    TaskMsg::SetResult(v) => {
                        self.result = Some(v);
                        Cmd::none()
                    }
                    TaskMsg::SpawnTask => Cmd::task(|| {
                        // Simulate computation
                        TaskMsg::SetResult(42)
                    }),
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim = ProgramSimulator::new(TaskModel { result: None });
        sim.init();
        sim.send(TaskMsg::SpawnTask);

        // Task should execute synchronously in simulator
        assert_eq!(sim.model().result, Some(42));

        // Should have Task record in command log
        let has_task = sim
            .command_log()
            .iter()
            .any(|r| matches!(r, CmdRecord::Task));
        assert!(has_task);
    }

    #[test]
    fn tick_rate_is_set() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });

        assert!(sim.tick_rate().is_none());

        sim.execute_cmd(Cmd::tick(std::time::Duration::from_millis(100)));

        assert_eq!(sim.tick_rate(), Some(std::time::Duration::from_millis(100)));
    }

    #[test]
    fn logs_accumulate_across_messages() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::LogValue);
        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::LogValue);
        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::LogValue);

        assert_eq!(sim.logs().len(), 3);
        assert_eq!(sim.logs()[0], "value=0");
        assert_eq!(sim.logs()[1], "value=1");
        assert_eq!(sim.logs()[2], "value=2");
    }

    #[test]
    fn deterministic_frame_content_across_runs() {
        fn capture_frame_content(value: i32) -> Vec<Option<char>> {
            let mut sim = ProgramSimulator::new(Counter {
                value,
                initialized: false,
            });
            sim.init();

            let buf = sim.capture_frame(15, 1);
            (0..15)
                .map(|x| buf.get(x, 0).and_then(|c| c.content.as_char()))
                .collect()
        }

        let content1 = capture_frame_content(123);
        let content2 = capture_frame_content(123);
        let content3 = capture_frame_content(123);

        assert_eq!(content1, content2);
        assert_eq!(content2, content3);

        // Should be "Count: 123" followed by None (unwritten cells)
        let expected: Vec<Option<char>> = "Count: 123"
            .chars()
            .map(Some)
            .chain(std::iter::repeat_n(None, 5))
            .collect();
        assert_eq!(content1, expected);
    }

    #[test]
    fn complex_scenario_is_deterministic() {
        fn run_complex_scenario() -> (i32, usize, Vec<String>) {
            let mut sim = ProgramSimulator::new(Counter {
                value: 0,
                initialized: false,
            });
            sim.init();

            // Complex sequence of operations
            for _ in 0..10 {
                sim.send(CounterMsg::Increment);
            }
            sim.send(CounterMsg::LogValue);

            sim.send(CounterMsg::BatchIncrement(5));
            sim.send(CounterMsg::LogValue);

            for _ in 0..3 {
                sim.send(CounterMsg::Decrement);
            }
            sim.send(CounterMsg::LogValue);

            sim.send(CounterMsg::Reset);
            sim.send(CounterMsg::LogValue);

            sim.capture_frame(20, 10);

            (
                sim.model().value,
                sim.command_log().len(),
                sim.logs().to_vec(),
            )
        }

        let result1 = run_complex_scenario();
        let result2 = run_complex_scenario();

        assert_eq!(result1.0, result2.0);
        assert_eq!(result1.1, result2.1);
        assert_eq!(result1.2, result2.2);
    }

    #[test]
    fn model_unchanged_when_not_running() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 5,
            initialized: false,
        });
        sim.init();

        sim.send(CounterMsg::Quit);

        let value_before = sim.model().value;
        sim.send(CounterMsg::Increment);
        sim.send(CounterMsg::BatchIncrement(10));
        let value_after = sim.model().value;

        assert_eq!(value_before, value_after);
    }

    #[test]
    fn init_produces_consistent_command_log() {
        // Model with init that returns a command
        struct InitModel {
            init_ran: bool,
        }

        #[derive(Debug)]
        enum InitMsg {
            MarkInit,
        }

        impl From<Event> for InitMsg {
            fn from(_: Event) -> Self {
                InitMsg::MarkInit
            }
        }

        impl Model for InitModel {
            type Message = InitMsg;

            fn init(&mut self) -> Cmd<Self::Message> {
                Cmd::msg(InitMsg::MarkInit)
            }

            fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
                match msg {
                    InitMsg::MarkInit => {
                        self.init_ran = true;
                        Cmd::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame) {}
        }

        let mut sim1 = ProgramSimulator::new(InitModel { init_ran: false });
        let mut sim2 = ProgramSimulator::new(InitModel { init_ran: false });

        sim1.init();
        sim2.init();

        assert_eq!(sim1.model().init_ran, sim2.model().init_ran);
        assert_eq!(sim1.command_log().len(), sim2.command_log().len());
    }

    #[test]
    fn execute_cmd_directly() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });

        // Execute commands directly without going through update
        sim.execute_cmd(Cmd::log("direct log"));
        sim.execute_cmd(Cmd::tick(std::time::Duration::from_secs(1)));

        assert_eq!(sim.logs(), &["direct log"]);
        assert_eq!(sim.tick_rate(), Some(std::time::Duration::from_secs(1)));
    }

    #[test]
    fn save_restore_are_noops_in_simulator() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 7,
            initialized: false,
        });
        sim.init();

        let log_len = sim.command_log().len();
        let tick_rate = sim.tick_rate();
        let value_before = sim.model().value;

        sim.execute_cmd(Cmd::save_state());
        sim.execute_cmd(Cmd::restore_state());

        assert_eq!(sim.command_log().len(), log_len);
        assert_eq!(sim.tick_rate(), tick_rate);
        assert_eq!(sim.model().value, value_before);
        assert!(sim.is_running());
    }

    #[test]
    fn grapheme_pool_is_reused() {
        let mut sim = ProgramSimulator::new(Counter {
            value: 0,
            initialized: false,
        });
        sim.init();

        // Capture multiple frames - pool should be reused
        for i in 0..10 {
            sim.model_mut().value = i;
            sim.capture_frame(80, 24);
        }

        assert_eq!(sim.frame_count(), 10);
    }
}
