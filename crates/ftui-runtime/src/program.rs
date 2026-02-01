#![forbid(unsafe_code)]

//! Bubbletea/Elm-style runtime for terminal applications.
//!
//! The program runtime manages the update/view loop, handling events and
//! rendering frames. It separates state (Model) from rendering (View) and
//! provides a command pattern for side effects.
//!
//! # Example
//!
//! ```ignore
//! use ftui_runtime::program::{Model, Cmd};
//! use ftui_core::event::Event;
//! use ftui_render::frame::Frame;
//!
//! struct Counter {
//!     count: i32,
//! }
//!
//! enum Msg {
//!     Increment,
//!     Decrement,
//!     Quit,
//! }
//!
//! impl From<Event> for Msg {
//!     fn from(event: Event) -> Self {
//!         match event {
//!             Event::Key(k) if k.is_char('q') => Msg::Quit,
//!             Event::Key(k) if k.is_char('+') => Msg::Increment,
//!             Event::Key(k) if k.is_char('-') => Msg::Decrement,
//!             _ => Msg::Increment, // Default
//!         }
//!     }
//! }
//!
//! impl Model for Counter {
//!     type Message = Msg;
//!
//!     fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
//!         match msg {
//!             Msg::Increment => { self.count += 1; Cmd::none() }
//!             Msg::Decrement => { self.count -= 1; Cmd::none() }
//!             Msg::Quit => Cmd::quit(),
//!         }
//!     }
//!
//!     fn view(&self, frame: &mut Frame) {
//!         // Render counter value to frame
//!     }
//! }
//! ```

use crate::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
use ftui_core::event::Event;
use ftui_core::input_parser::InputParser;
use ftui_render::budget::{FrameBudgetConfig, RenderBudget};
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::sanitize::sanitize;
use std::io::{self, Stdout, Write};
use std::time::{Duration, Instant};

/// The Model trait defines application state and behavior.
///
/// Implementations define how the application responds to events
/// and renders its current state.
pub trait Model: Sized {
    /// The message type for this model.
    ///
    /// Messages represent actions that update the model state.
    /// Must be convertible from terminal events.
    type Message: From<Event> + Send + 'static;

    /// Initialize the model with startup commands.
    ///
    /// Called once when the program starts. Return commands to execute
    /// initial side effects like loading data.
    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::none()
    }

    /// Update the model in response to a message.
    ///
    /// This is the core state transition function. Returns commands
    /// for any side effects that should be executed.
    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message>;

    /// Render the current state to a frame.
    ///
    /// Called after updates when the UI needs to be redrawn.
    fn view(&self, frame: &mut Frame);
}

/// Commands represent side effects to be executed by the runtime.
///
/// Commands are returned from `init()` and `update()` to trigger
/// actions like quitting, sending messages, or scheduling ticks.
#[derive(Debug)]
pub enum Cmd<M> {
    /// No operation.
    None,
    /// Quit the application.
    Quit,
    /// Execute multiple commands in parallel.
    Batch(Vec<Cmd<M>>),
    /// Execute commands sequentially.
    Sequence(Vec<Cmd<M>>),
    /// Send a message to the model.
    Msg(M),
    /// Schedule a tick after a duration.
    Tick(Duration),
    /// Write a log message to the terminal output.
    ///
    /// This writes to the scrollback region in inline mode, or is ignored/handled
    /// appropriately in alternate screen mode. Safe to use with the One-Writer Rule.
    Log(String),
}

impl<M> Cmd<M> {
    /// Create a no-op command.
    #[inline]
    pub fn none() -> Self {
        Self::None
    }

    /// Create a quit command.
    #[inline]
    pub fn quit() -> Self {
        Self::Quit
    }

    /// Create a message command.
    #[inline]
    pub fn msg(m: M) -> Self {
        Self::Msg(m)
    }

    /// Create a log command.
    ///
    /// The message will be sanitized and written to the terminal log (scrollback).
    /// A newline is appended if not present.
    #[inline]
    pub fn log(msg: impl Into<String>) -> Self {
        Self::Log(msg.into())
    }

    /// Create a batch of parallel commands.
    pub fn batch(cmds: Vec<Self>) -> Self {
        if cmds.is_empty() {
            Self::None
        } else if cmds.len() == 1 {
            cmds.into_iter().next().unwrap()
        } else {
            Self::Batch(cmds)
        }
    }

    /// Create a sequence of commands.
    pub fn sequence(cmds: Vec<Self>) -> Self {
        if cmds.is_empty() {
            Self::None
        } else if cmds.len() == 1 {
            cmds.into_iter().next().unwrap()
        } else {
            Self::Sequence(cmds)
        }
    }

    /// Create a tick command.
    #[inline]
    pub fn tick(duration: Duration) -> Self {
        Self::Tick(duration)
    }
}

impl<M> Default for Cmd<M> {
    fn default() -> Self {
        Self::None
    }
}

/// Configuration for the program runtime.
#[derive(Debug, Clone)]
pub struct ProgramConfig {
    /// Screen mode (inline or alternate screen).
    pub screen_mode: ScreenMode,
    /// UI anchor for inline mode.
    pub ui_anchor: UiAnchor,
    /// Frame budget configuration.
    pub budget: FrameBudgetConfig,
    /// Input poll timeout.
    pub poll_timeout: Duration,
    /// Enable mouse support.
    pub mouse: bool,
    /// Enable bracketed paste.
    pub bracketed_paste: bool,
    /// Enable focus reporting.
    pub focus_reporting: bool,
}

impl Default for ProgramConfig {
    fn default() -> Self {
        Self {
            screen_mode: ScreenMode::Inline { ui_height: 4 },
            ui_anchor: UiAnchor::Bottom,
            budget: FrameBudgetConfig::default(),
            poll_timeout: Duration::from_millis(100),
            mouse: false,
            bracketed_paste: true,
            focus_reporting: false,
        }
    }
}

impl ProgramConfig {
    /// Create config for fullscreen applications.
    pub fn fullscreen() -> Self {
        Self {
            screen_mode: ScreenMode::AltScreen,
            ..Default::default()
        }
    }

    /// Create config for inline mode with specified height.
    pub fn inline(height: u16) -> Self {
        Self {
            screen_mode: ScreenMode::Inline { ui_height: height },
            ..Default::default()
        }
    }

    /// Enable mouse support.
    pub fn with_mouse(mut self) -> Self {
        self.mouse = true;
        self
    }

    /// Set the budget configuration.
    pub fn with_budget(mut self, budget: FrameBudgetConfig) -> Self {
        self.budget = budget;
        self
    }
}

/// The program runtime that manages the update/view loop.
pub struct Program<M: Model, W: Write = Stdout> {
    /// The application model.
    model: M,
    /// Terminal output coordinator.
    writer: TerminalWriter<W>,
    /// Input parser for terminal events.
    input_parser: InputParser,
    /// Whether the program is running.
    running: bool,
    /// Current tick rate (if any).
    tick_rate: Option<Duration>,
    /// Last tick time.
    last_tick: Instant,
    /// Whether the UI needs to be redrawn.
    dirty: bool,
    /// Previous frame buffer for diffing.
    prev_frame: Frame,
    /// Frame budget configuration.
    budget_config: FrameBudgetConfig,
    /// Current render budget (persists across frames for degradation tracking).
    budget: RenderBudget,
}

impl<M: Model> Program<M, Stdout> {
    /// Create a new program with default configuration.
    pub fn new(model: M) -> io::Result<Self> {
        Self::with_config(model, ProgramConfig::default())
    }

    /// Create a new program with the specified configuration.
    pub fn with_config(model: M, config: ProgramConfig) -> io::Result<Self> {
        let writer = TerminalWriter::new(
            io::stdout(),
            config.screen_mode,
            config.ui_anchor,
            ftui_render::presenter::TerminalCapabilities::basic(),
        );

        // Get terminal size for initial frame
        let (width, height) = (80, 24); // TODO: query terminal

        let budget = RenderBudget::from_config(&config.budget);

        Ok(Self {
            model,
            writer,
            input_parser: InputParser::new(),
            running: true,
            tick_rate: None,
            last_tick: Instant::now(),
            dirty: true,
            prev_frame: Frame::new(width, height),
            budget_config: config.budget,
            budget,
        })
    }
}

impl<M: Model, W: Write> Program<M, W> {
    /// Run the main event loop.
    ///
    /// This is the main entry point. It handles:
    /// 1. Initialization
    /// 2. Event polling and message dispatch
    /// 3. Frame rendering
    /// 4. Shutdown
    pub fn run(&mut self) -> io::Result<()> {
        // Initialize
        let cmd = self.model.init();
        self.execute_cmd(cmd)?;

        // Initial render
        self.render_frame()?;

        // Main loop
        while self.running {
            // Poll for input with tick timeout
            let timeout = self.effective_timeout();

            // TODO: actual input polling with non-blocking read
            // For now, just sleep and render
            std::thread::sleep(timeout);

            // Check for tick
            if self.should_tick() {
                self.dirty = true;
            }

            // Render if dirty
            if self.dirty {
                self.render_frame()?;
            }
        }

        Ok(())
    }

    /// Execute a command.
    fn execute_cmd(&mut self, cmd: Cmd<M::Message>) -> io::Result<()> {
        match cmd {
            Cmd::None => {}
            Cmd::Quit => self.running = false,
            Cmd::Msg(m) => {
                let cmd = self.model.update(m);
                self.dirty = true;
                self.execute_cmd(cmd)?;
            }
            Cmd::Batch(cmds) => {
                for c in cmds {
                    self.execute_cmd(c)?;
                }
            }
            Cmd::Sequence(cmds) => {
                for c in cmds {
                    self.execute_cmd(c)?;
                }
            }
            Cmd::Tick(duration) => {
                self.tick_rate = Some(duration);
                self.last_tick = Instant::now();
            }
            Cmd::Log(text) => {
                self.writer.write_log(&text)?;
            }
        }
        Ok(())
    }

    /// Render a frame with budget tracking.
    fn render_frame(&mut self) -> io::Result<()> {
        // Reset budget for new frame, potentially upgrading quality
        self.budget.next_frame();

        // Create new frame
        let mut frame = Frame::new(self.prev_frame.width(), self.prev_frame.height());

        // Let model render to frame
        self.model.view(&mut frame);

        // Check budget before presenting
        if !self.budget.exhausted() {
            // Present the frame
            // Note: present_ui internally handles diffing
            self.writer.present_ui(&frame.buffer)?;
        }

        // Store frame for next diff
        self.prev_frame = frame;
        self.dirty = false;

        Ok(())
    }

    /// Calculate the effective poll timeout.
    fn effective_timeout(&self) -> Duration {
        if let Some(tick_rate) = self.tick_rate {
            let elapsed = self.last_tick.elapsed();
            tick_rate.saturating_sub(elapsed)
        } else {
            Duration::from_millis(100)
        }
    }

    /// Check if we should send a tick.
    fn should_tick(&mut self) -> bool {
        if let Some(tick_rate) = self.tick_rate {
            if self.last_tick.elapsed() >= tick_rate {
                self.last_tick = Instant::now();
                return true;
            }
        }
        false
    }

    /// Get a reference to the model.
    pub fn model(&self) -> &M {
        &self.model
    }

    /// Get a mutable reference to the model.
    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// Check if the program is running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Request a quit.
    pub fn quit(&mut self) {
        self.running = false;
    }

    /// Mark the UI as needing redraw.
    pub fn request_redraw(&mut self) {
        self.dirty = true;
    }
}

/// Builder for creating and running programs.
pub struct App;

impl App {
    /// Create a new app builder with the given model.
    pub fn new<M: Model>(model: M) -> AppBuilder<M> {
        AppBuilder {
            model,
            config: ProgramConfig::default(),
        }
    }

    /// Create a fullscreen app.
    pub fn fullscreen<M: Model>(model: M) -> AppBuilder<M> {
        AppBuilder {
            model,
            config: ProgramConfig::fullscreen(),
        }
    }

    /// Create an inline app with the given height.
    pub fn inline<M: Model>(model: M, height: u16) -> AppBuilder<M> {
        AppBuilder {
            model,
            config: ProgramConfig::inline(height),
        }
    }
}

/// Builder for configuring and running programs.
pub struct AppBuilder<M: Model> {
    model: M,
    config: ProgramConfig,
}

impl<M: Model> AppBuilder<M> {
    /// Set the screen mode.
    pub fn screen_mode(mut self, mode: ScreenMode) -> Self {
        self.config.screen_mode = mode;
        self
    }

    /// Set the UI anchor.
    pub fn anchor(mut self, anchor: UiAnchor) -> Self {
        self.config.ui_anchor = anchor;
        self
    }

    /// Enable mouse support.
    pub fn with_mouse(mut self) -> Self {
        self.config.mouse = true;
        self
    }

    /// Set the frame budget configuration.
    pub fn with_budget(mut self, budget: FrameBudgetConfig) -> Self {
        self.config.budget = budget;
        self
    }

    /// Run the application.
    pub fn run(self) -> io::Result<()> {
        let mut program = Program::with_config(self.model, self.config)?;
        program.run()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Simple test model
    struct TestModel {
        value: i32,
    }

    #[derive(Debug)]
    enum TestMsg {
        Increment,
        Decrement,
        Quit,
    }

    impl From<Event> for TestMsg {
        fn from(_event: Event) -> Self {
            TestMsg::Increment
        }
    }

    impl Model for TestModel {
        type Message = TestMsg;

        fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
            match msg {
                TestMsg::Increment => {
                    self.value += 1;
                    Cmd::none()
                }
                TestMsg::Decrement => {
                    self.value -= 1;
                    Cmd::none()
                }
                TestMsg::Quit => Cmd::quit(),
            }
        }

        fn view(&self, _frame: &mut Frame) {
            // No-op for tests
        }
    }

    #[test]
    fn cmd_none() {
        let cmd: Cmd<TestMsg> = Cmd::none();
        assert!(matches!(cmd, Cmd::None));
    }

    #[test]
    fn cmd_quit() {
        let cmd: Cmd<TestMsg> = Cmd::quit();
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn cmd_msg() {
        let cmd: Cmd<TestMsg> = Cmd::msg(TestMsg::Increment);
        assert!(matches!(cmd, Cmd::Msg(TestMsg::Increment)));
    }

    #[test]
    fn cmd_batch_empty() {
        let cmd: Cmd<TestMsg> = Cmd::batch(vec![]);
        assert!(matches!(cmd, Cmd::None));
    }

    #[test]
    fn cmd_batch_single() {
        let cmd: Cmd<TestMsg> = Cmd::batch(vec![Cmd::quit()]);
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn cmd_batch_multiple() {
        let cmd: Cmd<TestMsg> = Cmd::batch(vec![Cmd::none(), Cmd::quit()]);
        assert!(matches!(cmd, Cmd::Batch(_)));
    }

    #[test]
    fn cmd_sequence_empty() {
        let cmd: Cmd<TestMsg> = Cmd::sequence(vec![]);
        assert!(matches!(cmd, Cmd::None));
    }

    #[test]
    fn cmd_tick() {
        let cmd: Cmd<TestMsg> = Cmd::tick(Duration::from_millis(100));
        assert!(matches!(cmd, Cmd::Tick(_)));
    }

    #[test]
    fn program_config_default() {
        let config = ProgramConfig::default();
        assert!(matches!(config.screen_mode, ScreenMode::Inline { .. }));
        assert!(!config.mouse);
        assert!(config.bracketed_paste);
    }

    #[test]
    fn program_config_fullscreen() {
        let config = ProgramConfig::fullscreen();
        assert!(matches!(config.screen_mode, ScreenMode::AltScreen));
    }

    #[test]
    fn program_config_inline() {
        let config = ProgramConfig::inline(10);
        assert!(matches!(
            config.screen_mode,
            ScreenMode::Inline { ui_height: 10 }
        ));
    }

    #[test]
    fn program_config_with_mouse() {
        let config = ProgramConfig::default().with_mouse();
        assert!(config.mouse);
    }

    #[test]
    fn model_update() {
        let mut model = TestModel { value: 0 };
        model.update(TestMsg::Increment);
        assert_eq!(model.value, 1);
        model.update(TestMsg::Decrement);
        assert_eq!(model.value, 0);
    }

    #[test]
    fn model_init_default() {
        let mut model = TestModel { value: 0 };
        let cmd = model.init();
        assert!(matches!(cmd, Cmd::None));
    }
}
