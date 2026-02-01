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
use ftui_core::event::Event;
use ftui_render::buffer::Buffer;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
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
        }
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
                }
            }
            Cmd::Sequence(cmds) => {
                let count = cmds.len();
                self.command_log.push(CmdRecord::Sequence(count));
                for c in cmds {
                    self.execute_cmd(c);
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
            Cmd::Task(f) => {
                self.command_log.push(CmdRecord::Task);
                let msg = f();
                let cmd = self.model.update(msg);
                self.execute_cmd(cmd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

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
}
