#![forbid(unsafe_code)]

//! Advanced Features screen — diagnostics, timers, and error display.
//!
//! Demonstrates:
//! - `Traceback` with styled exception frames
//! - `Timer` with countdown display
//! - `Spinner` with multiple frame sets
//! - System information panel

use std::cell::Cell;
use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::timer::{DisplayFormat, Timer};
use ftui_extras::traceback::{Traceback, TracebackFrame};
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::{Cmd, FilteredEventRecorder, InputMacro, RecordingFilter};
use ftui_style::Style;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::spinner::{self, Spinner, SpinnerState};
use ftui_widgets::{StatefulWidget, Widget};

use super::{HelpEntry, Screen};
use crate::theme;

/// Which panel has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Traceback,
    Timers,
    Macro,
    Info,
}

impl Panel {
    fn next(self) -> Self {
        match self {
            Self::Traceback => Self::Timers,
            Self::Timers => Self::Macro,
            Self::Macro => Self::Info,
            Self::Info => Self::Traceback,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Traceback => Self::Info,
            Self::Timers => Self::Traceback,
            Self::Macro => Self::Timers,
            Self::Info => Self::Macro,
        }
    }
}

const MACRO_SPEED_MIN: f64 = 0.25;
const MACRO_SPEED_MAX: f64 = 4.0;
const MACRO_SPEED_STEP: f64 = 0.25;
const MACRO_TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
struct MacroPlayback {
    playing: bool,
    loop_enabled: bool,
    speed: f64,
    index: usize,
    elapsed: Duration,
    next_at: Duration,
    error: Option<String>,
}

impl MacroPlayback {
    fn new() -> Self {
        Self {
            playing: false,
            loop_enabled: false,
            speed: 1.0,
            index: 0,
            elapsed: Duration::ZERO,
            next_at: Duration::ZERO,
            error: None,
        }
    }
}

pub struct AdvancedFeatures {
    focus: Panel,
    timer_compact: Timer,
    timer_clock: Timer,
    spinner_tick: usize,
    tick_count: u64,
    macro_recorder: Option<FilteredEventRecorder>,
    macro_recording: Option<InputMacro>,
    macro_playback: MacroPlayback,
    layout_traceback: Cell<Rect>,
    layout_timers: Cell<Rect>,
    layout_macro: Cell<Rect>,
    layout_info: Cell<Rect>,
}

impl Default for AdvancedFeatures {
    fn default() -> Self {
        Self::new()
    }
}

impl AdvancedFeatures {
    pub fn new() -> Self {
        let mut timer_compact = Timer::new(Duration::from_secs(300)).format(DisplayFormat::Compact);
        timer_compact.start();

        let mut timer_clock =
            Timer::with_interval(Duration::from_secs(120), Duration::from_millis(100))
                .format(DisplayFormat::Clock);
        timer_clock.start();

        Self {
            focus: Panel::Traceback,
            timer_compact,
            timer_clock,
            spinner_tick: 0,
            tick_count: 0,
            macro_recorder: None,
            macro_recording: None,
            macro_playback: MacroPlayback::new(),
            layout_traceback: Cell::new(Rect::default()),
            layout_timers: Cell::new(Rect::default()),
            layout_macro: Cell::new(Rect::default()),
            layout_info: Cell::new(Rect::default()),
        }
    }

    fn is_macro_control(event: &Event) -> bool {
        matches!(
            event,
            Event::Key(KeyEvent {
                code: KeyCode::Char('r')
                    | KeyCode::Char('p')
                    | KeyCode::Char('l')
                    | KeyCode::Char('+')
                    | KeyCode::Char('-')
                    | KeyCode::Escape,
                kind: KeyEventKind::Press,
                ..
            })
        )
    }

    fn is_recording(&self) -> bool {
        self.macro_recorder
            .as_ref()
            .is_some_and(FilteredEventRecorder::is_recording)
    }

    fn should_record_event(&self, event: &Event) -> bool {
        if self.macro_playback.playing {
            return false;
        }
        if self.focus == Panel::Macro && Self::is_macro_control(event) {
            return false;
        }
        true
    }

    fn start_recording(&mut self) {
        let name = format!("demo-macro-{}", self.tick_count);
        let filter = RecordingFilter {
            keys: true,
            mouse: true,
            resize: false,
            paste: true,
            focus: false,
        };
        let mut recorder = FilteredEventRecorder::new(name, filter);
        recorder.start();
        self.macro_recorder = Some(recorder);
        self.macro_recording = None;
        self.macro_playback.error = None;
        self.macro_playback.playing = false;
    }

    fn stop_recording(&mut self) {
        if let Some(recorder) = self.macro_recorder.take() {
            self.macro_recording = Some(recorder.finish());
        }
    }

    fn reset_playback(&mut self) {
        self.macro_playback.index = 0;
        self.macro_playback.elapsed = Duration::ZERO;
        self.macro_playback.next_at = self
            .macro_recording
            .as_ref()
            .and_then(|m| m.events().first())
            .map(|e| e.delay)
            .unwrap_or(Duration::ZERO);
    }

    fn stop_playback(&mut self) {
        self.macro_playback.playing = false;
        self.reset_playback();
    }

    fn toggle_playback(&mut self) {
        if self.is_recording() {
            self.macro_playback.error = Some("Stop recording before playback".to_string());
            return;
        }
        let Some(macro_data) = self.macro_recording.as_ref() else {
            self.macro_playback.error = Some("No macro recorded".to_string());
            return;
        };
        if macro_data.is_empty() {
            self.macro_playback.error = Some("Macro has no events".to_string());
            return;
        }
        self.macro_playback.error = None;
        self.macro_playback.playing = !self.macro_playback.playing;
        if self.macro_playback.playing {
            self.reset_playback();
        }
    }

    fn adjust_speed(&mut self, delta: f64) {
        self.macro_playback.speed =
            (self.macro_playback.speed + delta).clamp(MACRO_SPEED_MIN, MACRO_SPEED_MAX);
    }

    fn handle_macro_controls(&mut self, event: &Event) -> bool {
        if self.focus != Panel::Macro {
            return false;
        }
        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                KeyCode::Char('r') => {
                    if self.is_recording() {
                        self.stop_recording();
                    } else {
                        self.start_recording();
                    }
                    return true;
                }
                KeyCode::Char('p') => {
                    self.toggle_playback();
                    return true;
                }
                KeyCode::Char('l') => {
                    self.macro_playback.loop_enabled = !self.macro_playback.loop_enabled;
                    return true;
                }
                KeyCode::Char('+') => {
                    self.adjust_speed(MACRO_SPEED_STEP);
                    return true;
                }
                KeyCode::Char('-') => {
                    self.adjust_speed(-MACRO_SPEED_STEP);
                    return true;
                }
                KeyCode::Escape => {
                    if self.is_recording() {
                        self.stop_recording();
                        return true;
                    }
                    if self.macro_playback.playing {
                        self.stop_playback();
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn apply_event(&mut self, event: &Event) {
        // Reset timers
        if let Event::Key(KeyEvent {
            code: KeyCode::Char('r'),
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            self.timer_compact.reset();
            self.timer_clock.reset();
        }

        // Pause/resume
        if let Event::Key(KeyEvent {
            code: KeyCode::Char(' '),
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            self.timer_compact.toggle();
            self.timer_clock.toggle();
        }
    }

    fn tick_macro(&mut self, tick_delta: Duration) {
        if !self.macro_playback.playing {
            return;
        }
        let events_len = match self.macro_recording.as_ref() {
            Some(m) => m.events().len(),
            None => {
                self.macro_playback.playing = false;
                self.macro_playback.error = Some("No macro recorded".to_string());
                return;
            }
        };
        if events_len == 0 {
            self.macro_playback.playing = false;
            self.macro_playback.error = Some("Macro has no events".to_string());
            return;
        }

        let scaled = Duration::from_secs_f64(tick_delta.as_secs_f64() * self.macro_playback.speed);
        self.macro_playback.elapsed += scaled;

        while self.macro_playback.index < events_len {
            let next_at = self.macro_playback.next_at;
            if self.macro_playback.elapsed < next_at {
                break;
            }
            let (event, next_delay) = {
                let macro_data = self
                    .macro_recording
                    .as_ref()
                    .expect("macro_recording missing");
                let events = macro_data.events();
                let event = events[self.macro_playback.index].event.clone();
                let next_delay = if self.macro_playback.index + 1 < events_len {
                    events[self.macro_playback.index + 1].delay
                } else {
                    Duration::ZERO
                };
                (event, next_delay)
            };
            self.apply_event(&event);
            self.macro_playback.index += 1;
            if self.macro_playback.index < events_len {
                self.macro_playback.next_at += next_delay;
            }
        }

        if self.macro_playback.index >= events_len {
            if self.macro_playback.loop_enabled {
                self.reset_playback();
            } else {
                self.macro_playback.playing = false;
            }
        }
    }

    fn render_traceback_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = theme::panel_border_style(
            self.focus == Panel::Traceback,
            theme::screen_accent::ADVANCED,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Error Traceback")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let traceback = build_sample_traceback();
        traceback.render(inner, frame);
    }

    fn render_timers_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style =
            theme::panel_border_style(self.focus == Panel::Timers, theme::screen_accent::ADVANCED);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Timers & Spinners")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Min(1),
            ])
            .split(inner);

        // Timer compact
        let compact_text = format!(
            "Compact timer: {} ({}%)",
            self.timer_compact.view(),
            (self.timer_compact.progress() * 100.0) as u32
        );
        Paragraph::new(&*compact_text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(rows[0], frame);

        // Timer clock
        let clock_text = format!(
            "Clock timer:   {} ({}%)",
            self.timer_clock.view(),
            (self.timer_clock.progress() * 100.0) as u32
        );
        Paragraph::new(&*clock_text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(rows[1], frame);

        // Blank separator
        if !rows[3].is_empty() {
            Paragraph::new("Spinners:")
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(rows[3], frame);
        }

        // Spinner demos
        let spinner_sets = [
            ("dots", Spinner::new()),
            ("line", Spinner::new().frames(spinner::LINE)),
            ("braille", Spinner::new().frames(spinner::DOTS)),
        ];

        for (i, (name, spinner)) in spinner_sets.iter().enumerate() {
            let row_idx = 4 + i;
            if row_idx < rows.len() && !rows[row_idx].is_empty() {
                let cols = Flex::horizontal()
                    .constraints([
                        Constraint::Fixed(12),
                        Constraint::Fixed(3),
                        Constraint::Min(1),
                    ])
                    .split(rows[row_idx]);

                Paragraph::new(*name)
                    .style(Style::new().fg(theme::fg::MUTED))
                    .render(cols[0], frame);

                let styled_spinner = spinner
                    .clone()
                    .style(Style::new().fg(theme::screen_accent::ADVANCED));
                let mut state = SpinnerState {
                    current_frame: self.spinner_tick,
                };
                StatefulWidget::render(&styled_spinner, cols[1], frame, &mut state);
            }
        }

        // Progress indicator
        if !rows[7].is_empty() {
            let progress = self.timer_compact.progress();
            let bar_width = rows[7].width.saturating_sub(12) as usize;
            let filled = (progress * bar_width as f64) as usize;
            let bar: String = "\u{2588}"
                .repeat(filled)
                .chars()
                .chain("\u{2591}".repeat(bar_width.saturating_sub(filled)).chars())
                .collect();
            let label = format!("Progress: {bar}");
            Paragraph::new(&*label)
                .style(Style::new().fg(theme::accent::SUCCESS))
                .render(rows[7], frame);
        }
    }

    fn render_macro_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style =
            theme::panel_border_style(self.focus == Panel::Macro, theme::screen_accent::ADVANCED);

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

        let is_recording = self.is_recording();
        let state = if is_recording {
            "Recording"
        } else if self.macro_playback.playing {
            "Playing"
        } else {
            "Idle"
        };

        let (event_count, filtered) = if let Some(rec) = &self.macro_recorder {
            (rec.event_count(), rec.filtered_count())
        } else if let Some(m) = &self.macro_recording {
            (m.len(), 0)
        } else {
            (0, 0)
        };

        let duration = self
            .macro_recording
            .as_ref()
            .map(|m| format!("{:.2}s", m.total_duration().as_secs_f64()))
            .unwrap_or_else(|| "-".to_string());

        let name = self
            .macro_recording
            .as_ref()
            .map(|m| m.metadata().name.as_str())
            .unwrap_or("none");

        let mut lines = vec![
            format!("State: {state}"),
            format!("Macro: {name}"),
            format!("Events: {event_count}"),
            format!("Duration: {duration}"),
            format!("Speed: {:.2}x", self.macro_playback.speed),
            format!(
                "Loop: {}",
                if self.macro_playback.loop_enabled {
                    "On"
                } else {
                    "Off"
                }
            ),
        ];

        if is_recording && filtered > 0 {
            lines.push(format!("Filtered: {filtered}"));
        }

        if let Some(err) = &self.macro_playback.error {
            lines.push(format!("Error: {err}"));
        }

        lines.push("Controls: r/p/l +/- Esc".to_string());

        for (i, line) in lines.iter().enumerate() {
            if i >= inner.height as usize {
                break;
            }
            let style = if line.starts_with("State:") {
                Style::new().fg(theme::screen_accent::ADVANCED)
            } else if line.starts_with("Error:") {
                Style::new().fg(theme::accent::ERROR)
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            };
            let row_area = Rect::new(inner.x, inner.y.saturating_add(i as u16), inner.width, 1);
            Paragraph::new(line.clone())
                .style(style)
                .render(row_area, frame);
        }
    }

    fn render_info_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style =
            theme::panel_border_style(self.focus == Panel::Info, theme::screen_accent::ADVANCED);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("System Info")
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let info_lines = [
            "FrankenTUI Demo Showcase",
            "",
            "Framework: ftui (Rust)",
            "Rendering: 16-byte Cell model",
            "Color: 24-bit true color (RGB)",
            "Input: Kitty keyboard protocol",
            "",
            "Features demonstrated:",
            "  - Traceback error display",
            "  - Countdown timers (compact/clock)",
            "  - Terminal spinners",
            "  - Progress bars",
            "  - Macro recorder + playback",
            "",
            "Controls:",
            "  r - Reset timers",
            "  Space - Pause/resume timers",
        ];

        for (i, &line) in info_lines.iter().enumerate() {
            if i >= inner.height as usize {
                break;
            }
            let style = if i == 0 {
                Style::new().fg(theme::screen_accent::ADVANCED)
            } else if line.starts_with("  -") || line.starts_with("  r") || line.starts_with("  S")
            {
                Style::new().fg(theme::fg::SECONDARY)
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            };
            let row_area = Rect::new(inner.x, inner.y.saturating_add(i as u16), inner.width, 1);
            Paragraph::new(line).style(style).render(row_area, frame);
        }
    }
}

fn build_sample_traceback() -> Traceback {
    Traceback::new(
        vec![
            TracebackFrame::new("main", 42)
                .filename("src/main.rs")
                .source_context(
                    "fn main() -> Result<()> {\n    let config = Config::load()?;\n    app::run(config)?;\n    Ok(())\n}",
                    40,
                ),
            TracebackFrame::new("run", 128)
                .filename("src/app.rs")
                .source_context(
                    "pub fn run(config: Config) -> Result<()> {\n    let db = Database::connect(&config.db_url)?;\n    let server = Server::new(db);\n    server.listen(config.port)?;\n    Ok(())\n}",
                    125,
                ),
            TracebackFrame::new("connect", 56)
                .filename("src/database.rs")
                .source_context(
                    "pub fn connect(url: &str) -> Result<Self> {\n    let pool = Pool::builder()\n        .max_size(10)\n        .build(url)?;\n    Ok(Self { pool })\n}",
                    53,
                ),
        ],
        "ConnectionError",
        "failed to connect to database: connection refused (localhost:5432)",
    )
    .title("Application Error")
}

impl Screen for AdvancedFeatures {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
        {
            self.focus = self.focus.next();
            return Cmd::None;
        }
        if let Event::Key(KeyEvent {
            code: KeyCode::Left,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
            && modifiers.contains(Modifiers::CTRL)
        {
            self.focus = self.focus.prev();
            return Cmd::None;
        }

        if self.is_recording()
            && self.should_record_event(event)
            && let Some(recorder) = &mut self.macro_recorder
        {
            recorder.record(event);
        }

        if self.handle_macro_controls(event) {
            return Cmd::None;
        }

        // Number keys for direct panel focus
        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers: Modifiers::NONE,
            ..
        }) = event
        {
            match code {
                KeyCode::Char('1') => self.focus = Panel::Traceback,
                KeyCode::Char('2') => self.focus = Panel::Timers,
                KeyCode::Char('3') => self.focus = Panel::Macro,
                KeyCode::Char('4') => self.focus = Panel::Info,
                _ => {}
            }
        }

        self.apply_event(event);

        // Mouse handling
        if let Event::Mouse(mouse) = event {
            let (x, y) = (mouse.x, mouse.y);
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if self.layout_traceback.get().contains(x, y) {
                        self.focus = Panel::Traceback;
                    } else if self.layout_timers.get().contains(x, y) {
                        self.focus = Panel::Timers;
                    } else if self.layout_macro.get().contains(x, y) {
                        self.focus = Panel::Macro;
                    } else if self.layout_info.get().contains(x, y) {
                        self.focus = Panel::Info;
                    }
                }
                MouseEventKind::ScrollDown => {
                    self.focus = self.focus.next();
                }
                MouseEventKind::ScrollUp => {
                    self.focus = self.focus.prev();
                }
                _ => {}
            }
        }

        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.spinner_tick = tick_count as usize;
        self.timer_compact.tick(MACRO_TICK);
        self.timer_clock.tick(MACRO_TICK);
        self.tick_macro(MACRO_TICK);
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let main = Flex::vertical()
            .constraints([Constraint::Min(1), Constraint::Fixed(1)])
            .split(area);

        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(main[0]);

        // Left: traceback
        self.layout_traceback.set(cols[0]);
        self.render_traceback_panel(frame, cols[0]);

        // Right: split into timers, macro, and info
        let right_rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(40.0),
                Constraint::Percentage(30.0),
                Constraint::Percentage(30.0),
            ])
            .split(cols[1]);

        self.layout_timers.set(right_rows[0]);
        self.layout_macro.set(right_rows[1]);
        self.layout_info.set(right_rows[2]);
        self.render_timers_panel(frame, right_rows[0]);
        self.render_macro_panel(frame, right_rows[1]);
        self.render_info_panel(frame, right_rows[2]);

        // Status bar
        let status = format!(
            "Tick: {} | Ctrl+\u{2190}/\u{2192}: panels | r: reset | Space: pause | Macro: r/p/l +/-",
            self.tick_count
        );
        Paragraph::new(&*status)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(main[1], frame);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Ctrl+\u{2190}/\u{2192}",
                action: "Switch panel",
            },
            HelpEntry {
                key: "1/2/3/4",
                action: "Focus panel directly",
            },
            HelpEntry {
                key: "r",
                action: "Reset timers",
            },
            HelpEntry {
                key: "Space",
                action: "Pause/resume",
            },
            HelpEntry {
                key: "Macro: r/p/l +/-",
                action: "Record/play/loop/speed",
            },
            HelpEntry {
                key: "Click",
                action: "Focus panel",
            },
            HelpEntry {
                key: "Scroll",
                action: "Cycle panels",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Advanced Features"
    }

    fn tab_label(&self) -> &'static str {
        "Advanced"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        })
    }

    fn ctrl_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::CTRL,
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn initial_state() {
        let screen = AdvancedFeatures::new();
        assert_eq!(screen.focus, Panel::Traceback);
        assert_eq!(screen.title(), "Advanced Features");
        assert!(screen.timer_compact.running());
    }

    #[test]
    fn panel_navigation() {
        let mut screen = AdvancedFeatures::new();
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, Panel::Timers);
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, Panel::Macro);
        screen.update(&ctrl_press(KeyCode::Right));
        assert_eq!(screen.focus, Panel::Info);
        screen.update(&ctrl_press(KeyCode::Left));
        assert_eq!(screen.focus, Panel::Macro);
    }

    #[test]
    fn timer_toggle() {
        let mut screen = AdvancedFeatures::new();
        assert!(screen.timer_compact.running());
        screen.update(&press(KeyCode::Char(' ')));
        assert!(!screen.timer_compact.running());
        screen.update(&press(KeyCode::Char(' ')));
        assert!(screen.timer_compact.running());
    }

    #[test]
    fn timer_reset() {
        let mut screen = AdvancedFeatures::new();
        screen.tick(10);
        let initial = screen.timer_compact.initial();
        screen.update(&press(KeyCode::Char('r')));
        assert_eq!(screen.timer_compact.remaining(), initial);
    }

    #[test]
    fn traceback_has_frames() {
        let tb = build_sample_traceback();
        assert_eq!(tb.frames().len(), 3);
        assert_eq!(tb.exception_type(), "ConnectionError");
    }

    fn render_screen(screen: &AdvancedFeatures) {
        use ftui_render::grapheme_pool::GraphemePool;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 80, 24));
    }

    fn mouse_click(x: u16, y: u16) -> Event {
        use ftui_core::event::MouseEvent;
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    fn mouse_scroll_down(x: u16, y: u16) -> Event {
        use ftui_core::event::MouseEvent;
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    fn mouse_scroll_up(x: u16, y: u16) -> Event {
        use ftui_core::event::MouseEvent;
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    #[test]
    fn number_keys_focus_panels() {
        let mut screen = AdvancedFeatures::new();
        assert_eq!(screen.focus, Panel::Traceback);
        screen.update(&press(KeyCode::Char('2')));
        assert_eq!(screen.focus, Panel::Timers);
        screen.update(&press(KeyCode::Char('3')));
        assert_eq!(screen.focus, Panel::Macro);
        screen.update(&press(KeyCode::Char('4')));
        assert_eq!(screen.focus, Panel::Info);
        screen.update(&press(KeyCode::Char('1')));
        assert_eq!(screen.focus, Panel::Traceback);
    }

    #[test]
    fn click_traceback_focuses() {
        let mut screen = AdvancedFeatures::new();
        screen.focus = Panel::Info;
        render_screen(&screen);
        let tb = screen.layout_traceback.get();
        assert!(!tb.is_empty());
        let cx = tb.x + tb.width / 2;
        let cy = tb.y + tb.height / 2;
        screen.update(&mouse_click(cx, cy));
        assert_eq!(screen.focus, Panel::Traceback);
    }

    #[test]
    fn click_timers_focuses() {
        let mut screen = AdvancedFeatures::new();
        render_screen(&screen);
        let t = screen.layout_timers.get();
        assert!(!t.is_empty());
        let cx = t.x + t.width / 2;
        let cy = t.y + t.height / 2;
        screen.update(&mouse_click(cx, cy));
        assert_eq!(screen.focus, Panel::Timers);
    }

    #[test]
    fn click_macro_focuses() {
        let mut screen = AdvancedFeatures::new();
        render_screen(&screen);
        let m = screen.layout_macro.get();
        assert!(!m.is_empty());
        let cx = m.x + m.width / 2;
        let cy = m.y + m.height / 2;
        screen.update(&mouse_click(cx, cy));
        assert_eq!(screen.focus, Panel::Macro);
    }

    #[test]
    fn click_info_focuses() {
        let mut screen = AdvancedFeatures::new();
        render_screen(&screen);
        let info = screen.layout_info.get();
        assert!(!info.is_empty());
        let cx = info.x + info.width / 2;
        let cy = info.y + info.height / 2;
        screen.update(&mouse_click(cx, cy));
        assert_eq!(screen.focus, Panel::Info);
    }

    #[test]
    fn scroll_cycles_panels() {
        let mut screen = AdvancedFeatures::new();
        assert_eq!(screen.focus, Panel::Traceback);
        screen.update(&mouse_scroll_down(40, 12));
        assert_eq!(screen.focus, Panel::Timers);
        screen.update(&mouse_scroll_down(40, 12));
        assert_eq!(screen.focus, Panel::Macro);
        screen.update(&mouse_scroll_up(40, 12));
        assert_eq!(screen.focus, Panel::Timers);
    }

    #[test]
    fn keybindings_include_mouse_hints() {
        let screen = AdvancedFeatures::new();
        let bindings = screen.keybindings();
        assert!(bindings.len() >= 7);
        let keys: Vec<&str> = bindings.iter().map(|b| b.key).collect();
        assert!(keys.contains(&"1/2/3/4"));
        assert!(keys.contains(&"Click"));
        assert!(keys.contains(&"Scroll"));
    }

    #[test]
    fn render_no_panic() {
        let screen = AdvancedFeatures::new();
        render_screen(&screen);
    }
}
