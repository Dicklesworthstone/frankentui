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
use std::path::Path;
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
use ftui_harness::flicker_detection;
use ftui_layout::{Constraint, Flex, Grid, GridArea};
use ftui_render::cell::Cell;
use ftui_render::frame::{Frame, HitId, HitRegion, WidgetSignal};
#[cfg(feature = "telemetry")]
use ftui_runtime::TelemetryConfig;
use ftui_runtime::locale::{Locale, LocaleContext, detect_system_locale, set_locale};
use ftui_runtime::{
    Cmd, ConformalConfig, Every, EvidenceSinkConfig, Model, Program, ProgramConfig,
    RenderTraceConfig, ScreenMode, Subscription, TaskSpec,
};
use ftui_style::Style;
use ftui_text::WrapMode;
use ftui_widgets::block::Alignment;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::inspector::{InspectorMode, InspectorOverlay, InspectorState, WidgetInfo};
use ftui_widgets::list::{List, ListState};
use ftui_widgets::log_viewer::{LogViewer, LogViewerState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::spinner::{DOTS, Spinner, SpinnerState};
use ftui_widgets::status_line::{StatusItem, StatusLine};
use ftui_widgets::table::{Row, Table, TableState};
use ftui_widgets::tree::{Tree, TreeNode};
use ftui_widgets::{Budgeted, StatefulWidget, Widget};

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
    /// Enable stress mode for the UI inspector (large widget trees).
    inspector_stress: bool,
    /// Grid columns for inspector stress tree.
    #[allow(dead_code)]
    inspector_grid_cols: u16,
    /// Grid rows for inspector stress tree.
    #[allow(dead_code)]
    inspector_grid_rows: u16,
    /// Depth for nested inspector widgets in stress mode.
    #[allow(dead_code)]
    inspector_depth: u8,
    /// Optional locale override for the locale context view.
    locale_override: Option<Locale>,
    /// Countdown ticks before switching the base locale.
    locale_switch_ticks: Option<u32>,
    /// Target locale to switch to after the countdown.
    locale_switch_target: Option<Locale>,
    /// Whether effect-queue tasks have been enqueued.
    effect_queue_seeded: bool,
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
    WidgetInspector,
    WidgetBudget,
    TileSkip,
    SpanDiff,
    SelectorStorm,
    #[allow(dead_code)]
    EffectQueue,
    LocaleContext,
}

struct BudgetCardSpec {
    widget_id: u64,
    title: &'static str,
    body: &'static str,
    essential: bool,
    priority: f32,
    staleness_ms: u64,
    cost_us: f32,
    focus_boost: f32,
    interaction_boost: f32,
}

struct BudgetCard {
    title: &'static str,
    body: &'static str,
    essential: bool,
}

impl Widget for BudgetCard {
    fn render(&self, area: Rect, frame: &mut Frame) {
        let block = Block::new()
            .title(self.title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));
        let inner = block.inner(area);
        block.render(area, frame);

        let paragraph = Paragraph::new(self.body)
            .alignment(Alignment::Center)
            .wrap(WrapMode::Word);
        paragraph.render(inner, frame);
    }

    fn is_essential(&self) -> bool {
        self.essential
    }
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

        let inspector_stress = std::env::var("FTUI_HARNESS_INSPECTOR_STRESS")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        let inspector_grid_cols = std::env::var("FTUI_HARNESS_INSPECTOR_GRID_COLS")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(12)
            .max(1);
        let inspector_grid_rows = std::env::var("FTUI_HARNESS_INSPECTOR_GRID_ROWS")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(8)
            .max(1);
        let inspector_depth = std::env::var("FTUI_HARNESS_INSPECTOR_DEPTH")
            .ok()
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(3)
            .max(1);

        let locale_override =
            std::env::var("FTUI_HARNESS_LOCALE_OVERRIDE")
                .ok()
                .and_then(|value| {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                });

        let locale_switch_target = std::env::var("FTUI_HARNESS_LOCALE_SWITCH_TO")
            .ok()
            .and_then(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            });

        let locale_switch_ticks = std::env::var("FTUI_HARNESS_LOCALE_SWITCH_TICKS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .or_else(|| {
                std::env::var("FTUI_HARNESS_LOCALE_SWITCH_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .and_then(|ms| {
                        if ms == 0 {
                            None
                        } else {
                            Some(ms.div_ceil(100) as u32)
                        }
                    })
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
            inspector_stress,
            inspector_grid_cols,
            inspector_grid_rows,
            inspector_depth,
            locale_override,
            locale_switch_ticks,
            locale_switch_target,
            effect_queue_seeded: false,
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
                                if self.tree_view_open {
                                    "Open"
                                } else {
                                    "Closed"
                                }
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
        if matches!(self.view_mode, HarnessView::EffectQueue) && !self.effect_queue_seeded {
            self.effect_queue_seeded = true;
            return self.seed_effect_queue_cmd();
        }
        Cmd::None
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
            Msg::SpinnerTick => {
                self.spinner_state.tick();

                let mut fire_locale_switch = false;
                if let Some(ticks) = self.locale_switch_ticks.as_mut() {
                    if *ticks > 0 {
                        *ticks = ticks.saturating_sub(1);
                    }
                    if *ticks == 0 {
                        fire_locale_switch = true;
                    }
                }
                if fire_locale_switch {
                    if let Some(target) = self.locale_switch_target.clone() {
                        set_locale(target.clone());
                        self.log_viewer.push(format!("Locale switch -> {}", target));
                    }
                    self.locale_switch_ticks = None;
                }

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
            HarnessView::WidgetInspector => self.view_widget_inspector(frame),
            HarnessView::WidgetBudget => self.view_widget_budget(frame),
            HarnessView::TileSkip => self.view_tile_skip(frame),
            HarnessView::SpanDiff => self.view_span_diff(frame),
            HarnessView::SelectorStorm => self.view_selector_storm(frame),
            HarnessView::EffectQueue => self.view_effect_queue(frame),
            HarnessView::LocaleContext => self.view_locale_context(frame),
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

    fn span_positions(frame_idx: u16, width: u16, height: u16) -> [(u16, u16); 4] {
        let w = width.max(1);
        let h = height.max(1);
        let band = w.saturating_sub(8).max(1);
        let base = frame_idx.wrapping_mul(3) % band;
        let x0 = base.saturating_add(2).min(w.saturating_sub(1));
        let x1 = x0.saturating_add(3).min(w.saturating_sub(1));
        let x2 = x0.saturating_add(5).min(w.saturating_sub(1));
        let row_a = (h / 3).min(h.saturating_sub(1));
        let row_b = (h.saturating_mul(2) / 3).min(h.saturating_sub(1));
        [(x0, row_a), (x1, row_a), (x0, row_b), (x2, row_b)]
    }

    fn tile_skip_spans(frame_idx: u16, width: u16, height: u16) -> Vec<(u16, u16, u16)> {
        let w = width.max(1) as usize;
        let h = height.max(1) as usize;
        let total_cells = w.saturating_mul(h);
        let target_cells = (total_cells.saturating_mul(2) / 100).max(1); // ~2% changes
        let span_len = (w / 10).max(1);
        let mut rows = (target_cells / span_len).max(1);
        rows = rows.min(h);
        let row_stride = (h / rows).max(1);
        let base_row = (frame_idx as usize).wrapping_mul(3) % h;
        let base_col = (frame_idx as usize).wrapping_mul(7) % w;

        let mut spans = Vec::with_capacity(rows);
        for i in 0..rows {
            let y = (base_row + i * row_stride) % h;
            let x0 = (base_col + i * 11) % w;
            let x1 = (x0 + span_len).min(w);
            let len = (x1.saturating_sub(x0)).max(1);
            spans.push((x0 as u16, y as u16, len as u16));
        }
        spans
    }

    fn view_tile_skip(&self, frame: &mut Frame) {
        self.apply_theme_base(frame);
        frame.buffer.clear_dirty();

        let width = frame.buffer.width();
        let height = frame.buffer.height();
        if width == 0 || height == 0 {
            return;
        }

        let frame_idx = self.spinner_state.current_frame as u16;
        let prev_idx = frame_idx.wrapping_sub(1);
        let prev_spans = Self::tile_skip_spans(prev_idx, width, height);
        let curr_spans = Self::tile_skip_spans(frame_idx, width, height);

        for (x, y, len) in prev_spans {
            for dx in 0..len {
                frame
                    .buffer
                    .set_raw(x.saturating_add(dx), y, Cell::default());
            }
        }

        for (x, y, len) in curr_spans {
            for dx in 0..len {
                frame
                    .buffer
                    .set_raw(x.saturating_add(dx), y, Cell::from_char('X'));
            }
        }
    }

    fn view_span_diff(&self, frame: &mut Frame) {
        self.apply_theme_base(frame);
        frame.buffer.clear_dirty();

        let width = frame.buffer.width();
        let height = frame.buffer.height();
        if width == 0 || height == 0 {
            return;
        }

        let frame_idx = self.spinner_state.current_frame as u16;
        let prev_idx = frame_idx.wrapping_sub(1);
        let prev_positions = Self::span_positions(prev_idx, width, height);
        let curr_positions = Self::span_positions(frame_idx, width, height);

        for (x, y) in prev_positions {
            frame.buffer.set_raw(x, y, Cell::default());
        }

        for (x, y) in curr_positions {
            frame.buffer.set_raw(x, y, Cell::from_char('X'));
        }
    }

    fn view_selector_storm(&self, frame: &mut Frame) {
        self.apply_theme_base(frame);
        frame.buffer.clear_dirty();

        let width = frame.buffer.width();
        let height = frame.buffer.height();
        if width == 0 || height == 0 {
            return;
        }

        let frame_idx = self.spinner_state.current_frame as u16;
        let phase_len = 6_u16;
        let phase = (frame_idx / phase_len) % 2;

        if phase == 0 {
            let span_len = (width / 10).clamp(2, 12);
            let rows = [0, height / 2, height.saturating_sub(1)];
            for (i, row) in rows.iter().enumerate() {
                if *row >= height {
                    continue;
                }
                let offset = frame_idx.wrapping_add((i as u16).saturating_mul(7));
                let start = offset % width;
                for dx in 0..span_len {
                    let x = start.saturating_add(dx);
                    if x >= width {
                        break;
                    }
                    frame.buffer.set_raw(x, *row, Cell::from_char('x'));
                }
            }
        } else {
            let fill_char = if frame_idx.is_multiple_of(2) {
                '#'
            } else {
                '@'
            };
            if width <= 1 || height <= 1 {
                frame.buffer.set_raw(0, 0, Cell::from_char(fill_char));
                return;
            }
            let x_end = width.saturating_sub(1);
            let y_end = height.saturating_sub(1);
            for y in 1..y_end {
                for x in 1..x_end {
                    frame.buffer.set_raw(x, y, Cell::from_char(fill_char));
                }
            }
        }
    }

    fn seed_effect_queue_cmd(&self) -> Cmd<Msg> {
        let jobs = [
            ("high_weight_short", 5.0_f64, 20.0_f64),
            ("mid_weight_mid", 2.0_f64, 50.0_f64),
            ("low_weight_long", 1.0_f64, 120.0_f64),
            ("high_weight_long", 4.0_f64, 200.0_f64),
        ];

        let mut cmds = Vec::new();
        for (name, weight, estimate_ms) in jobs {
            let spec = TaskSpec::new(weight, estimate_ms).with_name(name);
            let label = format!("EffectQueue done: {name}");
            cmds.push(Cmd::task_with_spec(spec, move || Msg::LogLine(label)));
        }

        Cmd::Batch(cmds)
    }

    fn view_effect_queue(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let text = [
            "Effect Queue Scheduling",
            "Enqueued tasks: high/medium/low weights",
            "Check evidence JSONL for effect_queue_select ordering",
        ]
        .join("\n");
        Paragraph::new(text).render(area, frame);
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

    fn view_widget_budget(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(33.4),
                Constraint::Percentage(33.3),
                Constraint::Percentage(33.3),
            ])
            .split(area);

        let specs = [
            BudgetCardSpec {
                widget_id: 1,
                title: " Essential ",
                body: "Core status",
                essential: true,
                priority: 1.0,
                staleness_ms: 0,
                cost_us: 1_500.0,
                focus_boost: 0.0,
                interaction_boost: 0.0,
            },
            BudgetCardSpec {
                widget_id: 2,
                title: " Search ",
                body: "Hot path",
                essential: false,
                priority: 1.2,
                staleness_ms: 200,
                cost_us: 2_000.0,
                focus_boost: 0.0,
                interaction_boost: 0.0,
            },
            BudgetCardSpec {
                widget_id: 3,
                title: " Starved ",
                body: "Needs refresh",
                essential: false,
                priority: 0.5,
                staleness_ms: 4_000,
                cost_us: 1_200.0,
                focus_boost: 0.0,
                interaction_boost: 0.0,
            },
            BudgetCardSpec {
                widget_id: 4,
                title: " Charts ",
                body: "Analytics",
                essential: false,
                priority: 0.9,
                staleness_ms: 600,
                cost_us: 2_500.0,
                focus_boost: 0.0,
                interaction_boost: 0.0,
            },
            BudgetCardSpec {
                widget_id: 5,
                title: " Logs ",
                body: "Low pri",
                essential: false,
                priority: 0.2,
                staleness_ms: 500,
                cost_us: 500.0,
                focus_boost: 0.0,
                interaction_boost: 0.0,
            },
            BudgetCardSpec {
                widget_id: 6,
                title: " Preview ",
                body: "Inline view",
                essential: false,
                priority: 0.7,
                staleness_ms: 900,
                cost_us: 1_800.0,
                focus_boost: 0.0,
                interaction_boost: 0.0,
            },
        ];

        let mut idx = 0usize;
        for row in rows {
            let cols = Flex::horizontal()
                .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
                .split(row);
            for col in cols {
                if let Some(spec) = specs.get(idx) {
                    let card = BudgetCard {
                        title: spec.title,
                        body: spec.body,
                        essential: spec.essential,
                    };
                    let mut signal = WidgetSignal::new(spec.widget_id);
                    signal.priority = spec.priority;
                    signal.staleness_ms = spec.staleness_ms;
                    signal.cost_estimate_us = spec.cost_us;
                    signal.focus_boost = spec.focus_boost;
                    signal.interaction_boost = spec.interaction_boost;

                    Budgeted::new(spec.widget_id, card)
                        .with_signal(signal)
                        .render(col, frame);
                }
                idx = idx.saturating_add(1);
            }
        }
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

    fn view_widget_inspector(&self, frame: &mut Frame) {
        frame.enable_hit_testing();

        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(55.0), Constraint::Percentage(45.0)])
            .split(area);

        let left_block = Block::new()
            .title(" Logs ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));
        let left_inner = left_block.inner(chunks[0]);
        left_block.render(chunks[0], frame);

        let log_text = Paragraph::new("Search results\n- hit: button\n- hit: list\n- hit: panel")
            .wrap(WrapMode::Word);
        log_text.render(left_inner, frame);

        let right_block = Block::new()
            .title(" Actions ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));
        let right_inner = right_block.inner(chunks[1]);
        right_block.render(chunks[1], frame);

        let details = Paragraph::new("Hover + selection\nshould show in\nInspector panel.")
            .alignment(Alignment::Left)
            .wrap(WrapMode::Word);
        details.render(right_inner, frame);

        let button_hit = Rect::new(
            right_inner.x + 2,
            right_inner.y + 1,
            right_inner.width.saturating_sub(4),
            3,
        );
        frame.register_hit(button_hit, HitId::new(2), HitRegion::Button, 7);
        frame.register_hit(left_inner, HitId::new(1), HitRegion::Content, 0);

        let mut inspector = InspectorState::new();
        inspector.mode = InspectorMode::Full;
        inspector.show_detail_panel = true;
        if self.inspector_stress {
            self.populate_inspector_stress(area, frame, &mut inspector);
        } else {
            inspector.selected = Some(HitId::new(1));
            inspector.hover_pos = Some((button_hit.x + 1, button_hit.y + 1));

            let mut root = WidgetInfo::new("Harness", area).with_depth(0);
            let mut left_info = WidgetInfo::new("LogPanel", chunks[0])
                .with_depth(1)
                .with_hit_id(HitId::new(1));
            left_info.add_child(WidgetInfo::new("LogText", left_inner).with_depth(2));
            let mut right_info = WidgetInfo::new("ActionPanel", chunks[1])
                .with_depth(1)
                .with_hit_id(HitId::new(2));
            right_info.add_child(WidgetInfo::new("PrimaryButton", button_hit).with_depth(2));
            root.add_child(left_info);
            root.add_child(right_info);
            inspector.register_widget(root);
        }

        InspectorOverlay::new(&inspector).render(area, frame);
    }

    fn view_locale_context(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let block = Block::new()
            .title(" Locale Context ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));
        let inner = block.inner(area);
        block.render(area, frame);

        let ctx = LocaleContext::global();
        let base_locale = ctx.base_locale();
        let version = ctx.version();
        let system_locale = detect_system_locale();
        let override_label = self.locale_override.as_deref().unwrap_or("none");
        let switch_target = self.locale_switch_target.as_deref().unwrap_or("none");
        let switch_ticks = self
            .locale_switch_ticks
            .map(|ticks| ticks.to_string())
            .unwrap_or_else(|| "none".to_string());

        let _override_guard = self
            .locale_override
            .as_ref()
            .map(|override_locale| ctx.push_override(override_locale.clone()));
        let current_locale = ctx.current_locale();

        let info_text = format!(
            "Base locale: {base}\nCurrent locale: {current}\nOverride: {override_label}\nSystem locale: {system}\nLocale version: {version}\nSwitch target: {switch_target}\nSwitch countdown: {switch_ticks}",
            base = base_locale,
            current = current_locale,
            override_label = override_label,
            system = system_locale,
            version = version,
            switch_target = switch_target,
            switch_ticks = switch_ticks,
        );

        let info = Paragraph::new(info_text)
            .wrap(WrapMode::Word)
            .alignment(Alignment::Left);
        info.render(inner, frame);
    }

    fn populate_inspector_stress(
        &self,
        area: Rect,
        frame: &mut Frame,
        inspector: &mut InspectorState,
    ) {
        let cols = self.inspector_grid_cols.max(1);
        let rows = self.inspector_grid_rows.max(1);
        let cell_width = (area.width / cols).max(1);
        let cell_height = (area.height / rows).max(1);

        let mut root = WidgetInfo::new("HarnessStress", area).with_depth(0);
        let mut id_counter: u32 = 1;
        let mut selected_set = false;

        for row in 0..rows {
            let y = area.y.saturating_add(row.saturating_mul(cell_height));
            if y >= area.bottom() {
                break;
            }
            let height = area.bottom().saturating_sub(y).min(cell_height);
            if height == 0 {
                continue;
            }

            for col in 0..cols {
                let x = area.x.saturating_add(col.saturating_mul(cell_width));
                if x >= area.right() {
                    break;
                }
                let width = area.right().saturating_sub(x).min(cell_width);
                if width == 0 {
                    continue;
                }

                let rect = Rect::new(x, y, width, height);
                let mut widget = self.build_inspector_chain(
                    format!("Cell {col},{row}"),
                    rect,
                    1,
                    self.inspector_depth,
                );
                widget.hit_id = Some(HitId::new(id_counter));

                frame.register_hit(
                    rect,
                    HitId::new(id_counter),
                    HitRegion::Content,
                    u64::from(id_counter),
                );

                if !selected_set {
                    inspector.selected = Some(HitId::new(id_counter));
                    inspector.hover_pos = Some((
                        rect.x.saturating_add(rect.width / 2),
                        rect.y.saturating_add(rect.height / 2),
                    ));
                    selected_set = true;
                }

                root.add_child(widget);
                id_counter = id_counter.saturating_add(1);
            }
        }

        inspector.register_widget(root);
    }

    fn build_inspector_chain(
        &self,
        name: String,
        area: Rect,
        depth: u8,
        max_depth: u8,
    ) -> WidgetInfo {
        let mut widget = WidgetInfo::new(name, area).with_depth(depth);

        if depth < max_depth {
            let next_depth = depth.saturating_add(1);
            let child_area = Rect::new(
                area.x.saturating_add(1),
                area.y.saturating_add(1),
                area.width.saturating_sub(2),
                area.height.saturating_sub(2),
            );
            if !child_area.is_empty() {
                let child = self.build_inspector_chain(
                    format!("Depth {}", next_depth),
                    child_area,
                    next_depth,
                    max_depth,
                );
                widget.add_child(child);
            }
        }

        widget
    }

    /// Render the tree view overlay (toggled via Esc Esc).
    fn view_tree_overlay(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());

        // Layout: Tree view (70%), Info panel (30%)
        let chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(70.0), Constraint::Percentage(30.0)])
            .split(area);

        // Create a demo tree representing the harness state
        // TreeNode is expanded by default, so no need to call with_expanded(true)
        let tree_root = TreeNode::new("Harness State")
            .child(
                TreeNode::new("Model")
                    .child(TreeNode::new(format!("name: {}", self.model_name)))
                    .child(TreeNode::new(format!("commands: {}", self.command_count))),
            )
            .child(
                TreeNode::new("Task")
                    .child(TreeNode::new(format!("running: {}", self.task_running)))
                    .child(TreeNode::new(format!(
                        "tool: {}",
                        self.current_tool.as_deref().unwrap_or("none")
                    ))),
            )
            .child(
                TreeNode::new("View")
                    .child(TreeNode::new(format!("mode: {:?}", self.view_mode)))
                    .child(TreeNode::new("tree_open: true")),
            )
            .child(
                TreeNode::new("Input")
                    .child(TreeNode::new(format!("value: \"{}\"", self.input.value())))
                    .child(TreeNode::new(format!("cursor: {}", self.input.cursor()))),
            );

        // Render block first, then tree inside it
        let tree_block = Block::new()
            .title(" Tree View (Esc Esc to close) ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::accent::SECONDARY));
        let tree_inner = tree_block.inner(chunks[0]);
        tree_block.render(chunks[0], frame);

        let tree = Tree::new(tree_root);
        tree.render(tree_inner, frame);

        // Info panel
        let info_text = format!(
            "Tree View Overlay\n\n\
             Press Esc Esc to toggle\n\n\
             This shows the harness state:\n\
             - Model configuration\n\
             - Task status\n\
             - View mode\n\
             - Input state\n\n\
             Log lines: {}",
            self.log_viewer.line_count()
        );

        let info_block = Block::new()
            .title(" Info ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(theme::alpha::SURFACE))
            .border_style(Style::new().fg(theme::fg::MUTED));
        let info_inner = info_block.inner(chunks[1]);
        info_block.render(chunks[1], frame);

        let info_paragraph = Paragraph::new(info_text)
            .wrap(WrapMode::Word)
            .alignment(Alignment::Left);
        info_paragraph.render(info_inner, frame);
    }
}

fn env_flag(name: &str) -> Option<bool> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let enabled = matches!(
        trimmed,
        "1" | "true" | "TRUE" | "True" | "yes" | "YES" | "on" | "ON"
    );
    Some(enabled)
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn main() -> std::io::Result<()> {
    if std::env::var("FTUI_HARNESS_FLICKER_ANALYZE").is_ok() {
        let input_path = std::env::var("FTUI_HARNESS_FLICKER_INPUT").map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "FTUI_HARNESS_FLICKER_INPUT must be set",
            )
        })?;
        let run_id =
            std::env::var("FTUI_HARNESS_FLICKER_RUN_ID").unwrap_or_else(|_| "flicker".into());
        let bytes = std::fs::read(&input_path)?;
        let analysis = flicker_detection::analyze_stream_with_id(&run_id, &bytes);

        if let Ok(output_path) = std::env::var("FTUI_HARNESS_FLICKER_JSONL") {
            std::fs::write(&output_path, analysis.jsonl.as_bytes())?;
        } else {
            print!("{}", analysis.jsonl);
        }

        if !analysis.flicker_free {
            std::process::exit(2);
        }

        return Ok(());
    }

    if let Ok(trace_path) = std::env::var("FTUI_HARNESS_REPLAY_TRACE") {
        match ftui_harness::trace_replay::replay_trace(Path::new(&trace_path)) {
            Ok(summary) => {
                eprintln!(
                    "trace replay OK: frames={} last_checksum={}",
                    summary.frames,
                    summary
                        .last_checksum
                        .map(|v| format!("{v:016x}"))
                        .unwrap_or_else(|| "none".to_string())
                );
                return Ok(());
            }
            Err(err) => {
                eprintln!("trace replay failed: {err}");
                std::process::exit(2);
            }
        }
    }

    let input_mode = std::env::var("FTUI_HARNESS_INPUT_MODE")
        .unwrap_or_else(|_| "runtime".to_string())
        .to_ascii_lowercase();

    if input_mode == "parser" || input_mode == "input-parser" || input_mode == "input_parser" {
        return run_input_trace(parse_exit_after());
    }

    #[cfg(feature = "telemetry")]
    let _telemetry_guard = match TelemetryConfig::from_env().install() {
        Ok(guard) => Some(guard),
        Err(err) => {
            eprintln!("Telemetry init failed: {err}");
            None
        }
    };

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
        "widget-inspector" | "widget_inspector" | "inspector" => HarnessView::WidgetInspector,
        "widget-budget" | "widget_budget" | "budget" => HarnessView::WidgetBudget,
        "tile-skip" | "tile_skip" | "tile" => HarnessView::TileSkip,
        "span-diff" | "span_diff" | "span" => HarnessView::SpanDiff,
        "selector-storm" | "selector_storm" | "selector" => HarnessView::SelectorStorm,
        "locale-context" | "locale_context" | "locale" => HarnessView::LocaleContext,
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

    let locale_base = std::env::var("FTUI_HARNESS_LOCALE").ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let mut config = ProgramConfig {
        screen_mode,
        mouse: enable_mouse,
        focus_reporting: enable_focus,
        kitty_keyboard: enable_kitty_keyboard,
        ..Default::default()
    };
    if let Some(locale) = locale_base {
        config = config.with_locale(locale);
    }
    if let Some(enabled) = env_flag("FTUI_HARNESS_DIFF_BAYESIAN") {
        config.diff_config = config.diff_config.with_bayesian_enabled(enabled);
    }
    if let Some(enabled) = env_flag("FTUI_HARNESS_BOCPD") {
        if enabled {
            config.resize_coalescer = config.resize_coalescer.with_bocpd();
        } else {
            config.resize_coalescer.enable_bocpd = false;
            config.resize_coalescer.bocpd_config = None;
        }
    }
    if let Some(enabled) = env_flag("FTUI_HARNESS_CONFORMAL") {
        if enabled {
            config = config.with_conformal_config(ConformalConfig::default());
        } else {
            config = config.without_conformal();
        }
    }
    if let Some(total_us) = env_u64("FTUI_HARNESS_FRAME_BUDGET_US") {
        config.budget.total = Duration::from_micros(total_us);
    }
    if let Some(render_us) = env_u64("FTUI_HARNESS_RENDER_BUDGET_US") {
        config.budget.phase_budgets.render = Duration::from_micros(render_us);
        let phase_total = config.budget.phase_budgets.diff
            + config.budget.phase_budgets.present
            + config.budget.phase_budgets.render;
        if phase_total > config.budget.total {
            config.budget.total = phase_total;
        }
    }
    if let Ok(path) = std::env::var("FTUI_HARNESS_EVIDENCE_JSONL") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            config = config.with_evidence_sink(EvidenceSinkConfig::enabled_file(trimmed));
        }
    }
    if let Some(path) = env_string("FTUI_HARNESS_RENDER_TRACE_JSONL") {
        let mut trace_config = RenderTraceConfig::enabled_file(path);
        if let Some(run_id) = env_string("FTUI_HARNESS_RENDER_TRACE_RUN_ID") {
            trace_config = trace_config.with_run_id(run_id);
        }
        if let Some(seed) = env_u64("FTUI_HARNESS_RENDER_TRACE_SEED") {
            trace_config = trace_config.with_seed(seed);
        }
        if let Some(module) = env_string("FTUI_HARNESS_RENDER_TRACE_MODULE") {
            trace_config = trace_config.with_test_module(module);
        }
        if let Some(enabled) = env_flag("FTUI_HARNESS_RENDER_TRACE_FLUSH") {
            trace_config = trace_config.with_flush_on_write(enabled);
        }
        if let Some(enabled) = env_flag("FTUI_HARNESS_RENDER_TRACE_START_TS_MS") {
            trace_config = trace_config.with_start_ts_ms(enabled);
        }
        config.render_trace = trace_config;
    }

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
