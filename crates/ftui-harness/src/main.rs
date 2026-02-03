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
//! - Escape: Clear input (if text present), cancel task (if running), or close overlay
//! - Esc Esc: Toggle tree view overlay (double-tap within 250ms)
//! - Ctrl+C: Clear input (if text present), cancel task (if running), or quit
//! - Ctrl+D: Soft quit (cancel task if running, otherwise quit)
//! - Ctrl+Q: Hard quit (immediate exit)
//! - Ctrl+T: Cycle theme
//! - Page Up/Down: Scroll log viewer
//!
//! # Keybinding Policy
//!
//! This harness implements the Pi-style keybinding policy (bd-2vne.1) using the
//! ActionMapper from ftui-core. See `docs/spec/keybinding-policy.md` for details.

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseEvent, MouseEventKind, PasteEvent,
};
use ftui_core::geometry::Rect;
use ftui_core::input_parser::InputParser;
use ftui_core::keybinding::{Action, ActionMapper, AppState};
use ftui_core::terminal_session::{SessionOptions, TerminalSession};
use ftui_extras::theme;
use ftui_layout::{Constraint, Flex, Grid, GridArea};
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::{Cmd, Every, Model, Program, ProgramConfig, ScreenMode, Subscription};
use ftui_style::Style;
use ftui_text::WrapMode;
use ftui_widgets::block::Alignment;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::list::{List, ListState};
use ftui_widgets::log_viewer::{LogViewer, LogViewerState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::spinner::{DOTS, Spinner, SpinnerState};
use ftui_widgets::status_line::{StatusItem, StatusLine};
use ftui_widgets::table::{Row, Table, TableState};
use ftui_widgets::tree::{Tree, TreeNode};
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
    /// Which view layout to render.
    view_mode: HarnessView,
    /// Whether to log key events to the log viewer.
    log_keys: bool,
    /// Keybinding action mapper (handles Esc sequences, Ctrl+C priority, etc).
    action_mapper: ActionMapper,
    /// Whether the tree view overlay is visible.
    tree_view_open: bool,
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
    /// Terminal resized.
    Resize { width: u16, height: u16 },
    /// Mouse event observed.
    Mouse(MouseEvent),
    /// Paste event observed.
    Paste(PasteEvent),
    /// Focus changed.
    Focus(bool),
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
            Event::Resize { width, height } => Msg::Resize { width, height },
            Event::Mouse(mouse) => Msg::Mouse(mouse),
            Event::Paste(paste) => Msg::Paste(paste),
            Event::Focus(focused) => Msg::Focus(focused),
            _ => Msg::Noop,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum HarnessView {
    Default,
    LayoutFlexRow,
    LayoutFlexCol,
    LayoutGrid,
    LayoutNested,
    WidgetBlock,
    WidgetParagraph,
    WidgetTable,
    WidgetList,
    WidgetInput,
}

impl AgentHarness {
    fn new(view_mode: HarnessView, log_keys: bool) -> Self {
        let suppress_welcome = std::env::var("FTUI_HARNESS_SUPPRESS_WELCOME")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        let mut log_viewer = LogViewer::new(10_000);
        if !suppress_welcome {
            log_viewer.push("Welcome to the Agent Harness Reference Application");
            log_viewer.push("---");
            log_viewer.push("This demonstrates FrankenTUI's inline mode with:");
            log_viewer.push("  - Streaming log output without flicker");
            log_viewer.push("  - Stable UI chrome (status bar, input line)");
            log_viewer.push("  - Elm/Bubbletea-style architecture");
            log_viewer.push("---");
            log_viewer.push("Type a command and press Enter. Use Ctrl+C to quit.");
            log_viewer.push("");
        }

        let markup_enabled = std::env::var("FTUI_HARNESS_LOG_MARKUP")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

        if let Ok(path) = std::env::var("FTUI_HARNESS_LOG_FILE")
            && let Ok(contents) = std::fs::read_to_string(path)
        {
            for line in contents.lines() {
                if markup_enabled {
                    match ftui_text::markup::parse_markup(line) {
                        Ok(text) => log_viewer.push(text),
                        Err(_) => log_viewer.push(line),
                    }
                } else {
                    log_viewer.push(line);
                }
            }
        }

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
            view_mode,
            log_keys,
            action_mapper: ActionMapper::from_env(),
            tree_view_open: false,
        }
    }

    /// Get the current application state for keybinding resolution.
    fn app_state(&self) -> AppState {
        AppState::new()
            .with_input(!self.input.value().is_empty())
            .with_task(self.task_running)
            .with_modal(false) // No modals in current harness
            .with_overlay(self.tree_view_open)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Cmd<Msg> {
        if self.log_keys {
            let mods = format_modifiers(key.modifiers);
            self.log_viewer.push(format!(
                "Key: code={:?} kind={:?} mods={}",
                key.code, key.kind, mods
            ));
        }

        // Only handle Press events
        if key.kind != KeyEventKind::Press {
            return Cmd::None;
        }

        // Ctrl+T for theme cycling (harness-specific, not part of keybinding spec)
        if key.modifiers.contains(Modifiers::CTRL)
            && matches!(key.code, KeyCode::Char('t') | KeyCode::Char('T'))
        {
            let next = theme::cycle_theme();
            self.log_viewer.push(format!("Theme: {}", next.name()));
            return Cmd::None;
        }

        // Use the ActionMapper for keybinding resolution
        let state = self.app_state();
        let now = Instant::now();
        match self.action_mapper.map(&key, &state, now) {
            Some(Action::PassThrough) => {
                // Pass through to raw key handling
                return self.handle_raw_key(key);
            }
            Some(action) => {
                return self.handle_action(action);
            }
            None => {
                // ActionMapper returned None (e.g., pending Esc or Noop)
                // Nothing to do right now; timeout will be checked on tick
            }
        }
        Cmd::None
    }

    /// Handle a resolved keybinding action.
    fn handle_action(&mut self, action: Action) -> Cmd<Msg> {
        match action {
            Action::ClearInput => {
                if !self.input.value().is_empty() {
                    self.input.clear();
                    self.log_viewer.push("(Input cleared)");
                }
                Cmd::None
            }
            Action::CancelTask => {
                if self.task_running {
                    self.task_running = false;
                    self.current_tool = None;
                    self.log_viewer.push("(Task cancelled)");
                }
                Cmd::None
            }
            Action::DismissModal => {
                // No modals in current harness; treat as no-op
                Cmd::None
            }
            Action::CloseOverlay => {
                if self.tree_view_open {
                    self.tree_view_open = false;
                    self.log_viewer.push("(Tree view closed)");
                }
                Cmd::None
            }
            Action::ToggleTreeView => {
                self.tree_view_open = !self.tree_view_open;
                let status = if self.tree_view_open {
                    "opened"
                } else {
                    "closed"
                };
                self.log_viewer.push(format!("(Tree view {})", status));
                Cmd::None
            }
            Action::Quit | Action::HardQuit => Cmd::Quit,
            Action::SoftQuit => {
                // Soft quit: cancel task if running, otherwise quit
                if self.task_running {
                    self.task_running = false;
                    self.current_tool = None;
                    self.log_viewer.push("(Task cancelled via Ctrl+D)");
                    Cmd::None
                } else {
                    Cmd::Quit
                }
            }
            Action::Bell => {
                // Emit terminal bell (BEL character)
                // The runtime should handle this, but we can log it
                self.log_viewer.push("(Bell)");
                Cmd::None
            }
            Action::PassThrough => {
                // PassThrough should be handled in handle_key, not here
                // This case exists for exhaustive matching
                Cmd::None
            }
        }
    }

    /// Handle raw key input for passthrough cases.
    fn handle_raw_key(&mut self, key: KeyEvent) -> Cmd<Msg> {
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
                            self.log_viewer.push("  tree      - Toggle tree view");
                        }
                        "search" => {
                            self.task_running = true;
                            self.task_tick_count = 0;
                            self.current_tool = Some("grep".to_string());
                            self.log_viewer.push("Starting search...");
                            return Cmd::Batch(vec![
                                Cmd::Msg(Msg::LogLine("Searching for patterns...".to_string())),
                                Cmd::Tick(Duration::from_millis(500)),
                            ]);
                        }
                        "status" => {
                            self.log_viewer.push(format!(
                                "Model: {} | Commands: {} | Task: {} | Tree: {}",
                                self.model_name,
                                self.command_count,
                                if self.task_running { "Running" } else { "Idle" },
                                if self.tree_view_open { "Open" } else { "Closed" }
                            ));
                        }
                        "clear" => {
                            self.log_viewer.clear();
                            self.log_viewer.push("Log cleared.");
                        }
                        "tree" => {
                            self.tree_view_open = !self.tree_view_open;
                            let status = if self.tree_view_open {
                                "opened"
                            } else {
                                "closed"
                            };
                            self.log_viewer.push(format!("Tree view {}.", status));
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

fn format_modifiers(mods: Modifiers) -> String {
    if mods.is_empty() {
        return "none".to_string();
    }
    let mut parts = Vec::new();
    if mods.contains(Modifiers::SHIFT) {
        parts.push("shift");
    }
    if mods.contains(Modifiers::ALT) {
        parts.push("alt");
    }
    if mods.contains(Modifiers::CTRL) {
        parts.push("ctrl");
    }
    if mods.contains(Modifiers::SUPER) {
        parts.push("super");
    }
    parts.join("+")
}

fn parse_exit_after() -> Option<Duration> {
    std::env::var("FTUI_HARNESS_EXIT_AFTER_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|ms| {
            if ms == 0 {
                None
            } else {
                Some(Duration::from_millis(ms))
            }
        })
}

fn run_input_trace(exit_after: Option<Duration>) -> io::Result<()> {
    let _session = TerminalSession::new(SessionOptions {
        kitty_keyboard: true,
        ..Default::default()
    })?;
    let mut parser = InputParser::new();
    let start = Instant::now();
    let mut stdout = io::stdout();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let poll_timeout = Duration::from_millis(50);

    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(count) => {
                    if tx.send(buf[..count].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    loop {
        if let Some(limit) = exit_after
            && start.elapsed() >= limit
        {
            break;
        }

        match rx.recv_timeout(poll_timeout) {
            Ok(bytes) => {
                for event in parser.parse(&bytes) {
                    if let Event::Key(key) = event {
                        let mods = format_modifiers(key.modifiers);
                        writeln!(
                            stdout,
                            "Key: code={:?} kind={:?} mods={}",
                            key.code, key.kind, mods
                        )?;
                    }
                }
                stdout.flush()?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
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

                // Check for pending Esc timeout (Esc Esc detection)
                let state = self.app_state();
                let now = Instant::now();
                if let Some(action) = self.action_mapper.check_timeout(&state, now) {
                    let cmd = self.handle_action(action);
                    if !matches!(cmd, Cmd::None) {
                        return cmd;
                    }
                }

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
            Msg::Resize { width, height } => {
                self.log_viewer
                    .push(format!("Resize: {}x{}", width, height));
                Cmd::None
            }
            Msg::Mouse(mouse) => {
                let kind = match mouse.kind {
                    MouseEventKind::Down(button) => format!("Down({button:?})"),
                    MouseEventKind::Up(button) => format!("Up({button:?})"),
                    MouseEventKind::Drag(button) => format!("Drag({button:?})"),
                    MouseEventKind::Moved => "Moved".to_string(),
                    MouseEventKind::ScrollUp => "ScrollUp".to_string(),
                    MouseEventKind::ScrollDown => "ScrollDown".to_string(),
                    MouseEventKind::ScrollLeft => "ScrollLeft".to_string(),
                    MouseEventKind::ScrollRight => "ScrollRight".to_string(),
                };
                self.log_viewer
                    .push(format!("Mouse: {} @ {},{}", kind, mouse.x, mouse.y));
                Cmd::None
            }
            Msg::Paste(paste) => {
                self.log_viewer.push(format!("Paste: {}", paste.text));
                Cmd::None
            }
            Msg::Focus(focused) => {
                self.log_viewer.push(if focused {
                    "Focus: gained"
                } else {
                    "Focus: lost"
                });
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
        self.apply_theme_base(frame);

        // If tree view is open, render it as an overlay
        if self.tree_view_open {
            self.view_tree_overlay(frame);
            return;
        }

        match self.view_mode {
            HarnessView::Default => self.view_default(frame),
            HarnessView::LayoutFlexRow => self.view_layout_flex_row(frame),
            HarnessView::LayoutFlexCol => self.view_layout_flex_col(frame),
            HarnessView::LayoutGrid => self.view_layout_grid(frame),
            HarnessView::LayoutNested => self.view_layout_nested(frame),
            HarnessView::WidgetBlock => self.view_widget_block(frame),
            HarnessView::WidgetParagraph => self.view_widget_paragraph(frame),
            HarnessView::WidgetTable => self.view_widget_table(frame),
            HarnessView::WidgetList => self.view_widget_list(frame),
            HarnessView::WidgetInput => self.view_widget_input(frame),
        }
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        // Tick every 100ms for spinner animation
        vec![Box::new(Every::new(Duration::from_millis(100), || {
            Msg::SpinnerTick
        }))]
    }
}

impl AgentHarness {
    fn apply_theme_base(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        frame.buffer.fill(
            area,
            Cell::default()
                .with_bg(theme::bg::DEEP.into())
                .with_fg(theme::fg::PRIMARY.into()),
        );
    }

    fn view_default(&self, frame: &mut Frame) {
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
            .style(
                Style::new()
                    .bg(theme::alpha::OVERLAY)
                    .fg(theme::fg::SECONDARY),
            )
            .separator("  ")
            .left(StatusItem::text(&self.model_name))
            .center(StatusItem::text(&tool_status))
            .right(StatusItem::text(theme::current_theme_name()))
            .right(StatusItem::key_hint("^T", "Theme"))
            .right(StatusItem::key_hint("^C", "Quit"));

        status.render(chunks[0], frame);

        // --- Log Viewer ---
        let log_block = Block::new()
            .title(" Log ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));

        let inner = log_block.inner(chunks[1]);
        log_block.render(chunks[1], frame);

        // Render log viewer (need mutable state)
        let mut log_state = self.log_state.borrow_mut();
        self.log_viewer.render(inner, frame, &mut log_state);

        // --- Input Line ---
        let input_block = Block::new()
            .title(" Command ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));

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

    fn render_label_block(&self, frame: &mut Frame, area: Rect, title: &str, body: &str) {
        let block = Block::new()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));
        let inner = block.inner(area);
        block.render(area, frame);

        let paragraph = Paragraph::new(body)
            .alignment(Alignment::Center)
            .wrap(WrapMode::Word);
        paragraph.render(inner, frame);
    }

    fn view_layout_flex_row(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let chunks = Flex::horizontal()
            .constraints([
                Constraint::Percentage(30.0),
                Constraint::Percentage(40.0),
                Constraint::Percentage(30.0),
            ])
            .split(area);

        self.render_label_block(frame, chunks[0], " Left ", "LEFT");
        self.render_label_block(frame, chunks[1], " Center ", "CENTER");
        self.render_label_block(frame, chunks[2], " Right ", "RIGHT");
    }

    fn view_layout_flex_col(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let chunks = Flex::vertical()
            .constraints([
                Constraint::Fixed(3),
                Constraint::Min(3),
                Constraint::Fixed(3),
            ])
            .split(area);

        self.render_label_block(frame, chunks[0], " Top ", "TOP");
        self.render_label_block(frame, chunks[1], " Middle ", "MIDDLE");
        self.render_label_block(frame, chunks[2], " Bottom ", "BOTTOM");
    }

    fn view_layout_grid(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let grid = Grid::new()
            .rows([
                Constraint::Fixed(3),
                Constraint::Min(4),
                Constraint::Fixed(3),
            ])
            .columns([Constraint::Percentage(30.0), Constraint::Min(10)])
            .row_gap(0)
            .col_gap(1)
            .area("header", GridArea::span(0, 0, 1, 2))
            .area("sidebar", GridArea::cell(1, 0))
            .area("content", GridArea::cell(1, 1))
            .area("footer", GridArea::span(2, 0, 1, 2));

        let layout = grid.split(area);
        if let Some(area) = layout.area("header") {
            self.render_label_block(frame, area, " Header ", "HEADER");
        }
        if let Some(area) = layout.area("sidebar") {
            self.render_label_block(frame, area, " Sidebar ", "SIDEBAR");
        }
        if let Some(area) = layout.area("content") {
            self.render_label_block(frame, area, " Content ", "CONTENT");
        }
        if let Some(area) = layout.area("footer") {
            self.render_label_block(frame, area, " Footer ", "FOOTER");
        }
    }

    fn view_layout_nested(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let outer = Flex::vertical()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(area);

        let top = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(outer[0]);

        self.render_label_block(frame, top[0], " A ", "LEFT");
        self.render_label_block(frame, top[1], " B ", "RIGHT");
        self.render_label_block(frame, outer[1], " Bottom ", "BOTTOM");
    }

    fn view_widget_block(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        self.render_label_block(frame, area, " Block ", "BLOCK");
    }

    fn view_widget_paragraph(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let block = Block::new()
            .title(" Paragraph ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));
        let inner = block.inner(area);
        block.render(area, frame);

        let paragraph =
            Paragraph::new("This paragraph wraps long text across multiple lines for testing.")
                .wrap(WrapMode::Word)
                .alignment(Alignment::Left);
        paragraph.render(inner, frame);
    }

    fn view_widget_table(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let rows = vec![
            Row::new(["Alpha", "1"]),
            Row::new(["Beta", "2"]),
            Row::new(["Gamma", "3"]),
        ];
        let header = Row::new(["Name", "Value"]).style(
            Style::new()
                .bg(theme::alpha::OVERLAY)
                .fg(theme::fg::PRIMARY)
                .bold(),
        );
        let table = Table::new(
            rows,
            [Constraint::Percentage(70.0), Constraint::Percentage(30.0)],
        )
        .header(header)
        .block(
            Block::new()
                .title(" Table ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(Style::new().bg(theme::alpha::SURFACE))
                .border_style(Style::new().fg(theme::fg::MUTED)),
        )
        .highlight_style(
            Style::new()
                .fg(theme::bg::DEEP)
                .bg(theme::accent::PRIMARY)
                .bold(),
        );

        let mut state = TableState::default();
        state.select(Some(1));
        StatefulWidget::render(&table, area, frame, &mut state);
    }

    fn view_widget_list(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let items = vec!["Item one", "Item two", "Item three", "Item four"];
        let list = List::new(items)
            .block(
                Block::new()
                    .title(" List ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(Style::new().bg(theme::alpha::SURFACE))
                    .border_style(Style::new().fg(theme::fg::MUTED)),
            )
            .highlight_style(
                Style::new()
                    .fg(theme::bg::DEEP)
                    .bg(theme::accent::PRIMARY)
                    .bold(),
            )
            .highlight_symbol("> ");

        let mut state = ListState::default();
        state.select(Some(2));
        StatefulWidget::render(&list, area, frame, &mut state);
    }

    fn view_widget_input(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let block = Block::new()
            .title(" Input ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));
        let inner = block.inner(area);
        block.render(area, frame);
        self.input.render(inner, frame);
    }
}

fn main() -> std::io::Result<()> {
    let input_mode = std::env::var("FTUI_HARNESS_INPUT_MODE")
        .unwrap_or_else(|_| "runtime".to_string())
        .to_ascii_lowercase();

    if input_mode == "parser" || input_mode == "input-parser" || input_mode == "input_parser" {
        return run_input_trace(parse_exit_after());
    }

    let ui_height = std::env::var("FTUI_HARNESS_UI_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(10);

    let auto_ui_height = std::env::var("FTUI_HARNESS_AUTO_UI_HEIGHT")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let screen_mode = match std::env::var("FTUI_HARNESS_SCREEN_MODE") {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "alt" | "altscreen" | "alt-screen" | "alt_screen" => ScreenMode::AltScreen,
            _ => {
                if auto_ui_height {
                    ScreenMode::InlineAuto {
                        min_height: ui_height,
                        max_height: u16::MAX,
                    }
                } else {
                    ScreenMode::Inline { ui_height }
                }
            }
        },
        Err(_) => {
            if auto_ui_height {
                ScreenMode::InlineAuto {
                    min_height: ui_height,
                    max_height: u16::MAX,
                }
            } else {
                ScreenMode::Inline { ui_height }
            }
        }
    };

    let view_mode = match std::env::var("FTUI_HARNESS_VIEW")
        .unwrap_or_else(|_| "default".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "layout-flex-row" | "layout_flex_row" | "flex_row" => HarnessView::LayoutFlexRow,
        "layout-flex-col" | "layout_flex_col" | "flex_col" => HarnessView::LayoutFlexCol,
        "layout-grid" | "layout_grid" | "grid" => HarnessView::LayoutGrid,
        "layout-nested" | "layout_nested" | "nested" => HarnessView::LayoutNested,
        "widget-block" | "widget_block" | "block" => HarnessView::WidgetBlock,
        "widget-paragraph" | "widget_paragraph" | "paragraph" => HarnessView::WidgetParagraph,
        "widget-table" | "widget_table" | "table" => HarnessView::WidgetTable,
        "widget-list" | "widget_list" | "list" => HarnessView::WidgetList,
        "widget-input" | "widget_input" | "input" => HarnessView::WidgetInput,
        _ => HarnessView::Default,
    };

    let enable_mouse = std::env::var("FTUI_HARNESS_ENABLE_MOUSE")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let enable_focus = std::env::var("FTUI_HARNESS_ENABLE_FOCUS")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let enable_kitty_keyboard = std::env::var("FTUI_HARNESS_ENABLE_KITTY_KEYBOARD")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let config = ProgramConfig {
        screen_mode,
        mouse: enable_mouse,
        focus_reporting: enable_focus,
        kitty_keyboard: enable_kitty_keyboard,
        ..Default::default()
    };

    // Run the agent harness in inline mode
    let log_keys = std::env::var("FTUI_HARNESS_LOG_KEYS")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let mut program = Program::with_config(AgentHarness::new(view_mode, log_keys), config)?;
    program.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_cycle_advances_and_restores() {
        let original = theme::current_theme();
        let next = theme::cycle_theme();
        assert_ne!(original, next);
        theme::set_theme(original);
        assert_eq!(theme::current_theme(), original);
    }

    #[test]
    fn status_line_style_has_fg_and_bg() {
        let style = Style::new()
            .bg(theme::alpha::OVERLAY)
            .fg(theme::fg::SECONDARY);
        assert!(style.bg.is_some());
        assert!(style.fg.is_some());
    }
}
