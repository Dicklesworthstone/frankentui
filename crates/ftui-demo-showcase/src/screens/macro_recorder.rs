#![forbid(unsafe_code)]

//! Macro Recorder screen — record, replay, and visualize input macros.
//!
//! Demonstrates:
//! - `FilteredEventRecorder` for live event capture
//! - Deterministic playback with speed control
//! - Timeline and scenario runner panels

use std::cell::Cell;
use web_time::{Duration, Instant};

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseEvent};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_runtime::input_macro::{
    FilteredEventRecorder, InputMacro, MacroMetadata, MacroPlayback, RecordingFilter, TimedEvent,
};
use ftui_style::Style;
use ftui_text::{Line, Span, Text};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

const TICK_MS: u64 = 100;
const SPEED_MIN: f64 = 0.25;
const SPEED_MAX: f64 = 4.0;
const SPEED_STEP: f64 = 0.25;
const MAX_EVENT_LINES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPanel {
    Controls,
    Timeline,
    Scenarios,
}

impl FocusPanel {
    fn next(self) -> Self {
        match self {
            Self::Controls => Self::Timeline,
            Self::Timeline => Self::Scenarios,
            Self::Scenarios => Self::Controls,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Controls => Self::Scenarios,
            Self::Timeline => Self::Controls,
            Self::Scenarios => Self::Timeline,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UiState {
    Idle,
    Recording,
    Stopped,
    Playing,
    Error(String),
}

#[derive(Debug, Clone, Copy)]
struct ScenarioInfo {
    name: &'static str,
    description: &'static str,
}

const SCENARIOS: &[ScenarioInfo] = &[
    ScenarioInfo {
        name: "Tab Tour",
        description: "Cycle tabs and toggle the help overlay",
    },
    ScenarioInfo {
        name: "Search Flow",
        description: "Open palette, type a target, confirm",
    },
    ScenarioInfo {
        name: "Layout Lab",
        description: "Cycle screens and n/p adjustments",
    },
];

pub struct MacroRecorderScreen {
    state: UiState,
    recorder: Option<FilteredEventRecorder>,
    macro_data: Option<InputMacro>,
    playback: Option<MacroPlayback>,
    playback_last_tick: Option<u64>,
    pending_playback: Vec<Event>,
    recording_started: Option<Instant>,
    recorded_events: usize,
    filtered_events: usize,
    speed: f64,
    looping: bool,
    terminal_size: (u16, u16),
    last_tick_count: u64,
    focus: FocusPanel,
    timeline_cursor: usize,
    scenario_index: usize,
    status_note: Option<String>,
    layout_controls: Cell<Rect>,
    layout_timeline: Cell<Rect>,
    layout_scenarios: Cell<Rect>,
}

impl Default for MacroRecorderScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl MacroRecorderScreen {
    pub fn new() -> Self {
        Self {
            state: UiState::Idle,
            recorder: None,
            macro_data: None,
            playback: None,
            playback_last_tick: None,
            pending_playback: Vec::new(),
            recording_started: None,
            recorded_events: 0,
            filtered_events: 0,
            speed: 1.0,
            looping: false,
            terminal_size: (80, 24),
            last_tick_count: 0,
            focus: FocusPanel::Controls,
            timeline_cursor: 0,
            scenario_index: 0,
            status_note: Some(
                "Press Space to record or Enter in Scenarios to load a demo".to_string(),
            ),
            layout_controls: Cell::new(Rect::default()),
            layout_timeline: Cell::new(Rect::default()),
            layout_scenarios: Cell::new(Rect::default()),
        }
    }

    pub fn record_event(&mut self, event: &Event, filter_controls: bool) {
        let Some(recorder) = &mut self.recorder else {
            return;
        };
        if !recorder.is_recording() {
            return;
        }
        if filter_controls && is_control_key(event) {
            return;
        }
        if recorder.record(event) {
            self.recorded_events = recorder.event_count();
            self.filtered_events = recorder.filtered_count();
        }
    }

    pub fn set_terminal_size(&mut self, width: u16, height: u16) {
        self.terminal_size = (width, height);
    }

    fn set_focus(&mut self, focus: FocusPanel) {
        self.focus = focus;
        let label = match focus {
            FocusPanel::Controls => "Controls",
            FocusPanel::Timeline => "Timeline",
            FocusPanel::Scenarios => "Scenarios",
        };
        self.status_note = Some(format!(
            "Focus: {label} (Tab or Alt+Arrows: \u{2191}Controls \u{2190}Timeline \u{2192}Scenarios; Ctrl+\u{2190}/\u{2192} jumps timeline)"
        ));
    }

    pub fn drain_playback_events(&mut self) -> Vec<Event> {
        if self.pending_playback.is_empty() {
            return Vec::new();
        }
        std::mem::take(&mut self.pending_playback)
    }

    fn start_recording(&mut self) {
        let filter = RecordingFilter::keys_only();
        let recorder = FilteredEventRecorder::new("macro", filter)
            .with_terminal_size(self.terminal_size.0, self.terminal_size.1);
        self.recorder = Some(recorder);
        if let Some(recorder) = &mut self.recorder {
            recorder.start();
        }
        self.recorded_events = 0;
        self.filtered_events = 0;
        self.macro_data = None;
        self.playback = None;
        self.playback_last_tick = None;
        self.recording_started = Some(Instant::now());
        self.state = UiState::Recording;
        self.timeline_cursor = 0;
        self.status_note = Some("Recording… navigate anywhere to capture inputs".to_string());
    }

    fn stop_recording(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        let recorded = recorder.event_count();
        let filtered = recorder.filtered_count();
        let macro_data = recorder.finish();
        self.recorded_events = recorded;
        self.filtered_events = filtered;
        self.macro_data = Some(macro_data);
        self.playback = None;
        self.playback_last_tick = None;
        self.recording_started = None;
        self.state = UiState::Stopped;
        self.clamp_timeline_cursor();
        self.status_note = Some(format!("Recorded {recorded} events"));
    }

    fn start_playback(&mut self, tick_count: u64) {
        let Some(macro_data) = &self.macro_data else {
            tracing::warn!(macro_event = "playback_error", reason = "no_macro",);
            self.state =
                UiState::Error("No macro recorded — press 'r' to start recording".to_string());
            return;
        };
        if macro_data.is_empty() {
            tracing::warn!(
                macro_event = "playback_error",
                reason = "macro_empty",
                name = %macro_data.metadata().name,
            );
            self.state =
                UiState::Error("Macro is empty — record some keystrokes first".to_string());
            return;
        }
        let mut playback = MacroPlayback::new(macro_data.clone())
            .with_speed(self.speed)
            .with_looping(self.looping);
        playback.reset();
        self.playback = Some(playback);
        self.playback_last_tick = Some(tick_count);
        self.state = UiState::Playing;
        self.status_note = None;
    }

    fn pause_playback(&mut self) {
        if self.playback.is_some() {
            self.state = UiState::Stopped;
        }
    }

    fn stop_playback(&mut self) {
        if let Some(macro_data) = &self.macro_data {
            tracing::info!(
                macro_event = "playback_stop",
                reason = "manual",
                name = %macro_data.metadata().name,
                events = macro_data.len(),
            );
        }
        self.playback = None;
        self.playback_last_tick = None;
        self.state = UiState::Stopped;
        self.status_note = Some("Playback stopped".to_string());
    }

    fn toggle_playback(&mut self, tick_count: u64) {
        match self.state {
            UiState::Playing => self.pause_playback(),
            UiState::Stopped => {
                if self.playback.is_some() {
                    self.state = UiState::Playing;
                    self.playback_last_tick = Some(tick_count);
                } else {
                    self.start_playback(tick_count);
                }
            }
            UiState::Idle => self.start_playback(tick_count),
            UiState::Recording => {}
            UiState::Error(_) => {
                if self.macro_data.is_some() {
                    self.state = UiState::Stopped;
                } else {
                    self.state = UiState::Idle;
                }
            }
        }
    }

    fn toggle_loop(&mut self) {
        self.looping = !self.looping;
        if let Some(playback) = &mut self.playback {
            playback.set_looping(self.looping);
        }
        self.status_note = Some(format!(
            "Looping {}",
            if self.looping { "enabled" } else { "disabled" }
        ));
    }

    fn adjust_speed(&mut self, delta: f64) {
        let mut speed = self.speed + delta;
        speed = speed.clamp(SPEED_MIN, SPEED_MAX);
        self.speed = speed;
        if let Some(playback) = &mut self.playback {
            playback.set_speed(self.speed);
        }
        self.status_note = Some(format!("Speed set to {:.2}x", self.speed));
    }

    fn set_macro_data(&mut self, macro_data: InputMacro) {
        self.macro_data = Some(macro_data);
        self.playback = None;
        self.playback_last_tick = None;
        self.recording_started = None;
        self.state = UiState::Stopped;
        self.clamp_timeline_cursor();
    }

    fn clamp_timeline_cursor(&mut self) {
        let len = self.macro_data.as_ref().map(|m| m.len()).unwrap_or(0);
        if len == 0 {
            self.timeline_cursor = 0;
        } else {
            self.timeline_cursor = self.timeline_cursor.min(len - 1);
        }
    }

    fn move_timeline_cursor(&mut self, delta: i32) {
        let len = self.macro_data.as_ref().map(|m| m.len()).unwrap_or(0) as i32;
        if len == 0 {
            self.timeline_cursor = 0;
            return;
        }
        let next = (self.timeline_cursor as i32 + delta).clamp(0, len - 1);
        self.timeline_cursor = next as usize;
    }

    fn scenario_macro(&self, scenario: ScenarioInfo) -> InputMacro {
        let steps: Vec<(Event, u64)> = match scenario.name {
            "Tab Tour" => vec![
                (key_event(KeyCode::Tab, Modifiers::NONE), 180),
                (key_event(KeyCode::Tab, Modifiers::NONE), 180),
                (key_event(KeyCode::Tab, Modifiers::NONE), 180),
                (key_event(KeyCode::Char('?'), Modifiers::NONE), 250),
                (key_event(KeyCode::BackTab, Modifiers::SHIFT), 200),
                (key_event(KeyCode::BackTab, Modifiers::SHIFT), 200),
                (key_event(KeyCode::Char('?'), Modifiers::NONE), 250),
            ],
            "Search Flow" => vec![
                (key_event(KeyCode::Char('k'), Modifiers::CTRL), 200),
                (key_event(KeyCode::Char('s'), Modifiers::NONE), 120),
                (key_event(KeyCode::Char('h'), Modifiers::NONE), 120),
                (key_event(KeyCode::Char('a'), Modifiers::NONE), 120),
                (key_event(KeyCode::Enter, Modifiers::NONE), 260),
                (key_event(KeyCode::Char('?'), Modifiers::NONE), 250),
            ],
            "Layout Lab" => vec![
                (key_event(KeyCode::Tab, Modifiers::NONE), 200),
                (key_event(KeyCode::Tab, Modifiers::NONE), 200),
                (key_event(KeyCode::Tab, Modifiers::NONE), 200),
                (key_event(KeyCode::Char('n'), Modifiers::NONE), 200),
                (key_event(KeyCode::Char('p'), Modifiers::NONE), 200),
                (key_event(KeyCode::Char('n'), Modifiers::NONE), 200),
                (key_event(KeyCode::Char('t'), Modifiers::CTRL), 220),
            ],
            _ => vec![],
        };

        let mut total = Duration::ZERO;
        let mut events = Vec::with_capacity(steps.len());
        for (event, delay_ms) in steps {
            let delay = Duration::from_millis(delay_ms);
            total += delay;
            events.push(TimedEvent::new(event, delay));
        }

        InputMacro::new(
            events,
            MacroMetadata {
                name: scenario.name.to_string(),
                terminal_size: self.terminal_size,
                total_duration: total,
            },
        )
    }

    fn handle_controls(&mut self, event: &Event) {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event
        else {
            return;
        };
        if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }

        let (code, modifiers) = (*code, *modifiers);

        let focus_jump = modifiers.intersects(Modifiers::ALT | Modifiers::SHIFT | Modifiers::SUPER)
            || (modifiers.contains(Modifiers::CTRL) && self.focus != FocusPanel::Timeline);
        if focus_jump {
            match code {
                KeyCode::Up => {
                    self.set_focus(FocusPanel::Controls);
                    return;
                }
                KeyCode::Down => {
                    self.set_focus(FocusPanel::Timeline);
                    return;
                }
                KeyCode::Left => {
                    self.set_focus(FocusPanel::Timeline);
                    return;
                }
                KeyCode::Right => {
                    self.set_focus(FocusPanel::Scenarios);
                    return;
                }
                _ => {}
            }
        }

        match (code, modifiers) {
            (KeyCode::Tab, Modifiers::NONE) => {
                let next = self.focus.next();
                self.set_focus(next);
                return;
            }
            (KeyCode::BackTab, _) => {
                let next = self.focus.prev();
                self.set_focus(next);
                return;
            }
            _ => {}
        }

        if self.focus == FocusPanel::Timeline {
            if modifiers.contains(Modifiers::CTRL) {
                match code {
                    KeyCode::Up => {
                        self.move_timeline_cursor(-(MAX_EVENT_LINES as i32 / 2).max(1));
                        return;
                    }
                    KeyCode::Down => {
                        self.move_timeline_cursor((MAX_EVENT_LINES as i32 / 2).max(1));
                        return;
                    }
                    KeyCode::Left => {
                        self.timeline_cursor = 0;
                        return;
                    }
                    KeyCode::Right => {
                        if let Some(macro_data) = &self.macro_data
                            && !macro_data.is_empty()
                        {
                            self.timeline_cursor = macro_data.len() - 1;
                        }
                        return;
                    }
                    _ => {}
                }
            }
            match code {
                KeyCode::Up => {
                    self.move_timeline_cursor(-1);
                    return;
                }
                KeyCode::Down => {
                    self.move_timeline_cursor(1);
                    return;
                }
                KeyCode::PageUp => {
                    self.move_timeline_cursor(-(MAX_EVENT_LINES as i32 / 2).max(1));
                    return;
                }
                KeyCode::PageDown => {
                    self.move_timeline_cursor((MAX_EVENT_LINES as i32 / 2).max(1));
                    return;
                }
                KeyCode::Home => {
                    self.timeline_cursor = 0;
                    return;
                }
                KeyCode::End => {
                    if let Some(macro_data) = &self.macro_data
                        && !macro_data.is_empty()
                    {
                        self.timeline_cursor = macro_data.len() - 1;
                    }
                    return;
                }
                KeyCode::Enter => {
                    self.toggle_playback(self.last_tick_count);
                    return;
                }
                _ => {}
            }
        }

        if self.focus == FocusPanel::Scenarios {
            match code {
                KeyCode::Up => {
                    if self.scenario_index == 0 {
                        self.scenario_index = SCENARIOS.len().saturating_sub(1);
                    } else {
                        self.scenario_index = self.scenario_index.saturating_sub(1);
                    }
                    return;
                }
                KeyCode::Down => {
                    self.scenario_index = (self.scenario_index + 1) % SCENARIOS.len();
                    return;
                }
                KeyCode::Enter => {
                    let scenario = SCENARIOS[self.scenario_index];
                    let macro_data = self.scenario_macro(scenario);
                    self.set_macro_data(macro_data);
                    self.status_note = Some(format!("Loaded scenario: {}", scenario.name));
                    return;
                }
                _ => {}
            }
        }

        match (code, modifiers) {
            (KeyCode::Char('r'), Modifiers::NONE) => {
                if self.state == UiState::Recording {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }
            (KeyCode::Char('p'), Modifiers::NONE) => self.toggle_playback(self.last_tick_count),
            (KeyCode::Char('l'), Modifiers::NONE) => self.toggle_loop(),
            (KeyCode::Char('+'), Modifiers::NONE) | (KeyCode::Char('='), Modifiers::NONE) => {
                self.adjust_speed(SPEED_STEP)
            }
            (KeyCode::Char('-'), Modifiers::NONE) => self.adjust_speed(-SPEED_STEP),
            (KeyCode::Char(' '), Modifiers::NONE) => {
                if self.state == UiState::Recording {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }
            (KeyCode::Enter, Modifiers::NONE) => self.toggle_playback(self.last_tick_count),
            (KeyCode::Escape, _) => {
                if self.state == UiState::Recording {
                    self.stop_recording();
                } else if self.state == UiState::Playing {
                    self.stop_playback();
                } else if matches!(&self.state, UiState::Error(_)) {
                    if self.macro_data.is_some() {
                        self.state = UiState::Stopped;
                    } else {
                        self.state = UiState::Idle;
                    }
                }
            }
            _ => {}
        }
    }

    fn playback_tick(&mut self, tick_count: u64) {
        if self.state != UiState::Playing {
            return;
        }
        if self.playback.is_none() {
            // Defensive: keep UI consistent if playback disappears unexpectedly.
            self.state = UiState::Stopped;
            self.playback_last_tick = None;
            self.status_note = Some("Playback halted (missing macro)".to_string());
            return;
        }

        let last_tick = self.playback_last_tick.get_or_insert(tick_count);
        let delta_ticks = tick_count.saturating_sub(*last_tick).max(1);
        *last_tick = tick_count;

        let playback = match self.playback.as_mut() {
            Some(playback) => playback,
            None => {
                self.state = UiState::Stopped;
                self.playback_last_tick = None;
                self.status_note = Some("Playback halted (missing macro)".to_string());
                return;
            }
        };
        let delta = Duration::from_millis(delta_ticks * TICK_MS);
        let events = playback.advance(delta);
        let done = playback.is_done();
        let playhead_pos = playback.position();

        self.pending_playback.extend(events);
        if done {
            self.state = UiState::Stopped;
            self.playback = None;
            self.playback_last_tick = None;
            self.status_note = Some("Playback complete".to_string());
        }

        if self.focus != FocusPanel::Timeline
            && let Some(macro_data) = &self.macro_data
            && !macro_data.is_empty()
        {
            let pos = playhead_pos.min(macro_data.len() - 1);
            self.timeline_cursor = pos;
        }
    }

    fn render_controls_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == FocusPanel::Controls,
            theme::screen_accent::ADVANCED,
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Macro Recorder")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        // A11y: state labels include Unicode symbols so states are distinguishable
        // without color (bd-2lus.9).
        let state_label = match &self.state {
            UiState::Idle => "○ Idle",
            UiState::Recording => "● Recording",
            UiState::Stopped => {
                if self.playback.is_some() {
                    "⏸ Paused"
                } else {
                    "■ Stopped"
                }
            }
            UiState::Playing => "▶ Playing",
            UiState::Error(_) => "⚠ Error",
        };

        // A11y: Recording uses italic, Error uses underline so they are
        // visually distinct beyond color alone (bd-2lus.9).
        let state_style = match &self.state {
            UiState::Recording => Style::new().fg(theme::accent::ERROR).bold().italic(),
            UiState::Playing => Style::new().fg(theme::accent::SUCCESS).bold(),
            UiState::Error(_) => Style::new().fg(theme::accent::WARNING).bold().underline(),
            UiState::Stopped => Style::new().fg(theme::accent::WARNING),
            UiState::Idle => Style::new().fg(theme::fg::MUTED),
        };

        let duration = self
            .recording_started
            .map(|t| t.elapsed())
            .or_else(|| self.macro_data.as_ref().map(|m| m.total_duration()))
            .unwrap_or_default();

        let duration_label = format_duration(duration);

        let event_count = if let Some(macro_data) = &self.macro_data {
            macro_data.len()
        } else {
            self.recorded_events
        };

        let progress = self.playback_progress();

        let focus_style = |active: bool| {
            if active {
                Style::new().fg(theme::accent::PRIMARY).bold()
            } else {
                Style::new().fg(theme::fg::MUTED)
            }
        };

        let mut lines = vec![
            Line::from_spans([
                Span::styled("State: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(state_label, state_style),
                Span::raw("   "),
                Span::styled("Events: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("{}", event_count),
                    Style::new().fg(theme::fg::PRIMARY).bold(),
                ),
                Span::raw("   "),
                Span::styled("Duration: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(duration_label, Style::new().fg(theme::fg::PRIMARY)),
            ]),
            Line::from_spans([
                Span::styled("Speed: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("{:.2}x", self.speed),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
                Span::raw("   "),
                Span::styled("Loop: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    if self.looping { "On" } else { "Off" },
                    Style::new().fg(if self.looping {
                        theme::accent::SUCCESS
                    } else {
                        theme::fg::MUTED
                    }),
                ),
                Span::raw("   "),
                Span::styled("Progress: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("{:>3.0}%", progress * 100.0),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
                Span::raw("   "),
                Span::styled("Filtered: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("{}", self.filtered_events),
                    Style::new().fg(theme::fg::MUTED),
                ),
            ]),
            Line::from_spans([
                Span::styled("Focus: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    "[Controls]",
                    focus_style(self.focus == FocusPanel::Controls),
                ),
                Span::raw("  "),
                Span::styled(
                    "[Timeline]",
                    focus_style(self.focus == FocusPanel::Timeline),
                ),
                Span::raw("  "),
                Span::styled(
                    "[Scenarios]",
                    focus_style(self.focus == FocusPanel::Scenarios),
                ),
                Span::raw("   "),
                Span::styled("Ctrl/Alt+Arrows:", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" \u{2191}Controls  \u{2190}Timeline  \u{2192}Scenarios"),
            ]),
            Line::from_spans([
                Span::styled("Quick Start: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled("Enter", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" load scenario  "),
                Span::styled("Space", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" record  "),
                Span::styled("\u{2191}/\u{2193}", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" scrub timeline"),
            ]),
            Line::from_spans([
                Span::styled("Space/r", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" record/stop  "),
                Span::styled("Enter/p", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" play/pause  "),
                Span::styled("Esc", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" stop  "),
                Span::styled("L", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" loop  "),
                Span::styled("+/-", Style::new().fg(theme::accent::PRIMARY)),
                Span::raw(" speed"),
            ]),
        ];

        if let Some(note) = &self.status_note {
            lines.push(Line::from_spans([Span::styled(
                note,
                Style::new().fg(theme::fg::SECONDARY).italic(),
            )]));
        }

        if let UiState::Error(message) = &self.state {
            // A11y: error row uses underline + distinct color to be
            // distinguishable from normal content without color (bd-2lus.9).
            lines.push(Line::from_spans([
                Span::styled(
                    "⚠ Error: ",
                    Style::new().fg(theme::accent::ERROR).bold().underline(),
                ),
                Span::styled(message, Style::new().fg(theme::fg::SECONDARY).italic()),
            ]));
        }

        let lines = Text::from_lines(lines);

        Paragraph::new(lines)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
    }

    fn render_timeline_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == FocusPanel::Timeline,
            theme::screen_accent::ADVANCED,
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Timeline")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let Some(macro_data) = &self.macro_data else {
            Paragraph::new(
                "No macro loaded.\n\
                 Space to record, or select a scenario and press Enter.",
            )
            .style(Style::new().fg(theme::fg::MUTED))
            .render(inner, frame);
            return;
        };

        let events = macro_data.events();
        if events.is_empty() {
            Paragraph::new("Macro is empty.")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
            return;
        }

        let playhead = self
            .playback
            .as_ref()
            .map(MacroPlayback::position)
            .unwrap_or(0);
        let cursor = self.timeline_cursor.min(events.len().saturating_sub(1));
        let max_lines = inner.height as usize;
        let mut lines = Vec::new();

        let visible = MAX_EVENT_LINES.min(max_lines).min(events.len());
        let anchor = if self.focus == FocusPanel::Timeline {
            cursor
        } else {
            playhead
        };
        let start = if anchor >= visible {
            anchor + 1 - visible
        } else {
            0
        };

        let mut cumulative_ms: u64 = 0;
        for (idx, timed) in events.iter().enumerate() {
            if idx < start {
                cumulative_ms += timed.delay.as_millis() as u64;
                continue;
            }
            if lines.len() >= visible {
                break;
            }
            cumulative_ms += timed.delay.as_millis() as u64;

            // A11y: use Unicode symbols instead of ASCII so playhead position
            // is distinguishable without color (bd-2lus.9).
            let marker = if self.state == UiState::Playing && idx == playhead {
                "▶"
            } else if idx < playhead {
                "●"
            } else {
                "○"
            };
            let raw_label = format_event(&timed.event);
            // A11y: truncate long labels with ellipsis so the timeline
            // stays readable at narrow widths (bd-2lus.9).
            let max_label = (inner.width as usize).saturating_sub(22);
            let label = truncate_with_ellipsis(&raw_label, max_label);
            let selected = self.focus == FocusPanel::Timeline && idx == cursor;
            let mut marker_style = if idx == playhead && self.state == UiState::Playing {
                Style::new().fg(theme::accent::SUCCESS).bold()
            } else {
                Style::new().fg(theme::accent::PRIMARY)
            };
            if selected {
                marker_style = marker_style.bg(theme::alpha::HIGHLIGHT);
            }
            let row_style = if selected {
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::HIGHLIGHT)
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            };

            let line = Line::from_spans([
                Span::styled(marker, marker_style),
                Span::raw(" "),
                Span::styled(
                    format!("{:03}", idx + 1),
                    if selected {
                        Style::new()
                            .fg(theme::fg::MUTED)
                            .bg(theme::alpha::HIGHLIGHT)
                    } else {
                        Style::new().fg(theme::fg::MUTED)
                    },
                ),
                Span::raw("  +"),
                Span::styled(
                    format!("{:>4}ms", timed.delay.as_millis()),
                    if selected {
                        Style::new()
                            .fg(theme::fg::SECONDARY)
                            .bg(theme::alpha::HIGHLIGHT)
                    } else {
                        Style::new().fg(theme::fg::SECONDARY)
                    },
                ),
                Span::raw("  @"),
                Span::styled(
                    format!("{:>5}ms", cumulative_ms),
                    if selected {
                        Style::new()
                            .fg(theme::fg::MUTED)
                            .bg(theme::alpha::HIGHLIGHT)
                    } else {
                        Style::new().fg(theme::fg::MUTED)
                    },
                ),
                Span::raw("  "),
                Span::styled(label, row_style),
            ]);
            lines.push(line);
        }

        Paragraph::new(Text::from_lines(lines))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
    }

    fn render_event_detail_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == FocusPanel::Timeline,
            theme::screen_accent::ADVANCED,
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Event Detail")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let Some(macro_data) = &self.macro_data else {
            Paragraph::new("Select a scenario or record a macro to see details.")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
            return;
        };

        let events = macro_data.events();
        if events.is_empty() {
            Paragraph::new("Macro is empty.")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
            return;
        }

        let idx = self.timeline_cursor.min(events.len().saturating_sub(1));
        let mut cumulative_ms: u64 = 0;
        for (i, timed) in events.iter().enumerate() {
            cumulative_ms += timed.delay.as_millis() as u64;
            if i == idx {
                break;
            }
        }
        let timed = &events[idx];

        let mut lines = vec![
            Line::from_spans([
                Span::styled("Selected: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("#{idx:03}"),
                    Style::new().fg(theme::fg::PRIMARY).bold(),
                ),
            ]),
            Line::from_spans([
                Span::styled("Delay: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("{:>4}ms", timed.delay.as_millis()),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
                Span::raw("   "),
                Span::styled("At: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("{:>5}ms", cumulative_ms),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
            ]),
            Line::new(),
            Line::from_spans([
                Span::styled("Event: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format_event(&timed.event),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
            ]),
        ];

        if let Event::Key(key) = &timed.event {
            lines.push(Line::from_spans([
                Span::styled("Kind: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("{:?}", key.kind),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
            ]));
            lines.push(Line::from_spans([
                Span::styled("Modifiers: ", Style::new().fg(theme::fg::SECONDARY)),
                Span::styled(
                    format!("{:?}", key.modifiers),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
            ]));
        }

        Paragraph::new(Text::from_lines(lines))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
    }

    fn render_scenarios_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == FocusPanel::Scenarios,
            theme::screen_accent::ADVANCED,
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Scenario Runner")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines = Vec::new();
        lines.push(Line::from_spans([Span::styled(
            "Preset scenarios (Enter to load)",
            Style::new().fg(theme::fg::SECONDARY),
        )]));
        lines.push(Line::new());

        for (idx, scenario) in SCENARIOS.iter().enumerate() {
            let selected = self.focus == FocusPanel::Scenarios && idx == self.scenario_index;
            let name_style = if selected {
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::HIGHLIGHT)
                    .bold()
            } else {
                Style::new().fg(theme::accent::PRIMARY)
            };
            let desc_style = if selected {
                Style::new()
                    .fg(theme::fg::SECONDARY)
                    .bg(theme::alpha::HIGHLIGHT)
            } else {
                Style::new().fg(theme::fg::MUTED)
            };

            lines.push(Line::from_spans([Span::styled(scenario.name, name_style)]));
            lines.push(Line::from_spans([Span::styled(
                format!("  {}", scenario.description),
                desc_style,
            )]));
            lines.push(Line::new());
        }

        Paragraph::new(Text::from_lines(lines))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
    }

    fn playback_progress(&self) -> f64 {
        let Some(macro_data) = &self.macro_data else {
            return 0.0;
        };
        let total = macro_data.total_duration().as_millis() as f64;
        if total <= 0.0 {
            return if macro_data.is_empty() { 0.0 } else { 1.0 };
        }
        let elapsed = self
            .playback
            .as_ref()
            .map(|p| p.elapsed().as_millis() as f64)
            .unwrap_or(0.0);
        (elapsed / total).clamp(0.0, 1.0)
    }
}

impl Screen for MacroRecorderScreen {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Resize { width, height } = event {
            self.terminal_size = (*width, *height);
        }
        if let Event::Mouse(mouse) = event
            && matches!(
                mouse.kind,
                ftui_core::event::MouseEventKind::Down(ftui_core::event::MouseButton::Left)
            )
        {
            let controls = self.layout_controls.get();
            let timeline = self.layout_timeline.get();
            let scenarios = self.layout_scenarios.get();
            if controls.contains(mouse.x, mouse.y) {
                self.set_focus(FocusPanel::Controls);
            } else if timeline.contains(mouse.x, mouse.y) {
                self.set_focus(FocusPanel::Timeline);
            } else if scenarios.contains(mouse.x, mouse.y) {
                self.set_focus(FocusPanel::Scenarios);
            }
        }
        self.handle_controls(event);
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        let mut controls_height: u16 = 6;
        if self.status_note.is_some() {
            controls_height += 1;
        }
        if matches!(&self.state, UiState::Error(_)) {
            controls_height += 1;
        }
        let sections = Flex::vertical()
            .constraints([Constraint::Fixed(controls_height), Constraint::Min(1)])
            .split(area);

        self.layout_controls.set(sections[0]);
        self.render_controls_panel(frame, sections[0]);

        let bottom = Flex::horizontal()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(sections[1]);

        let right = Flex::vertical()
            .constraints([Constraint::Percentage(65.0), Constraint::Percentage(35.0)])
            .split(bottom[1]);

        self.layout_timeline.set(bottom[0]);
        self.layout_scenarios.set(right[1]);
        self.render_timeline_panel(frame, bottom[0]);
        self.render_event_detail_panel(frame, right[0]);
        self.render_scenarios_panel(frame, right[1]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Space/r",
                action: "Record / Stop",
            },
            HelpEntry {
                key: "Enter/p",
                action: "Play / Pause",
            },
            HelpEntry {
                key: "Alt+\u{2191}/\u{2190}/\u{2192}",
                action: "Jump focus (Controls/Timeline/Scenarios)",
            },
            HelpEntry {
                key: "Ctrl+\u{2191}/\u{2193}",
                action: "Timeline page scroll",
            },
            HelpEntry {
                key: "Ctrl+\u{2190}/\u{2192}",
                action: "Timeline jump start/end",
            },
            HelpEntry {
                key: "Tab/Shift+Tab",
                action: "Cycle focus",
            },
            HelpEntry {
                key: "\u{2191}/\u{2193}",
                action: "Select event/scenario",
            },
            HelpEntry {
                key: "l",
                action: "Toggle loop",
            },
            HelpEntry {
                key: "+/-",
                action: "Adjust speed",
            },
            HelpEntry {
                key: "Esc",
                action: "Stop playback",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click panel to focus",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        self.last_tick_count = tick_count;
        self.playback_tick(tick_count);
    }

    fn title(&self) -> &'static str {
        "Macro Recorder"
    }

    fn tab_label(&self) -> &'static str {
        "Macro"
    }
}

fn is_control_key(event: &Event) -> bool {
    let Event::Key(KeyEvent {
        code,
        modifiers,
        kind,
        ..
    }) = event
    else {
        return false;
    };
    if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return false;
    }

    let (code, modifiers) = (*code, *modifiers);

    matches!(
        (code, modifiers),
        (KeyCode::Char('r'), Modifiers::NONE)
            | (KeyCode::Char('p'), Modifiers::NONE)
            | (KeyCode::Char('l'), Modifiers::NONE)
            | (KeyCode::Char('+'), Modifiers::NONE)
            | (KeyCode::Char('='), Modifiers::NONE)
            | (KeyCode::Char('-'), Modifiers::NONE)
            | (KeyCode::Char(' '), Modifiers::NONE)
            | (KeyCode::Enter, Modifiers::NONE)
            | (KeyCode::Left, Modifiers::CTRL)
            | (KeyCode::Right, Modifiers::CTRL)
            | (KeyCode::Up, Modifiers::CTRL)
            | (KeyCode::Down, Modifiers::CTRL)
            | (KeyCode::Escape, _)
    )
}

fn key_event(code: KeyCode, modifiers: Modifiers) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
    })
}

fn format_event(event: &Event) -> String {
    match event {
        Event::Key(key) => format_key_event(key),
        Event::Mouse(mouse) => format_mouse_event(mouse),
        Event::Paste(text) => format!("Paste({} chars)", text.text.len()),
        Event::Resize { width, height } => format!("Resize {}x{}", width, height),
        Event::Focus(focus) => format!("Focus({:?})", focus),
        Event::Clipboard(_) => "Clipboard".to_string(),
        Event::Tick => "Tick".to_string(),
    }
}

fn format_key_event(key: &KeyEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    if key.modifiers.contains(Modifiers::CTRL) {
        parts.push("Ctrl".to_string());
    }
    if key.modifiers.contains(Modifiers::ALT) {
        parts.push("Alt".to_string());
    }
    if key.modifiers.contains(Modifiers::SHIFT) {
        parts.push("Shift".to_string());
    }
    if key.modifiers.contains(Modifiers::SUPER) {
        parts.push("Super".to_string());
    }

    let code = match key.code {
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BackTab => "BackTab".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::Escape => "Esc".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("F{}", n),
        other => format!("{:?}", other),
    };

    parts.push(code);

    if key.kind != KeyEventKind::Press {
        parts.push(format!("{:?}", key.kind));
    }

    parts.join("+")
}

fn format_mouse_event(mouse: &MouseEvent) -> String {
    format!("Mouse({:?} @{}, {})", mouse.kind, mouse.x, mouse.y)
}

fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    if max_chars < 2 || s.len() <= max_chars {
        return s.to_string();
    }
    let mut result: String = s.chars().take(max_chars - 1).collect();
    result.push('…');
    result
}

fn format_duration(duration: std::time::Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1000 {
        return format!("{}ms", ms);
    }
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    format!("{}.{:03}s", secs, millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_runtime::input_macro::{MacroMetadata, TimedEvent};

    fn key_event(c: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    }

    fn ctrl_key(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        })
    }

    fn macro_with_delays(name: &str, items: &[(char, u64)]) -> InputMacro {
        let mut events = Vec::with_capacity(items.len());
        let mut total = std::time::Duration::ZERO;
        for (ch, delay_ms) in items {
            let delay = std::time::Duration::from_millis(*delay_ms);
            total += delay;
            events.push(TimedEvent::new(key_event(*ch), delay));
        }
        InputMacro::new(
            events,
            MacroMetadata {
                name: name.to_string(),
                terminal_size: (80, 24),
                total_duration: total,
            },
        )
    }

    #[test]
    fn playback_drains_events_in_order_for_zero_delay_macro() {
        let mut screen = MacroRecorderScreen::new();
        screen.macro_data = Some(InputMacro::from_events(
            "zero",
            vec![key_event('a'), key_event('b'), key_event('c')],
        ));

        screen.start_playback(0);
        screen.tick(0);

        let events = screen.drain_playback_events();
        assert_eq!(events, vec![key_event('a'), key_event('b'), key_event('c')]);
        assert_eq!(screen.state, UiState::Stopped);
        assert!(screen.playback.is_none());
    }

    #[test]
    fn playback_speed_affects_due_time_for_delayed_events() {
        // Two events: immediate 'a', then 'b' due at +1000ms.
        let mut screen = MacroRecorderScreen::new();
        screen.speed = 2.0;
        screen.macro_data = Some(macro_with_delays("delayed", &[('a', 0), ('b', 1000)]));

        screen.start_playback(0);

        // Tick 0 advances by at least one tick (100ms) scaled by speed (2x => 200ms),
        // so only the first event should be emitted.
        screen.tick(0);
        assert_eq!(screen.drain_playback_events(), vec![key_event('a')]);

        // Next event is due at 1000ms, which at 2x speed arrives on tick 4:
        // ticks 0..=4 => 5 steps * 100ms * 2 = 1000ms
        for t in 1..4 {
            screen.tick(t);
            assert!(screen.drain_playback_events().is_empty());
        }

        screen.tick(4);
        assert_eq!(screen.drain_playback_events(), vec![key_event('b')]);
        assert_eq!(screen.state, UiState::Stopped);
    }

    #[test]
    fn control_keys_can_be_filtered_from_recording() {
        let mut screen = MacroRecorderScreen::new();
        screen.start_recording();

        screen.record_event(&key_event('a'), true);
        screen.record_event(&key_event('p'), true); // control key -> ignored
        screen.record_event(&key_event('l'), true); // control key -> ignored

        screen.stop_recording();

        let recorded = screen
            .macro_data
            .as_ref()
            .expect("macro_data should be present after stop_recording")
            .bare_events();
        assert_eq!(recorded, vec![key_event('a')]);
    }

    #[test]
    fn ctrl_arrows_jump_timeline_when_focused() {
        let mut screen = MacroRecorderScreen::new();
        screen.macro_data = Some(InputMacro::from_events(
            "jump",
            vec![key_event('a'), key_event('b'), key_event('c')],
        ));
        screen.focus = FocusPanel::Timeline;
        screen.timeline_cursor = 1;

        screen.update(&ctrl_key(KeyCode::Left));
        assert_eq!(screen.timeline_cursor, 0);

        screen.update(&ctrl_key(KeyCode::Right));
        assert_eq!(screen.timeline_cursor, 2);
    }

    // ====================================================================
    // Accessibility tests (bd-2lus.9)
    // ====================================================================

    #[test]
    fn state_labels_contain_unicode_symbol_prefix() {
        // Each state label must contain a non-ASCII symbol so states are
        // distinguishable without colour.
        let mut screen = MacroRecorderScreen::new();
        let area = Rect::from_size(80, 24);
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        // Idle
        screen.view(&mut frame, area);
        let buf_str = frame_text(&frame);
        assert!(buf_str.contains('○'), "Idle state should contain ○ symbol");

        // Recording
        screen.start_recording();
        frame.buffer.clear();
        screen.view(&mut frame, area);
        let buf_str = frame_text(&frame);
        assert!(
            buf_str.contains('●'),
            "Recording state should contain ● symbol"
        );
        screen.stop_recording();

        // Playing (need a macro first)
        screen.macro_data = Some(InputMacro::from_events("test", vec![key_event('x')]));
        screen.start_playback(0);
        frame.buffer.clear();
        screen.view(&mut frame, area);
        let buf_str = frame_text(&frame);
        assert!(
            buf_str.contains('▶'),
            "Playing state should contain ▶ symbol"
        );
    }

    #[test]
    fn error_messages_include_recovery_hint() {
        let mut screen = MacroRecorderScreen::new();
        // Trigger "No macro recorded" error
        screen.start_playback(0);
        assert!(
            matches!(&screen.state, UiState::Error(_)),
            "Expected Error state, got {:?}",
            screen.state
        );
        let UiState::Error(msg) = &screen.state else {
            return;
        };
        assert!(
            msg.contains("press 'r'"),
            "Error should include recovery action, got: {msg}"
        );
    }

    #[test]
    fn error_state_uses_distinct_style_from_recording() {
        // Error should use WARNING color + underline while Recording uses
        // ERROR color + italic, ensuring they are visually distinct.
        let screen_rec = {
            let mut s = MacroRecorderScreen::new();
            s.state = UiState::Recording;
            s
        };
        let screen_err = {
            let mut s = MacroRecorderScreen::new();
            s.state = UiState::Error("test".into());
            s
        };

        let area = Rect::from_size(80, 24);
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame_rec = Frame::new(80, 24, &mut pool);
        screen_rec.view(&mut frame_rec, area);

        let mut pool2 = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame_err = Frame::new(80, 24, &mut pool2);
        screen_err.view(&mut frame_err, area);

        // The "State:" label cell (column after "State: ") should differ
        // between recording and error frames.
        let rec_text = frame_text(&frame_rec);
        let err_text = frame_text(&frame_err);
        assert!(rec_text.contains("● Recording"), "Recording label mismatch");
        assert!(
            err_text.contains("Error"),
            "Error label mismatch: {err_text}"
        );
    }

    #[test]
    fn timeline_playhead_uses_unicode_markers() {
        let mut screen = MacroRecorderScreen::new();
        screen.macro_data = Some(macro_with_delays(
            "markers",
            &[('a', 0), ('b', 500), ('c', 500)],
        ));
        screen.start_playback(0);
        // Don't tick, so playhead is at position 0.
        let area = Rect::from_size(80, 24);
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        screen.view(&mut frame, area);
        let buf_str = frame_text(&frame);

        // The playhead marker ▶ should appear and future markers ○ should too.
        assert!(buf_str.contains('▶'), "Playhead should show ▶");
        assert!(buf_str.contains('○'), "Future events should show ○");
    }

    #[test]
    fn truncate_with_ellipsis_works() {
        assert_eq!(super::truncate_with_ellipsis("hello", 10), "hello");
        assert_eq!(super::truncate_with_ellipsis("hello world", 6), "hello…");
        assert_eq!(super::truncate_with_ellipsis("ab", 2), "ab");
        assert_eq!(super::truncate_with_ellipsis("abc", 2), "a…");
        // max_chars < 2 returns original
        assert_eq!(super::truncate_with_ellipsis("abc", 1), "abc");
    }

    /// Extract all text content from a frame buffer as a single string.
    fn frame_text(frame: &Frame) -> String {
        let mut out = String::new();
        for y in 0..frame.buffer.height() {
            for x in 0..frame.buffer.width() {
                if let Some(cell) = frame.buffer.get(x, y) {
                    if let Some(id) = cell.content.grapheme_id() {
                        if let Some(grapheme) = frame.pool.get(id) {
                            out.push_str(grapheme);
                        } else {
                            out.push(' ');
                        }
                    } else if let Some(ch) = cell.content.as_char() {
                        out.push(ch);
                    } else {
                        out.push(' ');
                    }
                }
            }
        }
        out
    }
}
