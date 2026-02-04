#![forbid(unsafe_code)]

//! Inline mode story screen.
//!
//! Demonstrates scrollback preservation by rendering a stable chrome bar while
//! logs stream underneath. Includes a compare toggle to contrast inline vs
//! alt-screen behavior inside the demo.

use std::collections::VecDeque;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

const MAX_LOG_LINES: usize = 2_000;
const INITIAL_LOG_LINES: usize = 60;
const LOG_RATE_OPTIONS: [usize; 4] = [1, 2, 5, 10];
const UI_HEIGHT_OPTIONS: [u16; 4] = [1, 2, 3, 4];

const LEVELS: [&str; 4] = ["INFO", "WARN", "ERROR", "DEBUG"];
const MODULES: [&str; 6] = ["core", "render", "runtime", "widgets", "io", "layout"];
const EVENTS: [&str; 8] = [
    "diff pass",
    "present frame",
    "flush writer",
    "resize coalesce",
    "cursor sync",
    "scrollback ok",
    "inline anchor",
    "budget check",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineAnchor {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    Inline,
    AltScreen,
}

/// Inline mode story screen state.
pub struct InlineModeStory {
    log_lines: VecDeque<String>,
    lines_generated: u64,
    tick_count: u64,
    log_rate_idx: usize,
    ui_height_idx: usize,
    anchor: InlineAnchor,
    compare: bool,
    mode: DisplayMode,
    paused: bool,
}

impl Default for InlineModeStory {
    fn default() -> Self {
        Self::new()
    }
}

impl InlineModeStory {
    pub fn new() -> Self {
        let mut log_lines = VecDeque::with_capacity(MAX_LOG_LINES + 16);
        for i in 0..INITIAL_LOG_LINES {
            log_lines.push_back(generate_log_line(i as u64));
        }
        Self {
            log_lines,
            lines_generated: INITIAL_LOG_LINES as u64,
            tick_count: 0,
            log_rate_idx: 1,
            ui_height_idx: 1,
            anchor: InlineAnchor::Bottom,
            compare: false,
            mode: DisplayMode::Inline,
            paused: false,
        }
    }

    pub fn set_ui_height(&mut self, height: u16) {
        let idx = UI_HEIGHT_OPTIONS
            .iter()
            .position(|&h| h == height)
            .unwrap_or(0);
        self.ui_height_idx = idx;
    }

    pub fn set_anchor(&mut self, anchor: InlineAnchor) {
        self.anchor = anchor;
    }

    pub fn set_compare(&mut self, compare: bool) {
        self.compare = compare;
    }

    pub fn set_mode(&mut self, mode: DisplayMode) {
        self.mode = mode;
    }

    fn ui_height(&self) -> u16 {
        UI_HEIGHT_OPTIONS[self.ui_height_idx]
    }

    fn log_rate(&self) -> usize {
        LOG_RATE_OPTIONS[self.log_rate_idx]
    }

    fn push_log_line(&mut self) {
        let line = generate_log_line(self.lines_generated);
        self.lines_generated = self.lines_generated.saturating_add(1);
        self.log_lines.push_back(line);
        if self.log_lines.len() > MAX_LOG_LINES {
            self.log_lines.pop_front();
        }
    }

    fn append_log_burst(&mut self, count: usize) {
        for _ in 0..count {
            self.push_log_line();
        }
    }

    fn cycle_log_rate(&mut self) {
        self.log_rate_idx = (self.log_rate_idx + 1) % LOG_RATE_OPTIONS.len();
    }

    fn cycle_ui_height(&mut self) {
        self.ui_height_idx = (self.ui_height_idx + 1) % UI_HEIGHT_OPTIONS.len();
    }

    fn toggle_anchor(&mut self) {
        self.anchor = match self.anchor {
            InlineAnchor::Top => InlineAnchor::Bottom,
            InlineAnchor::Bottom => InlineAnchor::Top,
        };
    }

    fn toggle_compare(&mut self) {
        self.compare = !self.compare;
    }

    fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            DisplayMode::Inline => DisplayMode::AltScreen,
            DisplayMode::AltScreen => DisplayMode::Inline,
        };
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let mode_label = match self.mode {
            DisplayMode::Inline => "Inline",
            DisplayMode::AltScreen => "Alt-screen",
        };
        let anchor_label = match self.anchor {
            InlineAnchor::Top => "Top",
            InlineAnchor::Bottom => "Bottom",
        };
        let compare_label = if self.compare { "ON" } else { "OFF" };
        let paused_label = if self.paused { "Paused" } else { "Live" };

        let line1 = format!(
            "Mode: {mode_label}  |  Compare: {compare_label}  |  Anchor: {anchor_label}  |  UI height: {}  |  Rate: {}/tick",
            self.ui_height(),
            self.log_rate()
        );
        let line2 = format!(
            "Status: {paused_label}  |  Lines: {}  |  Scrollback preserved in inline mode",
            self.lines_generated
        );

        let text = if area.height >= 2 {
            format!("{line1}\n{line2}")
        } else {
            line1
        };

        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(area, frame);
    }

    fn render_log_area(&self, frame: &mut Frame, area: Rect, style: Style) {
        if area.is_empty() {
            return;
        }
        let visible = visible_lines(&self.log_lines, area.height);
        Paragraph::new(visible).style(style).render(area, frame);
    }

    fn render_inline_bar(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let anchor = match self.anchor {
            InlineAnchor::Top => "TOP",
            InlineAnchor::Bottom => "BOTTOM",
        };
        let text = if area.height >= 2 {
            format!(
                " INLINE MODE - SCROLLBACK PRESERVED \n Anchor: {anchor}  |  UI height: {}  |  Log rate: {}/tick ",
                self.ui_height(),
                self.log_rate()
            )
        } else {
            format!(
                "INLINE - SCROLLBACK PRESERVED  |  Anchor: {anchor}  |  UI: {}",
                self.ui_height()
            )
        };

        let style = Style::new()
            .fg(theme::fg::PRIMARY)
            .bg(theme::accent::INFO)
            .bold();

        let bar_block = Block::new().borders(Borders::NONE).style(style);
        bar_block.render(area, frame);
        Paragraph::new(text).style(style).render(area, frame);
    }

    fn render_alt_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let text = if area.height >= 2 {
            " ALT-SCREEN MODE - SCROLLBACK HIDDEN \n Full-screen takeover (logs do not persist)"
        } else {
            "ALT-SCREEN - SCROLLBACK HIDDEN"
        };
        let style = Style::new()
            .fg(theme::fg::PRIMARY)
            .bg(theme::accent::WARNING)
            .bold();
        let bar_block = Block::new().borders(Borders::NONE).style(style);
        bar_block.render(area, frame);
        Paragraph::new(text).style(style).render(area, frame);
    }

    fn render_inline_pane(&self, frame: &mut Frame, area: Rect, title: &str) {
        if area.is_empty() {
            return;
        }
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title)
            .style(Style::new().fg(theme::fg::PRIMARY));
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }

        let ui_height = self.ui_height().min(inner.height.max(1));
        let log_height = inner.height.saturating_sub(ui_height);

        let (log_area, bar_area) = match self.anchor {
            InlineAnchor::Top => (
                Rect::new(inner.x, inner.y + ui_height, inner.width, log_height),
                Rect::new(inner.x, inner.y, inner.width, ui_height),
            ),
            InlineAnchor::Bottom => (
                Rect::new(inner.x, inner.y, inner.width, log_height),
                Rect::new(inner.x, inner.y + log_height, inner.width, ui_height),
            ),
        };

        self.render_log_area(
            frame,
            log_area,
            Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::BASE),
        );
        self.render_inline_bar(frame, bar_area);
    }

    fn render_alt_pane(&self, frame: &mut Frame, area: Rect, title: &str) {
        if area.is_empty() {
            return;
        }
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title)
            .style(Style::new().fg(theme::fg::PRIMARY));
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }

        let header_height = inner.height.min(2);
        let header = Rect::new(inner.x, inner.y, inner.width, header_height);
        let log_area = Rect::new(
            inner.x,
            inner.y + header_height,
            inner.width,
            inner.height.saturating_sub(header_height),
        );

        self.render_alt_header(frame, header);
        self.render_log_area(
            frame,
            log_area,
            Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::BASE),
        );
    }

    fn render_compare(&self, frame: &mut Frame, area: Rect) {
        let chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);

        self.render_inline_pane(frame, chunks[0], "Inline (scrollback preserved)");
        self.render_alt_pane(frame, chunks[1], "Alt-screen (scrollback hidden)");
    }

    fn render_single(&self, frame: &mut Frame, area: Rect) {
        match self.mode {
            DisplayMode::Inline => self.render_inline_pane(frame, area, "Inline Mode Story"),
            DisplayMode::AltScreen => self.render_alt_pane(frame, area, "Alt-screen Story"),
        }
    }
}

impl Screen for InlineModeStory {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        else {
            return Cmd::none();
        };

        match code {
            KeyCode::Char(' ') => {
                self.paused = !self.paused;
            }
            KeyCode::Char(ch) => match ch.to_ascii_lowercase() {
                'a' => self.toggle_anchor(),
                'c' => self.toggle_compare(),
                'h' => self.cycle_ui_height(),
                'm' => self.toggle_mode(),
                'r' => self.cycle_log_rate(),
                't' => self.append_log_burst(150),
                _ => {}
            },
            _ => {}
        }

        Cmd::none()
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let outer = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Inline Mode Story")
            .style(Style::new().fg(theme::fg::PRIMARY));
        let inner = outer.inner(area);
        outer.render(area, frame);
        if inner.is_empty() {
            return;
        }

        let header_height = match inner.height {
            0 | 1 => 0,
            2 | 3 => 1,
            _ => 2,
        };

        if header_height == 0 {
            if self.compare {
                self.render_compare(frame, inner);
            } else {
                self.render_single(frame, inner);
            }
            return;
        }

        let chunks = Flex::vertical()
            .constraints([Constraint::Fixed(header_height), Constraint::Fill])
            .split(inner);
        self.render_header(frame, chunks[0]);

        if self.compare {
            self.render_compare(frame, chunks[1]);
        } else {
            self.render_single(frame, chunks[1]);
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Space",
                action: "Pause/resume stream",
            },
            HelpEntry {
                key: "A",
                action: "Toggle chrome anchor",
            },
            HelpEntry {
                key: "H",
                action: "Cycle UI height",
            },
            HelpEntry {
                key: "R",
                action: "Cycle log rate",
            },
            HelpEntry {
                key: "C",
                action: "Toggle inline vs alt comparison",
            },
            HelpEntry {
                key: "M",
                action: "Toggle single view mode",
            },
            HelpEntry {
                key: "T",
                action: "Scrollback stress burst",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        if self.paused {
            return;
        }
        for _ in 0..self.log_rate() {
            self.push_log_line();
        }
    }

    fn title(&self) -> &'static str {
        "Inline Mode Story"
    }

    fn tab_label(&self) -> &'static str {
        "Inline"
    }
}

fn visible_lines(lines: &VecDeque<String>, height: u16) -> String {
    if height == 0 {
        return String::new();
    }
    let count = height as usize;
    let start = lines.len().saturating_sub(count);
    lines
        .iter()
        .skip(start)
        .take(count)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn generate_log_line(seq: u64) -> String {
    let level = LEVELS[(seq as usize) % LEVELS.len()];
    let module = MODULES[((seq / 3) as usize) % MODULES.len()];
    let event = EVENTS[((seq / 7) as usize) % EVENTS.len()];
    format!("{seq:06} [{level:<5}] {module:<7} {event}")
}
