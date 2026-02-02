#![forbid(unsafe_code)]

//! Agent Harness Reference Application
//!
//! This is the PRIMARY reference application for FrankenTUI, demonstrating:
//! - Inline mode with streaming logs and stable UI chrome
//! - Elm/Bubbletea-style Model/Update/View pattern
//! - LogViewer, StatusLine, TextInput, and Spinner widgets
//! - No flicker, no cursor corruption, reliable cleanup
//!
//! # Running
//!
//! ```sh
//! cargo run -p ftui-harness
//! ```
//!
//! # Controls
//!
//! - Type to enter text in the input field
//! - Enter: Submit command (echoed to log)
//! - Ctrl+C / Ctrl+Q: Quit
//! - Page Up/Down: Scroll log viewer
//! - Escape: Clear input

use std::cell::RefCell;
use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::{App, Cmd, Every, Model, ScreenMode, Subscription};
use ftui_style::Style;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::log_viewer::{LogViewer, LogViewerState};
use ftui_widgets::spinner::{DOTS, Spinner, SpinnerState};
use ftui_widgets::status_line::{StatusItem, StatusLine};
use ftui_widgets::{StatefulWidget, Widget};

/// Application state for the agent harness.
struct AgentHarness {
    /// Log viewer for streaming output.
    log_viewer: LogViewer,
    /// State for log viewer scrolling.
    log_state: RefCell<LogViewerState>,
    /// Text input for user commands.
    input: TextInput,
    /// Spinner state for animation.
    spinner_state: SpinnerState,
    /// Current model name (simulated).
    model_name: String,
    /// Current tool being run (if any).
    current_tool: Option<String>,
    /// Command count for demo purposes.
    command_count: usize,
    /// Whether a simulated task is running.
    task_running: bool,
    /// Tick counter for simulated task progress.
    task_tick_count: u32,
    /// Optional auto-quit countdown in spinner ticks (100ms each).
    auto_quit_ticks: Option<u32>,
}

/// Messages for the agent harness.
#[derive(Debug)]
#[allow(dead_code)]
enum Msg {
    /// A key was pressed.
    Key(KeyEvent),
    /// Tick for spinner animation.
    SpinnerTick,
    /// A log line was received.
    LogLine(String),
    /// Simulated tool started.
    ToolStart(String),
    /// Simulated tool finished.
    ToolEnd,
    /// Quit the application.
    Quit,
    /// Ignored event.
    Noop,
}

impl From<Event> for Msg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) => Msg::Key(key),
            _ => Msg::Noop,
        }
    }
}

impl AgentHarness {
    fn new() -> Self {
        let mut log_viewer = LogViewer::new(10_000);
        log_viewer.push("Welcome to the Agent Harness Reference Application");
        log_viewer.push("---");
        log_viewer.push("This demonstrates FrankenTUI's inline mode with:");
        log_viewer.push("  - Streaming log output without flicker");
        log_viewer.push("  - Stable UI chrome (status bar, input line)");
        log_viewer.push("  - Elm/Bubbletea-style architecture");
        log_viewer.push("---");
        log_viewer.push("Type a command and press Enter. Use Ctrl+C to quit.");
        log_viewer.push("");

        let extra_logs = std::env::var("FTUI_HARNESS_LOG_LINES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);

        for idx in 1..=extra_logs {
            log_viewer.push(format!("Log line {}", idx));
        }

        let auto_quit_ticks = std::env::var("FTUI_HARNESS_EXIT_AFTER_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(|ms| {
                if ms == 0 {
                    None
                } else {
                    Some(ms.div_ceil(100) as u32)
                }
            });

        Self {
            log_viewer,
            log_state: RefCell::new(LogViewerState::default()),
            input: TextInput::new()
                .with_placeholder("Enter command...")
                .with_style(Style::new())
                .with_focused(true),
            spinner_state: SpinnerState::default(),
            model_name: "claude-3.5".to_string(),
            current_tool: None,
            command_count: 0,
            task_running: false,
            task_tick_count: 0,
            auto_quit_ticks,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Cmd<Msg> {
        // Only handle Press events
        if key.kind != KeyEventKind::Press {
            return Cmd::None;
        }

        // Global shortcuts
        if key.modifiers.contains(Modifiers::CTRL) {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('q') => return Cmd::Quit,
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => {
                let command = self.input.value().to_string();
                if !command.is_empty() {
                    self.command_count += 1;
                    self.log_viewer.push(format!("> {}", command));
                    self.input.clear();

                    // Simulate different commands
                    match command.as_str() {
                        "help" => {
                            self.log_viewer.push("Available commands:");
                            self.log_viewer.push("  help      - Show this help");
                            self.log_viewer.push("  search    - Simulate a search task");
                            self.log_viewer.push("  status    - Show current status");
                            self.log_viewer.push("  clear     - Clear the log");
                            self.log_viewer.push("  quit      - Exit the application");
                        }
                        "search" => {
                            self.task_running = true;
                            self.task_tick_count = 0;
                            self.current_tool = Some("grep".to_string());
                            self.log_viewer.push("Starting search...");
                            // Simulate async task
                            return Cmd::Batch(vec![
                                Cmd::Msg(Msg::LogLine("Searching for patterns...".to_string())),
                                Cmd::Tick(Duration::from_millis(500)),
                            ]);
                        }
                        "status" => {
                            self.log_viewer.push(format!(
                                "Model: {} | Commands: {} | Task: {}",
                                self.model_name,
                                self.command_count,
                                if self.task_running { "Running" } else { "Idle" }
                            ));
                        }
                        "clear" => {
                            self.log_viewer.clear();
                            self.log_viewer.push("Log cleared.");
                        }
                        "quit" => return Cmd::Quit,
                        _ => {
                            self.log_viewer.push(format!(
                                "Unknown command: '{}'. Type 'help' for available commands.",
                                command
                            ));
                        }
                    }
                }
            }
            KeyCode::Escape => {
                self.input.clear();
            }
            KeyCode::PageUp => {
                let log_state = self.log_state.borrow();
                self.log_viewer.page_up(&log_state);
            }
            KeyCode::PageDown => {
                let log_state = self.log_state.borrow();
                self.log_viewer.page_down(&log_state);
            }
            _ => {
                // Forward to input widget
                self.input.handle_event(&Event::Key(key));
            }
        }

        Cmd::None
    }
}

impl Model for AgentHarness {
    type Message = Msg;

    fn init(&mut self) -> Cmd<Self::Message> {
        // No initial commands
        Cmd::None
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
            Msg::SpinnerTick => {
                self.spinner_state.tick();

                if let Some(ticks) = self.auto_quit_ticks.as_mut() {
                    if *ticks > 0 {
                        *ticks = ticks.saturating_sub(1);
                    }

                    if *ticks == 0 {
                        return Cmd::Quit;
                    }
                }

                // Simulate task progress
                if self.task_running {
                    self.task_tick_count += 1;
                    if self.task_tick_count >= 10 {
                        self.task_tick_count = 0;
                        self.task_running = false;
                        self.current_tool = None;
                        self.log_viewer.push("Search complete. Found 42 matches.");
                    }
                }
                Cmd::None
            }
            Msg::LogLine(line) => {
                self.log_viewer.push(line);
                Cmd::None
            }
            Msg::ToolStart(name) => {
                self.current_tool = Some(name);
                self.task_running = true;
                Cmd::None
            }
            Msg::ToolEnd => {
                self.current_tool = None;
                self.task_running = false;
                Cmd::None
            }
            Msg::Quit => Cmd::Quit,
            Msg::Noop => Cmd::None,
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());

        // Layout: Status bar (1), Log viewer (fill), Input (3)
        let chunks = Flex::vertical()
            .constraints([
                Constraint::Fixed(1), // Status bar
                Constraint::Min(3),   // Log viewer
                Constraint::Fixed(3), // Input with border
            ])
            .split(area);

        // --- Status Bar ---
        let tool_status = match &self.current_tool {
            Some(tool) => format!("Running: {}", tool),
            None => "Idle".to_string(),
        };

        let status = StatusLine::new()
            .left(StatusItem::text(&self.model_name))
            .center(StatusItem::text(&tool_status))
            .right(StatusItem::key_hint("^C", "Quit"));

        status.render(chunks[0], frame);

        // --- Log Viewer ---
        let log_block = Block::new()
            .title(" Log ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        let inner = log_block.inner(chunks[1]);
        log_block.render(chunks[1], frame);

        // Render log viewer (need mutable state)
        let mut log_state = self.log_state.borrow_mut();
        self.log_viewer.render(inner, frame, &mut log_state);

        // --- Input Line ---
        let input_block = Block::new()
            .title(" Command ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        let input_inner = input_block.inner(chunks[2]);
        input_block.render(chunks[2], frame);

        // Render input
        self.input.render(input_inner, frame);

        // Spinner in bottom-right corner if task running
        if self.task_running {
            let spinner_area = Rect::new(
                area.width.saturating_sub(3),
                area.height.saturating_sub(2),
                2,
                1,
            );
            let spinner = Spinner::new().frames(DOTS);
            let mut spinner_state = self.spinner_state.clone();
            StatefulWidget::render(&spinner, spinner_area, frame, &mut spinner_state);
        }
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        // Tick every 100ms for spinner animation
        vec![Box::new(Every::new(Duration::from_millis(100), || {
            Msg::SpinnerTick
        }))]
    }
}

fn main() -> std::io::Result<()> {
    let ui_height = std::env::var("FTUI_HARNESS_UI_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(10);

    let screen_mode = match std::env::var("FTUI_HARNESS_SCREEN_MODE") {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "alt" | "altscreen" | "alt-screen" | "alt_screen" => ScreenMode::AltScreen,
            _ => ScreenMode::Inline { ui_height },
        },
        Err(_) => ScreenMode::Inline { ui_height },
    };

    // Run the agent harness in inline mode
    App::new(AgentHarness::new()).screen_mode(screen_mode).run()
}
