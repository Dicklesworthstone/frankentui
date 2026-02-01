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

use crate::input_macro::{EventRecorder, InputMacro};
use crate::subscription::SubscriptionManager;
use crate::terminal_writer::{ScreenMode, TerminalWriter, UiAnchor};
use ftui_core::event::Event;
use ftui_core::terminal_capabilities::TerminalCapabilities;
use ftui_core::terminal_session::{SessionOptions, TerminalSession};
use ftui_render::budget::{FrameBudgetConfig, RenderBudget};
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_render::sanitize::sanitize;
use std::io::{self, Stdout, Write};
use std::time::{Duration, Instant};
use tracing::{debug, debug_span, info, info_span};

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

    /// Declare active subscriptions.
    ///
    /// Called after each `update()`. The runtime compares the returned set
    /// (by `SubId`) against currently running subscriptions and starts/stops
    /// as needed. Returning an empty vec stops all subscriptions.
    ///
    /// # Default
    ///
    /// Returns an empty vec (no subscriptions).
    fn subscriptions(&self) -> Vec<Box<dyn crate::subscription::Subscription<Self::Message>>> {
        vec![]
    }
}

/// Commands represent side effects to be executed by the runtime.
///
/// Commands are returned from `init()` and `update()` to trigger
/// actions like quitting, sending messages, or scheduling ticks.
#[derive(Default)]
pub enum Cmd<M> {
    /// No operation.
    #[default]
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
    /// Execute a blocking operation on a background thread.
    ///
    /// The closure runs on a spawned thread and its return value
    /// is sent back as a message to the model.
    Task(Box<dyn FnOnce() -> M + Send>),
}

impl<M: std::fmt::Debug> std::fmt::Debug for Cmd<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::Quit => write!(f, "Quit"),
            Self::Batch(cmds) => f.debug_tuple("Batch").field(cmds).finish(),
            Self::Sequence(cmds) => f.debug_tuple("Sequence").field(cmds).finish(),
            Self::Msg(m) => f.debug_tuple("Msg").field(m).finish(),
            Self::Tick(d) => f.debug_tuple("Tick").field(d).finish(),
            Self::Log(s) => f.debug_tuple("Log").field(s).finish(),
            Self::Task(_) => write!(f, "Task(...)"),
        }
    }
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

    /// Create a background task command.
    ///
    /// The closure runs on a spawned thread. When it completes,
    /// the returned message is sent back to the model's `update()`.
    pub fn task<F>(f: F) -> Self
    where
        F: FnOnce() -> M + Send + 'static,
    {
        Self::Task(Box::new(f))
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
    /// Debounce duration for resize events.
    pub resize_debounce: Duration,
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
            resize_debounce: Duration::from_millis(100),
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

    /// Set the resize debounce duration.
    pub fn with_resize_debounce(mut self, debounce: Duration) -> Self {
        self.resize_debounce = debounce;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeAction {
    None,
    ShowPlaceholder,
    ApplyResize {
        width: u16,
        height: u16,
        elapsed: Duration,
    },
}

#[derive(Debug)]
struct ResizeDebouncer {
    debounce: Duration,
    last_resize: Option<Instant>,
    pending_size: Option<(u16, u16)>,
    last_applied: (u16, u16),
}

impl ResizeDebouncer {
    fn new(debounce: Duration, initial_size: (u16, u16)) -> Self {
        Self {
            debounce,
            last_resize: None,
            pending_size: None,
            last_applied: initial_size,
        }
    }

    fn handle_resize(&mut self, width: u16, height: u16) -> ResizeAction {
        self.handle_resize_at(width, height, Instant::now())
    }

    fn handle_resize_at(&mut self, width: u16, height: u16, now: Instant) -> ResizeAction {
        if self.pending_size.is_none() && (width, height) == self.last_applied {
            return ResizeAction::None;
        }
        self.pending_size = Some((width, height));
        self.last_resize = Some(now);
        ResizeAction::ShowPlaceholder
    }

    fn tick(&mut self) -> ResizeAction {
        self.tick_at(Instant::now())
    }

    fn tick_at(&mut self, now: Instant) -> ResizeAction {
        let Some(pending) = self.pending_size else {
            return ResizeAction::None;
        };
        let Some(last) = self.last_resize else {
            return ResizeAction::None;
        };

        let elapsed = now.saturating_duration_since(last);
        if elapsed >= self.debounce {
            self.pending_size = None;
            self.last_resize = None;
            self.last_applied = pending;
            return ResizeAction::ApplyResize {
                width: pending.0,
                height: pending.1,
                elapsed,
            };
        }

        ResizeAction::None
    }

    fn time_until_apply(&self, now: Instant) -> Option<Duration> {
        let _pending = self.pending_size?;
        let last = self.last_resize?;
        let elapsed = now.saturating_duration_since(last);
        Some(self.debounce.saturating_sub(elapsed))
    }
}

/// The program runtime that manages the update/view loop.
pub struct Program<M: Model, W: Write = Stdout> {
    /// The application model.
    model: M,
    /// Terminal output coordinator.
    writer: TerminalWriter<W>,
    /// Terminal lifecycle guard (raw mode, mouse, paste, focus).
    session: TerminalSession,
    /// Whether the program is running.
    running: bool,
    /// Current tick rate (if any).
    tick_rate: Option<Duration>,
    /// Last tick time.
    last_tick: Instant,
    /// Whether the UI needs to be redrawn.
    dirty: bool,
    /// Current terminal width.
    width: u16,
    /// Current terminal height.
    height: u16,
    /// Poll timeout when no tick is scheduled.
    poll_timeout: Duration,
    /// Frame budget configuration.
    budget: RenderBudget,
    /// Resize debouncer for rapid resize events.
    resize_debouncer: ResizeDebouncer,
    /// Whether the resize placeholder should be shown.
    resizing: bool,
    /// Optional event recorder for macro capture.
    event_recorder: Option<EventRecorder>,
    /// Subscription lifecycle manager.
    subscriptions: SubscriptionManager<M::Message>,
    /// Channel for receiving messages from background tasks.
    task_sender: std::sync::mpsc::Sender<M::Message>,
    /// Channel for receiving messages from background tasks.
    task_receiver: std::sync::mpsc::Receiver<M::Message>,
}

impl<M: Model> Program<M, Stdout> {
    /// Create a new program with default configuration.
    pub fn new(model: M) -> io::Result<Self> {
        Self::with_config(model, ProgramConfig::default())
    }

    /// Create a new program with the specified configuration.
    pub fn with_config(model: M, config: ProgramConfig) -> io::Result<Self> {
        let capabilities = TerminalCapabilities::detect();
        let session = TerminalSession::new(SessionOptions {
            alternate_screen: matches!(config.screen_mode, ScreenMode::AltScreen),
            mouse_capture: config.mouse,
            bracketed_paste: config.bracketed_paste,
            focus_events: config.focus_reporting,
            kitty_keyboard: false,
        })?;

        let mut writer = TerminalWriter::new(
            io::stdout(),
            config.screen_mode,
            config.ui_anchor,
            capabilities,
        );

        // Get terminal size for initial frame
        let (width, height) = session.size().unwrap_or((80, 24));
        writer.set_size(width, height);

        let budget = RenderBudget::from_config(&config.budget);
        let resize_debouncer = ResizeDebouncer::new(config.resize_debounce, (width, height));
        let subscriptions = SubscriptionManager::new();
        let (task_sender, task_receiver) = std::sync::mpsc::channel();

        Ok(Self {
            model,
            writer,
            session,
            running: true,
            tick_rate: None,
            last_tick: Instant::now(),
            dirty: true,
            width,
            height,
            poll_timeout: config.poll_timeout,
            budget,
            resize_debouncer,
            resizing: false,
            event_recorder: None,
            subscriptions,
            task_sender,
            task_receiver,
        })
    }
}

impl<M: Model, W: Write> Program<M, W> {
    /// Run the main event loop.
    ///
    /// This is the main entry point. It handles:
    /// 1. Initialization (terminal setup, raw mode)
    /// 2. Event polling and message dispatch
    /// 3. Frame rendering
    /// 4. Shutdown (terminal cleanup)
    pub fn run(&mut self) -> io::Result<()> {
        self.run_event_loop()
    }

    /// The inner event loop, separated for proper cleanup handling.
    fn run_event_loop(&mut self) -> io::Result<()> {
        // Initialize
        let cmd = self.model.init();
        self.execute_cmd(cmd)?;

        // Reconcile initial subscriptions
        self.reconcile_subscriptions();

        // Initial render
        self.render_frame()?;

        // Main loop
        while self.running {
            // Poll for input with tick timeout
            let timeout = self.effective_timeout();

            // Poll for events with timeout
            if self.session.poll_event(timeout)? {
                // Drain all pending events
                loop {
                    // read_event returns Option<Event> after converting from crossterm
                    if let Some(event) = self.session.read_event()? {
                        self.handle_event(event)?;
                    }
                    if !self.session.poll_event(Duration::from_millis(0))? {
                        break;
                    }
                }
            }

            // Process subscription messages
            self.process_subscription_messages()?;

            // Process background task results
            self.process_task_results()?;

            self.process_resize_debounce()?;

            // Check for tick
            if self.should_tick() {
                self.dirty = true;
            }

            // Render if dirty
            if self.dirty {
                self.render_frame()?;
            }
        }

        // Stop all subscriptions on exit
        self.subscriptions.stop_all();

        Ok(())
    }

    fn handle_event(&mut self, event: Event) -> io::Result<()> {
        // Record event before processing (no-op when recorder is None or idle).
        if let Some(recorder) = &mut self.event_recorder {
            recorder.record(&event);
        }

        let event = match event {
            Event::Resize { width, height } => {
                debug!(width, height, "Resize event received, debouncing");
                let action = self.resize_debouncer.handle_resize(width, height);
                if matches!(action, ResizeAction::ShowPlaceholder) {
                    let was_resizing = self.resizing;
                    self.resizing = true;
                    if !was_resizing {
                        debug!("Showing resize placeholder");
                    }
                    self.width = width;
                    self.height = height;
                    self.writer.set_size(width, height);
                    self.dirty = true;
                }
                return Ok(());
            }
            other => other,
        };

        let msg = M::Message::from(event);
        let cmd = self.model.update(msg);
        self.dirty = true;
        self.execute_cmd(cmd)?;
        self.reconcile_subscriptions();
        Ok(())
    }

    /// Reconcile the model's declared subscriptions with running ones.
    fn reconcile_subscriptions(&mut self) {
        let subs = self.model.subscriptions();
        self.subscriptions.reconcile(subs);
    }

    /// Process pending messages from subscriptions.
    fn process_subscription_messages(&mut self) -> io::Result<()> {
        let messages = self.subscriptions.drain_messages();
        for msg in messages {
            let cmd = self.model.update(msg);
            self.dirty = true;
            self.execute_cmd(cmd)?;
        }
        if self.dirty {
            self.reconcile_subscriptions();
        }
        Ok(())
    }

    /// Process results from background tasks.
    fn process_task_results(&mut self) -> io::Result<()> {
        while let Ok(msg) = self.task_receiver.try_recv() {
            let cmd = self.model.update(msg);
            self.dirty = true;
            self.execute_cmd(cmd)?;
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
                // TODO: Batch is documented as "parallel" but currently executes
                // sequentially. True parallel execution would require async or
                // threading infrastructure. For now, Batch and Sequence have
                // identical behavior. This is acceptable for synchronous Cmd
                // variants (Msg, Quit, Log) but should be revisited if async
                // commands (e.g., IO tasks) are added.
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
                let sanitized = sanitize(&text);
                if sanitized.ends_with('\n') {
                    self.writer.write_log(&sanitized)?;
                } else {
                    let mut owned = sanitized.into_owned();
                    owned.push('\n');
                    self.writer.write_log(&owned)?;
                }
            }
            Cmd::Task(f) => {
                let sender = self.task_sender.clone();
                std::thread::spawn(move || {
                    let msg = f();
                    let _ = sender.send(msg);
                });
            }
        }
        Ok(())
    }

    /// Render a frame with budget tracking.
    fn render_frame(&mut self) -> io::Result<()> {
        let _frame_span =
            info_span!("render_frame", width = self.width, height = self.height).entered();

        // Reset budget for new frame, potentially upgrading quality
        self.budget.next_frame();

        if self.resizing {
            self.render_resize_placeholder()?;
            self.dirty = false;
            return Ok(());
        }

        // Early skip if budget says to skip this frame entirely
        if self.budget.exhausted() {
            debug!(
                degradation = self.budget.degradation().as_str(),
                "frame skipped: budget exhausted before render"
            );
            self.dirty = false;
            return Ok(());
        }

        // Create new frame with current degradation level
        // Note: Frame borrows the pool from writer, so we must drop frame
        // before calling present_ui (which also borrows writer).
        let mut frame = Frame::new(self.width, self.height, self.writer.pool_mut());
        frame.set_degradation(self.budget.degradation());

        // --- Render phase ---
        let render_start = Instant::now();
        {
            let _view_span = debug_span!("model_view").entered();
            self.model.view(&mut frame);
        }
        let render_elapsed = render_start.elapsed();

        // Check if render phase overspent its budget
        let render_budget = self.budget.phase_budgets().render;
        if render_elapsed > render_budget {
            debug!(
                render_ms = render_elapsed.as_millis() as u32,
                budget_ms = render_budget.as_millis() as u32,
                "render phase exceeded budget"
            );
            // Trigger degradation if we're consistently over budget
            if self.budget.should_degrade(render_budget) {
                self.budget.degrade();
            }
        }

        // --- Present phase ---
        if !self.budget.exhausted() {
            // Extract buffer to release borrow on pool
            let buffer = frame.buffer;

            let present_start = Instant::now();
            {
                let _present_span = debug_span!("frame_present").entered();
                self.writer.present_ui(&buffer)?;
            }
            let present_elapsed = present_start.elapsed();

            let present_budget = self.budget.phase_budgets().present;
            if present_elapsed > present_budget {
                debug!(
                    present_ms = present_elapsed.as_millis() as u32,
                    budget_ms = present_budget.as_millis() as u32,
                    "present phase exceeded budget"
                );
            }
        } else {
            debug!(
                degradation = self.budget.degradation().as_str(),
                elapsed_ms = self.budget.elapsed().as_millis() as u32,
                "frame present skipped: budget exhausted after render"
            );
        }

        self.dirty = false;

        Ok(())
    }

    /// Calculate the effective poll timeout.
    fn effective_timeout(&self) -> Duration {
        if let Some(tick_rate) = self.tick_rate {
            let elapsed = self.last_tick.elapsed();
            let mut timeout = tick_rate.saturating_sub(elapsed);
            if let Some(resize_timeout) = self.resize_debouncer.time_until_apply(Instant::now()) {
                timeout = timeout.min(resize_timeout);
            }
            timeout
        } else {
            let mut timeout = self.poll_timeout;
            if let Some(resize_timeout) = self.resize_debouncer.time_until_apply(Instant::now()) {
                timeout = timeout.min(resize_timeout);
            }
            timeout
        }
    }

    /// Check if we should send a tick.
    fn should_tick(&mut self) -> bool {
        if let Some(tick_rate) = self.tick_rate
            && self.last_tick.elapsed() >= tick_rate
        {
            self.last_tick = Instant::now();
            return true;
        }
        false
    }

    fn process_resize_debounce(&mut self) -> io::Result<()> {
        match self.resize_debouncer.tick() {
            ResizeAction::ApplyResize {
                width,
                height,
                elapsed,
            } => self.apply_resize(width, height, elapsed),
            _ => Ok(()),
        }
    }

    fn apply_resize(&mut self, width: u16, height: u16, elapsed: Duration) -> io::Result<()> {
        self.resizing = false;
        self.width = width;
        self.height = height;
        self.writer.set_size(width, height);
        info!(
            width = width,
            height = height,
            debounce_ms = elapsed.as_millis() as u64,
            "Resize applied"
        );

        let msg = M::Message::from(Event::Resize { width, height });
        let cmd = self.model.update(msg);
        self.dirty = true;
        self.execute_cmd(cmd)
    }

    fn render_resize_placeholder(&mut self) -> io::Result<()> {
        const PLACEHOLDER_TEXT: &str = "Resizing...";

        let mut frame = Frame::new(self.width, self.height, self.writer.pool_mut());
        let text_width = PLACEHOLDER_TEXT.chars().count().min(self.width as usize) as u16;
        let x_start = self.width.saturating_sub(text_width) / 2;
        let y = self.height / 2;

        for (offset, ch) in PLACEHOLDER_TEXT.chars().enumerate() {
            let x = x_start.saturating_add(offset as u16);
            if x >= self.width {
                break;
            }
            frame.buffer.set_raw(x, y, Cell::from_char(ch));
        }

        let buffer = frame.buffer;
        self.writer.present_ui(&buffer)?;

        Ok(())
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

    /// Start recording events into a macro.
    ///
    /// If already recording, the current recording is discarded and a new one starts.
    /// The current terminal size is captured as metadata.
    pub fn start_recording(&mut self, name: impl Into<String>) {
        let mut recorder = EventRecorder::new(name).with_terminal_size(self.width, self.height);
        recorder.start();
        self.event_recorder = Some(recorder);
    }

    /// Stop recording and return the recorded macro, if any.
    ///
    /// Returns `None` if not currently recording.
    pub fn stop_recording(&mut self) -> Option<InputMacro> {
        self.event_recorder.take().map(EventRecorder::finish)
    }

    /// Check if event recording is active.
    pub fn is_recording(&self) -> bool {
        self.event_recorder
            .as_ref()
            .is_some_and(EventRecorder::is_recording)
    }
}

/// Builder for creating and running programs.
pub struct App;

impl App {
    /// Create a new app builder with the given model.
    #[allow(clippy::new_ret_no_self)] // App is a namespace for builder methods
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

    /// Create a fullscreen app from a [`StringModel`](crate::string_model::StringModel).
    ///
    /// This wraps the string model in a [`StringModelAdapter`](crate::string_model::StringModelAdapter)
    /// so that `view_string()` output is rendered through the standard kernel pipeline.
    pub fn string_model<S: crate::string_model::StringModel>(
        model: S,
    ) -> AppBuilder<crate::string_model::StringModelAdapter<S>> {
        AppBuilder {
            model: crate::string_model::StringModelAdapter::new(model),
            config: ProgramConfig::fullscreen(),
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

    /// Set the resize debounce duration.
    pub fn resize_debounce(mut self, debounce: Duration) -> Self {
        self.config.resize_debounce = debounce;
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
    fn cmd_task() {
        let cmd: Cmd<TestMsg> = Cmd::task(|| TestMsg::Increment);
        assert!(matches!(cmd, Cmd::Task(_)));
    }

    #[test]
    fn cmd_debug_format() {
        let cmd: Cmd<TestMsg> = Cmd::task(|| TestMsg::Increment);
        let debug = format!("{cmd:?}");
        assert_eq!(debug, "Task(...)");
    }

    #[test]
    fn model_subscriptions_default_empty() {
        let model = TestModel { value: 0 };
        let subs = model.subscriptions();
        assert!(subs.is_empty());
    }

    #[test]
    fn program_config_default() {
        let config = ProgramConfig::default();
        assert!(matches!(config.screen_mode, ScreenMode::Inline { .. }));
        assert!(!config.mouse);
        assert!(config.bracketed_paste);
        assert_eq!(config.resize_debounce, Duration::from_millis(100));
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
        assert!(matches!(model.update(TestMsg::Quit), Cmd::Quit));
    }

    #[test]
    fn model_init_default() {
        let mut model = TestModel { value: 0 };
        let cmd = model.init();
        assert!(matches!(cmd, Cmd::None));
    }

    #[test]
    fn resize_debouncer_applies_after_delay() {
        let mut debouncer = ResizeDebouncer::new(Duration::from_millis(100), (80, 24));
        let now = Instant::now();

        assert!(matches!(
            debouncer.handle_resize_at(100, 40, now),
            ResizeAction::ShowPlaceholder
        ));

        assert!(matches!(
            debouncer.tick_at(now + Duration::from_millis(50)),
            ResizeAction::None
        ));

        assert!(matches!(
            debouncer.tick_at(now + Duration::from_millis(120)),
            ResizeAction::ApplyResize {
                width: 100,
                height: 40,
                ..
            }
        ));
    }

    #[test]
    fn resize_debouncer_uses_latest_size() {
        let mut debouncer = ResizeDebouncer::new(Duration::from_millis(100), (80, 24));
        let now = Instant::now();

        debouncer.handle_resize_at(100, 40, now);
        debouncer.handle_resize_at(120, 50, now + Duration::from_millis(10));

        assert!(matches!(
            debouncer.tick_at(now + Duration::from_millis(120)),
            ResizeAction::ApplyResize {
                width: 120,
                height: 50,
                ..
            }
        ));
    }
}
