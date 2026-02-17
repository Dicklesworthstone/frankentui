#![forbid(unsafe_code)]

//! Inline mode story screen.
//!
//! Demonstrates scrollback preservation by rendering a stable chrome bar while
//! logs stream underneath. Includes a compare toggle to contrast inline vs
//! alt-screen behavior inside the demo.

use std::cell::Cell as StdCell;
use std::collections::VecDeque;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
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
    // View-updated layout rects for next-tick mouse hit testing.
    layout_header: StdCell<Rect>,
    layout_content: StdCell<Rect>,
    layout_inline_bar: StdCell<Rect>,
    layout_alt_header: StdCell<Rect>,
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
            layout_header: StdCell::new(Rect::default()),
            layout_content: StdCell::new(Rect::default()),
            layout_inline_bar: StdCell::new(Rect::default()),
            layout_alt_header: StdCell::new(Rect::default()),
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

    fn cycle_log_rate_down(&mut self) {
        self.log_rate_idx =
            (self.log_rate_idx + LOG_RATE_OPTIONS.len() - 1) % LOG_RATE_OPTIONS.len();
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

    fn reset_to_defaults(&mut self) {
        self.log_rate_idx = 1;
        self.ui_height_idx = 1;
        self.anchor = InlineAnchor::Bottom;
        self.compare = false;
        self.mode = DisplayMode::Inline;
        self.paused = false;
        self.log_lines.clear();
        self.lines_generated = 0;
        for i in 0..INITIAL_LOG_LINES {
            self.log_lines.push_back(generate_log_line(i as u64));
        }
        self.lines_generated = INITIAL_LOG_LINES as u64;
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

        self.layout_inline_bar.set(bar_area);

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

        self.layout_alt_header.set(header);

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
        if let Event::Mouse(mouse) = event {
            let header = self.layout_header.get();
            let content = self.layout_content.get();
            let inline_bar = self.layout_inline_bar.get();
            let alt_header = self.layout_alt_header.get();

            match mouse.kind {
                MouseEventKind::Up(MouseButton::Left) => {
                    if inline_bar.contains(mouse.x, mouse.y) {
                        self.toggle_anchor();
                    } else if header.contains(mouse.x, mouse.y) {
                        self.toggle_compare();
                    } else if alt_header.contains(mouse.x, mouse.y) {
                        // When comparing, clicking the alt header "drills in" to alt-screen mode.
                        if self.compare {
                            self.compare = false;
                            self.mode = DisplayMode::AltScreen;
                        } else {
                            self.toggle_mode();
                        }
                    } else if content.contains(mouse.x, mouse.y) {
                        self.paused = !self.paused;
                    }
                }
                MouseEventKind::ScrollUp => {
                    if content.contains(mouse.x, mouse.y) {
                        self.cycle_log_rate();
                    }
                }
                MouseEventKind::ScrollDown => {
                    if content.contains(mouse.x, mouse.y) {
                        self.cycle_log_rate_down();
                    }
                }
                _ => {}
            }

            return Cmd::none();
        }

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
                'd' => self.reset_to_defaults(),
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

        // Update layout rects for mouse hit testing on the *next* event dispatch.
        self.layout_header.set(Rect::default());
        self.layout_content.set(Rect::default());
        self.layout_inline_bar.set(Rect::default());
        self.layout_alt_header.set(Rect::default());

        if area.is_empty() {
            return;
        }

        self.layout_content.set(area);

        let header_height = match area.height {
            0 | 1 => 0,
            2 | 3 => 1,
            _ => 2,
        };

        if header_height == 0 {
            if self.compare {
                self.render_compare(frame, area);
            } else {
                self.render_single(frame, area);
            }
            return;
        }

        let chunks = Flex::vertical()
            .constraints([Constraint::Fixed(header_height), Constraint::Fill])
            .split(area);
        self.layout_header.set(chunks[0]);
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
                action: "Toggle chrome anchor (top/bottom)",
            },
            HelpEntry {
                key: "C",
                action: "Toggle inline vs alt comparison",
            },
            HelpEntry {
                key: "D",
                action: "Reset story to defaults",
            },
            HelpEntry {
                key: "H",
                action: "Cycle UI height",
            },
            HelpEntry {
                key: "M",
                action: "Toggle single view mode",
            },
            HelpEntry {
                key: "R",
                action: "Cycle log rate",
            },
            HelpEntry {
                key: "T",
                action: "Scrollback stress burst",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click header: compare • click bar: anchor • click log: pause",
            },
            HelpEntry {
                key: "Wheel",
                action: "Adjust log rate (scrollback preserved when mouse off)",
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

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{MouseButton, MouseEvent, MouseEventKind};
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn mouse_click_on_inline_bar_toggles_anchor() {
        let mut state = InlineModeStory::new();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        state.view(&mut frame, Rect::new(0, 0, 80, 24));

        let bar = state.layout_inline_bar.get();
        assert!(!bar.is_empty(), "inline bar should be laid out");

        let click = Event::Mouse(MouseEvent::new(
            MouseEventKind::Up(MouseButton::Left),
            bar.x + 1,
            bar.y,
        ));
        state.update(&click);

        assert_eq!(state.anchor, InlineAnchor::Top);
    }

    #[test]
    fn mouse_click_on_header_toggles_compare() {
        let mut state = InlineModeStory::new();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        state.view(&mut frame, Rect::new(0, 0, 80, 24));

        let header = state.layout_header.get();
        assert!(!header.is_empty(), "header should be present at this size");

        let click = Event::Mouse(MouseEvent::new(
            MouseEventKind::Up(MouseButton::Left),
            header.x + 1,
            header.y,
        ));
        state.update(&click);

        assert!(state.compare);
    }

    #[test]
    fn mouse_wheel_adjusts_log_rate() {
        let mut state = InlineModeStory::new();

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        state.view(&mut frame, Rect::new(0, 0, 80, 24));

        let before = state.log_rate();
        let content = state.layout_content.get();
        assert!(!content.is_empty(), "content should be laid out");

        let scroll = Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            content.x + 1,
            content.y + 1,
        ));
        state.update(&scroll);

        assert_ne!(state.log_rate(), before);
    }

    fn press(ch: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(ch),
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::empty(),
        })
    }

    #[test]
    fn d_key_resets_to_defaults() {
        let mut state = InlineModeStory::new();
        state.toggle_anchor();
        state.toggle_compare();
        state.toggle_mode();
        state.paused = true;
        state.log_rate_idx = 3;
        state.ui_height_idx = 3;
        state.append_log_burst(500);
        assert!(state.lines_generated > INITIAL_LOG_LINES as u64);

        state.update(&press('d'));

        assert_eq!(state.anchor, InlineAnchor::Bottom);
        assert!(!state.compare);
        assert_eq!(state.mode, DisplayMode::Inline);
        assert!(!state.paused);
        assert_eq!(state.log_rate_idx, 1);
        assert_eq!(state.ui_height_idx, 1);
        assert_eq!(state.lines_generated, INITIAL_LOG_LINES as u64);
        assert_eq!(state.log_lines.len(), INITIAL_LOG_LINES);
    }

    #[test]
    fn space_key_toggles_pause() {
        let mut state = InlineModeStory::new();
        assert!(!state.paused);
        state.update(&Event::Key(KeyEvent {
            code: KeyCode::Char(' '),
            kind: KeyEventKind::Press,
            modifiers: ftui_core::event::Modifiers::empty(),
        }));
        assert!(state.paused);
    }

    #[test]
    fn keyboard_shortcuts_toggle_state() {
        let mut state = InlineModeStory::new();
        assert_eq!(state.anchor, InlineAnchor::Bottom);
        state.update(&press('a'));
        assert_eq!(state.anchor, InlineAnchor::Top);

        assert!(!state.compare);
        state.update(&press('c'));
        assert!(state.compare);

        assert_eq!(state.mode, DisplayMode::Inline);
        state.update(&press('m'));
        assert_eq!(state.mode, DisplayMode::AltScreen);
    }

    #[test]
    fn keybindings_returns_entries() {
        let state = InlineModeStory::new();
        let bindings = state.keybindings();
        assert!(bindings.len() >= 5);
        let keys: Vec<&str> = bindings.iter().map(|e| e.key).collect();
        assert!(keys.contains(&"D"));
        assert!(keys.contains(&"Space"));
    }

    #[test]
    fn tick_paused_does_not_grow_logs() {
        let mut state = InlineModeStory::new();
        state.paused = true;
        let before = state.lines_generated;
        state.tick(100);
        assert_eq!(state.lines_generated, before);
    }
}
