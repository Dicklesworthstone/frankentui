#![forbid(unsafe_code)]

//! Shakespeare Library screen — complete works with search and virtualized scroll.

use std::cell::{Cell, OnceCell};

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::charts::Sparkline;
use ftui_extras::text_effects::{ColorGradient, Direction, RevealMode, StyledText, TextEffect};
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::{Cell as RenderCell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::search::SearchResult;
use ftui_text::{display_width, grapheme_width, graphemes};
use ftui_widgets::StatefulWidget;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};

use super::{HelpEntry, Screen, line_contains_ignore_case};
use crate::app::ScreenId;
use crate::chrome;
use crate::theme;

/// Embedded complete works of Shakespeare.
const SHAKESPEARE_TEXT: &str = include_str!("../../data/shakespeare.txt");

/// A table-of-contents entry (play/section title and its line number).
struct TocEntry {
    title: String,
    line: usize,
}

/// Shakespeare Library screen state.
pub struct Shakespeare {
    /// All lines of the text, as static string slices.
    lines: Vec<&'static str>,
    /// Current scroll offset (top visible line).
    scroll_offset: usize,
    /// Table of contents entries.
    toc_entries: OnceCell<Vec<TocEntry>>,
    /// Selected TOC entry.
    toc_selected: usize,
    /// Scroll offset for TOC list.
    toc_scroll: Cell<usize>,
    /// Search input widget.
    search_input: TextInput,
    /// Whether search bar is focused/visible.
    search_active: bool,
    /// Line indices matching the current search query.
    search_matches: Vec<usize>,
    /// Search match density (bucketed) for heatmap/sparkline.
    match_density: Vec<f64>,
    /// Index into search_matches for current highlighted match.
    current_match: usize,
    /// Viewport height (lines visible), updated each render.
    viewport_height: Cell<u16>,
    /// Animation tick counter.
    tick_count: u64,
    /// Animation time (seconds).
    time: f64,
    /// Current view mode.
    mode: ShakespeareMode,
    /// Focused panel for keyboard/mouse interaction.
    focus: FocusPanel,
    /// Layout hit areas for mouse focus.
    layout_search: Cell<Rect>,
    layout_text: Cell<Rect>,
    layout_nav: Cell<Rect>,
    layout_toc: Cell<Rect>,
    layout_insights: Cell<Rect>,
    layout_status: Cell<Rect>,
}

impl Default for Shakespeare {
    fn default() -> Self {
        Self::new()
    }
}

impl Shakespeare {
    pub fn new() -> Self {
        let lines: Vec<&'static str> = SHAKESPEARE_TEXT.lines().collect();

        let mut state = Self {
            lines,
            scroll_offset: 0,
            toc_entries: OnceCell::new(),
            toc_selected: 0,
            toc_scroll: Cell::new(0),
            search_input: TextInput::new()
                .with_placeholder("Search... (/ to focus, Esc to close)")
                .with_style(
                    Style::new()
                        .fg(theme::fg::PRIMARY)
                        .bg(theme::alpha::SURFACE),
                )
                .with_placeholder_style(Style::new().fg(theme::fg::MUTED)),
            search_active: false,
            search_matches: Vec::new(),
            match_density: Vec::new(),
            current_match: 0,
            viewport_height: Cell::new(20),
            tick_count: 0,
            time: 0.0,
            mode: ShakespeareMode::Library,
            focus: FocusPanel::Text,
            layout_search: Cell::new(Rect::default()),
            layout_text: Cell::new(Rect::default()),
            layout_nav: Cell::new(Rect::default()),
            layout_toc: Cell::new(Rect::default()),
            layout_insights: Cell::new(Rect::default()),
            layout_status: Cell::new(Rect::default()),
        };
        state.apply_theme();
        state.update_match_density();
        state
    }

    pub fn apply_theme(&mut self) {
        let input_style = Style::new()
            .fg(theme::fg::PRIMARY)
            .bg(theme::alpha::SURFACE);
        let placeholder_style = Style::new().fg(theme::fg::MUTED);
        self.search_input = self
            .search_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style);
    }

    /// Build table of contents from the text by detecting play titles.
    /// Titles are lines that appear in ALL CAPS and match known patterns.
    fn build_toc(lines: &[&str]) -> Vec<TocEntry> {
        let mut entries = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            // Skip empty lines and very short lines
            if trimmed.len() < 5 {
                continue;
            }
            // Detect section headers: lines that are all uppercase letters/spaces/punctuation
            // and appear after a blank line, typical of Gutenberg Shakespeare formatting.
            // Also check for "THE SONNETS", "ACT" headers, play titles, etc.
            if i > 0
                && trimmed.len() > 8
                && trimmed.len() < 80
                && is_title_line(trimmed)
                && (i == 0
                    || lines
                        .get(i.wrapping_sub(1))
                        .is_some_and(|l| l.trim().is_empty()))
            {
                entries.push(TocEntry {
                    title: trimmed.to_owned(),
                    line: i,
                });
            }
        }
        entries
    }

    fn toc_entries(&self) -> &Vec<TocEntry> {
        self.toc_entries
            .get_or_init(|| Self::build_toc(&self.lines))
    }

    fn total_lines(&self) -> usize {
        self.lines.len()
    }

    fn scroll_by(&mut self, delta: i32) {
        let max_offset = self
            .total_lines()
            .saturating_sub(self.viewport_height.get() as usize);
        if delta < 0 {
            self.scroll_offset = self
                .scroll_offset
                .saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.scroll_offset = (self.scroll_offset + delta as usize).min(max_offset);
        }
    }

    fn scroll_to(&mut self, line: usize) {
        let max_offset = self
            .total_lines()
            .saturating_sub(self.viewport_height.get() as usize);
        self.scroll_offset = line.min(max_offset);
    }

    fn perform_search(&mut self) {
        let query = self.search_input.value().to_owned();
        self.search_matches.clear();
        self.current_match = 0;
        if query.len() < 2 {
            self.match_density = vec![0.0; 48];
            return;
        }

        // Optimization: Pre-compute lowercase query to avoid allocating a new String
        // for every single line in the text (100k+ allocations).
        let query_lower = query.to_ascii_lowercase();

        for (i, line) in self.lines.iter().enumerate() {
            // Use allocation-free check
            if line_contains_ignore_case(line, &query_lower) {
                self.search_matches.push(i);
            }
        }
        // Jump to first match
        if let Some(&first) = self.search_matches.first() {
            self.scroll_to(first.saturating_sub(3));
        }
        self.update_match_density();
    }

    fn next_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.current_match = (self.current_match + 1) % self.search_matches.len();
        let line = self.search_matches[self.current_match];
        self.scroll_to(line.saturating_sub(3));
    }

    fn prev_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.current_match =
            (self.current_match + self.search_matches.len() - 1) % self.search_matches.len();
        let line = self.search_matches[self.current_match];
        self.scroll_to(line.saturating_sub(3));
    }

    /// Determine the current play/section name based on scroll position.
    fn current_section(&self) -> &str {
        let mut section = "Preamble";
        for entry in self.toc_entries().iter() {
            if entry.line <= self.scroll_offset {
                section = &entry.title;
            } else {
                break;
            }
        }
        section
    }

    fn current_spotlight_line(&self) -> &str {
        if self.lines.is_empty() {
            return "…";
        }
        let center = self
            .scroll_offset
            .saturating_add(self.viewport_height.get() as usize / 2)
            .min(self.lines.len().saturating_sub(1));
        for delta in 0..6usize {
            let forward = center.saturating_add(delta);
            if forward < self.lines.len() {
                let line = self.lines[forward].trim();
                if !line.is_empty() {
                    return line;
                }
            }
            if center >= delta {
                let back = center.saturating_sub(delta);
                let line = self.lines[back].trim();
                if !line.is_empty() {
                    return line;
                }
            }
        }
        self.lines[center]
    }

    fn set_focus(&mut self, focus: FocusPanel) {
        self.focus = focus;
        self.search_active = matches!(focus, FocusPanel::Search);
        self.search_input.set_focused(self.search_active);
    }

    fn update_match_density(&mut self) {
        let buckets = 48usize;
        let mut density = vec![0.0; buckets];
        if self.search_matches.is_empty() {
            self.match_density = density;
            return;
        }
        let total = self.total_lines().max(1);
        for &line in &self.search_matches {
            let idx = (line * buckets) / total;
            if let Some(slot) = density.get_mut(idx) {
                *slot += 1.0;
            }
        }
        let max = density
            .iter()
            .copied()
            .fold(0.0, |a, b| if b > a { b } else { a });
        if max > 0.0 {
            for slot in &mut density {
                *slot /= max;
            }
        }
        self.match_density = density;
    }

    fn set_current_match(&mut self, idx: usize) {
        if self.search_matches.is_empty() {
            return;
        }
        self.current_match = idx.min(self.search_matches.len() - 1);
        let line = self.search_matches[self.current_match];
        self.scroll_to(line.saturating_sub(3));
    }

    fn focus_from_point(&mut self, x: u16, y: u16) {
        let search = self.layout_search.get();
        let text = self.layout_text.get();
        let nav = self.layout_nav.get();
        let toc = self.layout_toc.get();
        let insights = self.layout_insights.get();
        let status = self.layout_status.get();

        if !search.is_empty() && search.contains(x, y) {
            self.set_focus(FocusPanel::Search);
            return;
        }
        if text.contains(x, y) {
            self.set_focus(FocusPanel::Text);
            return;
        }
        if nav.contains(x, y) {
            self.set_focus(FocusPanel::Navigator);
            return;
        }
        if toc.contains(x, y) {
            self.set_focus(FocusPanel::Toc);
            return;
        }
        if insights.contains(x, y) {
            self.set_focus(FocusPanel::Insights);
            return;
        }
        if status.contains(x, y) {
            self.set_focus(FocusPanel::Status);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShakespeareMode {
    Library,
    Spotlight,
    Concordance,
}

impl ShakespeareMode {
    fn next(self) -> Self {
        match self {
            Self::Library => Self::Spotlight,
            Self::Spotlight => Self::Concordance,
            Self::Concordance => Self::Library,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Library => Self::Concordance,
            Self::Spotlight => Self::Library,
            Self::Concordance => Self::Spotlight,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Spotlight => "Spotlight",
            Self::Concordance => "Concordance",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Library => "Full text + navigator",
            Self::Spotlight => "Stage view + FX",
            Self::Concordance => "Search intelligence",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPanel {
    Search,
    Text,
    Navigator,
    Toc,
    Insights,
    Status,
}

/// Check if a line looks like a title (mostly uppercase with allowed punctuation).
fn is_title_line(s: &str) -> bool {
    let alpha_count = s.chars().filter(|c| c.is_alphabetic()).count();
    if alpha_count < 4 {
        return false;
    }
    let upper_count = s.chars().filter(|c| c.is_uppercase()).count();
    // At least 80% uppercase letters
    upper_count * 100 / alpha_count.max(1) >= 80
}

impl Screen for Shakespeare {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.focus_from_point(mouse.x, mouse.y);
                    if self.focus == FocusPanel::Navigator {
                        let nav = self.layout_nav.get();
                        let rel_y = mouse.y.saturating_sub(nav.y);
                        let index = self.current_match.saturating_sub(2) + rel_y as usize;
                        self.set_current_match(index);
                    } else if self.focus == FocusPanel::Toc {
                        let toc = self.layout_toc.get();
                        let rel_y = mouse.y.saturating_sub(toc.y);
                        let idx = self.toc_scroll.get() + rel_y as usize;
                        let line = {
                            let toc_entries = self.toc_entries();
                            if idx < toc_entries.len() {
                                Some(toc_entries[idx].line)
                            } else {
                                None
                            }
                        };
                        if let Some(line) = line {
                            self.toc_selected = idx;
                            self.scroll_to(line.saturating_sub(2));
                        }
                    }
                }
                MouseEventKind::ScrollUp => {
                    if self.focus == FocusPanel::Text {
                        self.scroll_by(-3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if self.focus == FocusPanel::Text {
                        self.scroll_by(3);
                    }
                }
                _ => {}
            }
            return Cmd::None;
        }

        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return Cmd::None;
            }

            // Search mode input handling
            if self.search_active {
                match (key.code, key.modifiers) {
                    (KeyCode::Escape, _) => {
                        self.set_focus(FocusPanel::Text);
                        return Cmd::None;
                    }
                    (KeyCode::Enter, _) | (KeyCode::Down, _) => {
                        if self.search_matches.is_empty() {
                            self.perform_search();
                        } else {
                            self.next_match();
                        }
                        return Cmd::None;
                    }
                    (KeyCode::Up, _) => {
                        self.prev_match();
                        return Cmd::None;
                    }
                    (KeyCode::Tab, _) => {
                        self.next_match();
                        return Cmd::None;
                    }
                    _ => {
                        let handled = self.search_input.handle_event(event);
                        if handled {
                            // Live search on each keystroke
                            self.perform_search();
                        }
                        return Cmd::None;
                    }
                }
            }

            // Normal mode keybindings
            match (key.code, key.modifiers) {
                (KeyCode::Char('/'), Modifiers::NONE) => {
                    self.set_focus(FocusPanel::Search);
                }
                (KeyCode::Tab, Modifiers::NONE) => {
                    let next = match self.focus {
                        FocusPanel::Text => FocusPanel::Navigator,
                        FocusPanel::Navigator => FocusPanel::Toc,
                        FocusPanel::Toc => FocusPanel::Insights,
                        FocusPanel::Insights => FocusPanel::Text,
                        FocusPanel::Search => FocusPanel::Text,
                        FocusPanel::Status => FocusPanel::Text,
                    };
                    self.set_focus(next);
                }
                (KeyCode::Char('n'), Modifiers::NONE) => self.next_match(),
                (KeyCode::Char('N'), Modifiers::NONE) | (KeyCode::Char('n'), Modifiers::SHIFT) => {
                    self.prev_match();
                }
                (KeyCode::Char('m'), Modifiers::NONE) => {
                    self.mode = self.mode.next();
                }
                (KeyCode::Char('M'), _) | (KeyCode::Char('m'), Modifiers::SHIFT) => {
                    self.mode = self.mode.prev();
                }
                (KeyCode::Down, Modifiers::NONE) => match self.focus {
                    FocusPanel::Navigator => self.next_match(),
                    FocusPanel::Toc => {
                        let line = {
                            let toc_entries = self.toc_entries();
                            let next = self.toc_selected.saturating_add(1);
                            if next < toc_entries.len() {
                                Some(toc_entries[next].line)
                            } else {
                                None
                            }
                        };
                        if let Some(line) = line {
                            self.toc_selected = self.toc_selected.saturating_add(1);
                            self.scroll_to(line.saturating_sub(2));
                        }
                    }
                    _ => self.scroll_by(1),
                },
                (KeyCode::Up, Modifiers::NONE) => match self.focus {
                    FocusPanel::Navigator => self.prev_match(),
                    FocusPanel::Toc => {
                        if self.toc_selected > 0 {
                            self.toc_selected -= 1;
                            let line = self.toc_entries()[self.toc_selected].line;
                            self.scroll_to(line.saturating_sub(2));
                        }
                    }
                    _ => self.scroll_by(-1),
                },
                (KeyCode::Char('j'), Modifiers::NONE) => self.scroll_by(1),
                (KeyCode::Char('k'), Modifiers::NONE) => self.scroll_by(-1),
                (KeyCode::Char('d'), Modifiers::CTRL) | (KeyCode::PageDown, _) => {
                    self.scroll_by(self.viewport_height.get() as i32 / 2);
                }
                (KeyCode::Char('u'), Modifiers::CTRL) | (KeyCode::PageUp, _) => {
                    self.scroll_by(-(self.viewport_height.get() as i32 / 2));
                }
                (KeyCode::Home, _) | (KeyCode::Char('g'), Modifiers::NONE) => {
                    self.scroll_to(0);
                }
                (KeyCode::End, _) | (KeyCode::Char('G'), Modifiers::NONE) => {
                    self.scroll_to(self.total_lines());
                }
                (KeyCode::Enter, Modifiers::NONE) => {
                    if self.focus == FocusPanel::Toc
                        && let Some(entry) = self.toc_entries().get(self.toc_selected)
                    {
                        self.scroll_to(entry.line.saturating_sub(2));
                    }
                }
                _ => {}
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.height < 5 || area.width < 30 {
            Paragraph::new("Terminal too small")
                .style(theme::muted())
                .render(area, frame);
            return;
        }

        // Vertical layout: search bar (if active) + body + status
        let v_chunks = Flex::vertical()
            .constraints(if self.search_active {
                vec![
                    Constraint::Fixed(3),
                    Constraint::Min(4),
                    Constraint::Fixed(1),
                ]
            } else {
                vec![Constraint::Min(4), Constraint::Fixed(1)]
            })
            .split(area);

        let (body_area, status_area) = if self.search_active {
            self.render_search_bar(frame, v_chunks[0]);
            (v_chunks[1], v_chunks[2])
        } else {
            (v_chunks[0], v_chunks[1])
        };

        // Body: left (text) + right (mode-specific)
        let h_chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(66.0), Constraint::Percentage(34.0)])
            .split(body_area);

        let right_rows = match self.mode {
            ShakespeareMode::Library => Flex::vertical()
                .constraints([
                    Constraint::Percentage(40.0),
                    Constraint::Percentage(35.0),
                    Constraint::Percentage(25.0),
                ])
                .split(h_chunks[1]),
            ShakespeareMode::Spotlight => Flex::vertical()
                .constraints([Constraint::Percentage(55.0), Constraint::Percentage(45.0)])
                .split(h_chunks[1]),
            ShakespeareMode::Concordance => Flex::vertical()
                .constraints([
                    Constraint::Percentage(34.0),
                    Constraint::Percentage(33.0),
                    Constraint::Percentage(33.0),
                ])
                .split(h_chunks[1]),
        };

        self.layout_text.set(h_chunks[0]);
        self.layout_status.set(status_area);
        if self.search_active {
            self.layout_search.set(v_chunks[0]);
        } else {
            self.layout_search.set(Rect::default());
        }

        match self.mode {
            ShakespeareMode::Library => {
                self.layout_nav.set(right_rows[0]);
                self.layout_toc.set(right_rows[1]);
                self.layout_insights.set(right_rows[2]);
            }
            ShakespeareMode::Spotlight => {
                self.layout_nav.set(right_rows[0]);
                self.layout_toc.set(right_rows[1]);
                self.layout_insights.set(Rect::default());
            }
            ShakespeareMode::Concordance => {
                self.layout_nav.set(right_rows[0]);
                self.layout_toc.set(right_rows[1]);
                self.layout_insights.set(right_rows[2]);
            }
        }

        chrome::register_pane_hit(frame, h_chunks[0], ScreenId::Shakespeare);
        for &rect in &right_rows {
            chrome::register_pane_hit(frame, rect, ScreenId::Shakespeare);
        }
        chrome::register_pane_hit(frame, status_area, ScreenId::Shakespeare);
        if self.search_active {
            chrome::register_pane_hit(frame, v_chunks[0], ScreenId::Shakespeare);
        }

        self.render_text_panel(frame, h_chunks[0]);
        match self.mode {
            ShakespeareMode::Library => {
                self.render_match_panel(frame, right_rows[0]);
                self.render_toc_panel(frame, right_rows[1]);
                self.render_insights_panel(frame, right_rows[2]);
            }
            ShakespeareMode::Spotlight => {
                self.render_spotlight_panel(frame, right_rows[0]);
                self.render_stagecraft_panel(frame, right_rows[1]);
            }
            ShakespeareMode::Concordance => {
                self.render_concordance_panel(frame, right_rows[0]);
                self.render_toc_panel(frame, right_rows[1]);
                self.render_insights_panel(frame, right_rows[2]);
            }
        }
        self.render_status_bar(frame, status_area);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "/",
                action: "Search",
            },
            HelpEntry {
                key: "Tab",
                action: "Cycle focus",
            },
            HelpEntry {
                key: "Enter/↓/Tab",
                action: "Next match (while searching)",
            },
            HelpEntry {
                key: "↑",
                action: "Prev match (while searching)",
            },
            HelpEntry {
                key: "n/N",
                action: "Next/prev match",
            },
            HelpEntry {
                key: "m/M",
                action: "Cycle view mode",
            },
            HelpEntry {
                key: "j/k",
                action: "Scroll up/down",
            },
            HelpEntry {
                key: "g/G",
                action: "Top/bottom",
            },
            HelpEntry {
                key: "PgUp/PgDn",
                action: "Page scroll",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click panes to focus",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.time = tick_count as f64 * 0.1;
    }

    fn title(&self) -> &'static str {
        "Shakespeare Library"
    }

    fn tab_label(&self) -> &'static str {
        "Shakespeare"
    }
}

impl Shakespeare {
    fn render_search_bar(&self, frame: &mut Frame, area: Rect) {
        if area.height < 2 {
            self.search_input.render(area, frame);
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .split(area);

        // Header row: animated title + controls
        let header_cols = Flex::horizontal()
            .constraints([Constraint::Min(10), Constraint::Fixed(32)])
            .split(rows[0]);

        let title = StyledText::new("LIVE SEARCH")
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::cyberpunk(),
                speed: 0.6,
            })
            .effect(TextEffect::PulsingGlow {
                color: theme::accent::ACCENT_7.into(),
                speed: 2.2,
            })
            .bold()
            .time(self.time);
        title.render(header_cols[0], frame);

        let hint = truncate_to_width(
            "↑/↓ jump · Enter/Tab next · Esc close",
            header_cols[1].width,
        );
        let hint_fx = StyledText::new(hint)
            .effect(TextEffect::ColorWave {
                color1: theme::accent::PRIMARY.into(),
                color2: theme::accent::ACCENT_8.into(),
                speed: 1.0,
                wavelength: 10.0,
            })
            .time(self.time);
        hint_fx.render(header_cols[1], frame);

        // Input row: label + input + match count
        let input_cols = Flex::horizontal()
            .constraints([
                Constraint::Fixed(10),
                Constraint::Min(10),
                Constraint::Fixed(22),
            ])
            .split(rows[1]);

        let label = StyledText::new("Query")
            .effect(TextEffect::Pulse {
                speed: 1.4,
                min_alpha: 0.35,
            })
            .bold()
            .time(self.time);
        label.render(input_cols[0], frame);
        self.search_input.render(input_cols[1], frame);

        let match_info = if self.search_matches.is_empty() {
            if self.search_input.value().len() >= 2 {
                "No matches".to_owned()
            } else {
                "Type to search".to_owned()
            }
        } else {
            format!(
                "{}/{} matches",
                self.current_match + 1,
                self.search_matches.len()
            )
        };
        let match_info = truncate_to_width(&match_info, input_cols[2].width);
        let match_fx = StyledText::new(match_info)
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::sunset(),
                speed: 0.5,
            })
            .effect(TextEffect::Glow {
                color: theme::accent::WARNING.into(),
                intensity: 0.45,
            })
            .time(self.time);
        match_fx.render(input_cols[2], frame);

        // Status row: mode + current line info
        let status_cols = Flex::horizontal()
            .constraints([Constraint::Min(10), Constraint::Fixed(24)])
            .split(rows[2]);
        let status = if self.search_input.value().len() >= 2 {
            format!("Mode: {} · Instant highlight active", self.mode.label())
        } else {
            format!("Mode: {} · Search updates as you type", self.mode.label())
        };
        Paragraph::new(status)
            .style(theme::muted())
            .render(status_cols[0], frame);

        let jump_hint = StyledText::new("M switches mode · n/N jumps")
            .effect(TextEffect::Reveal {
                mode: RevealMode::CenterOut,
                progress: ((self.time * 0.6).sin() * 0.5 + 0.5).clamp(0.0, 1.0),
                seed: 21,
            })
            .time(self.time);
        jump_hint.render(status_cols[1], frame);
    }

    fn render_text_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Heavy)
            .title("Complete Works of William Shakespeare")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Text,
                theme::screen_accent::SHAKESPEARE,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 || inner.width < 5 {
            return;
        }

        self.viewport_height.set(inner.height);

        // Reserve 1 col for scrollbar on the right
        let text_width = inner.width.saturating_sub(1);
        let text_area = Rect::new(inner.x, inner.y, text_width, inner.height);
        let scrollbar_area = Rect::new(inner.x + text_width, inner.y, 1, inner.height);

        // Update viewport height (conceptual - we'd need &mut self, but we can use the value
        // from the area directly)
        let vh = inner.height as usize;

        // Render visible lines
        let query = self.search_input.value();
        let has_query = query.len() >= 2;
        let query_lower = if has_query {
            Some(query.to_ascii_lowercase())
        } else {
            None
        };

        for row in 0..vh {
            let line_idx = self.scroll_offset + row;
            if line_idx >= self.lines.len() {
                break;
            }

            let line = self.lines[line_idx];
            let line_area = Rect::new(
                text_area.x,
                text_area.y.saturating_add(row as u16),
                text_area.width,
                1,
            );

            let matches = if let Some(lower) = query_lower.as_deref() {
                search_ascii_case_insensitive_ranges(line, lower)
            } else {
                Vec::new()
            };

            // Determine style: highlight if it's a search match
            let is_current_match = has_query
                && !self.search_matches.is_empty()
                && self.search_matches.get(self.current_match) == Some(&line_idx);
            let is_any_match = !matches.is_empty();

            // Line number prefix with animated marker
            let num_width = 8u16.min(text_area.width);
            let content_width = text_area.width.saturating_sub(num_width);
            let marker_area = Rect::new(line_area.x, line_area.y, 1.min(num_width), 1);
            let num_area = Rect::new(line_area.x + 1, line_area.y, num_width.saturating_sub(1), 1);
            let content_area = Rect::new(line_area.x + num_width, line_area.y, content_width, 1);

            if marker_area.width > 0 {
                if is_current_match {
                    let marker = StyledText::new("▶")
                        .effect(TextEffect::PulsingGlow {
                            color: theme::accent::WARNING.into(),
                            speed: 2.0,
                        })
                        .time(self.time);
                    marker.render(marker_area, frame);
                } else if is_any_match {
                    Paragraph::new("•")
                        .style(Style::new().fg(theme::accent::INFO))
                        .render(marker_area, frame);
                } else {
                    Paragraph::new(" ")
                        .style(Style::new().fg(theme::fg::MUTED))
                        .render(marker_area, frame);
                }
            }

            let line_num = format!("{:>6} ", line_idx + 1);
            let num_style = if is_current_match {
                Style::new()
                    .fg(theme::accent::WARNING)
                    .attrs(StyleFlags::BOLD)
            } else if is_any_match {
                Style::new().fg(theme::accent::INFO)
            } else {
                Style::new().fg(theme::fg::MUTED)
            };
            Paragraph::new(line_num)
                .style(num_style)
                .render(num_area, frame);

            if content_area.width == 0 {
                continue;
            }

            if is_any_match {
                let mut fill = RenderCell::from_char(' ');
                fill.bg = if is_current_match {
                    theme::alpha::HIGHLIGHT.into()
                } else {
                    theme::alpha::SURFACE.into()
                };
                frame.buffer.fill(content_area, fill);
            }

            if !is_any_match || query.is_empty() {
                Paragraph::new(line)
                    .style(Style::new().fg(theme::fg::SECONDARY))
                    .render(content_area, frame);
                continue;
            }

            let mut cursor_x = content_area.x;
            let line_y = content_area.y;
            let max_x = content_area.right();
            let mut last = 0usize;

            for result in &matches {
                let start = result.range.start;
                let end = result.range.end.min(line.len());
                if cursor_x >= max_x {
                    break;
                }
                if start < last || start >= line.len() {
                    continue;
                }
                if !line.is_char_boundary(start) || !line.is_char_boundary(end) {
                    continue;
                }

                let before = &line[last..start];
                if !before.is_empty() && cursor_x < max_x {
                    let remaining = max_x.saturating_sub(cursor_x);
                    let clipped = truncate_to_width(before, remaining);
                    let width = display_width(clipped.as_str()) as u16;
                    if width > 0 {
                        let area = Rect::new(cursor_x, line_y, width.min(remaining), 1);
                        Paragraph::new(clipped)
                            .style(Style::new().fg(theme::fg::SECONDARY))
                            .render(area, frame);
                        cursor_x = cursor_x.saturating_add(width);
                    }
                }

                if cursor_x >= max_x {
                    break;
                }

                let matched = &line[start..end];
                let remaining = max_x.saturating_sub(cursor_x);
                let clipped = truncate_to_width(matched, remaining);
                let width = display_width(clipped.as_str()) as u16;
                if width == 0 {
                    break;
                }

                if is_current_match {
                    let glow = StyledText::new(clipped)
                        .base_color(theme::accent::WARNING.into())
                        .bg_color(theme::alpha::HIGHLIGHT.into())
                        .bold()
                        .effect(TextEffect::AnimatedGradient {
                            gradient: ColorGradient::sunset(),
                            speed: 0.7,
                        })
                        .effect(TextEffect::PulsingGlow {
                            color: PackedRgba::rgb(255, 200, 120),
                            speed: 2.2,
                        })
                        .effect(TextEffect::ChromaticAberration {
                            offset: 1,
                            direction: Direction::Right,
                            animated: true,
                            speed: 0.5,
                        })
                        .time(self.time)
                        .seed(self.tick_count);
                    glow.render(Rect::new(cursor_x, line_y, width.min(remaining), 1), frame);
                } else {
                    // Subtle pulse for other matches
                    let highlight = StyledText::new(clipped)
                        .base_color(theme::fg::PRIMARY.into())
                        .bg_color(theme::alpha::HIGHLIGHT.into())
                        .effect(TextEffect::Pulse {
                            speed: 0.8,
                            min_alpha: 0.6,
                        })
                        .time(self.time)
                        .seed(start as u64);
                    highlight.render(Rect::new(cursor_x, line_y, width.min(remaining), 1), frame);
                }
                cursor_x = cursor_x.saturating_add(width);
                last = end;
            }

            if cursor_x < max_x && last < line.len() {
                let tail = &line[last..];
                let remaining = max_x.saturating_sub(cursor_x);
                let clipped = truncate_to_width(tail, remaining);
                let width = display_width(clipped.as_str()) as u16;
                if width > 0 {
                    Paragraph::new(clipped)
                        .style(Style::new().fg(theme::fg::SECONDARY))
                        .render(Rect::new(cursor_x, line_y, width.min(remaining), 1), frame);
                }
            }
        }

        // Scrollbar
        let mut scrollbar_state = ScrollbarState::new(self.total_lines(), self.scroll_offset, vh);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::new().fg(theme::accent::PRIMARY))
            .track_style(Style::new().fg(theme::bg::SURFACE));
        StatefulWidget::render(&scrollbar, scrollbar_area, frame, &mut scrollbar_state);
    }

    fn render_toc_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Navigator")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Toc,
                theme::screen_accent::SHAKESPEARE,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 || inner.width < 5 {
            return;
        }

        let visible = inner.height as usize;
        let toc_entries = self.toc_entries();
        if toc_entries.is_empty() || self.toc_selected >= toc_entries.len() {
            return;
        }

        let max_scroll = toc_entries.len().saturating_sub(visible).max(0);
        let mut start = self.toc_scroll.get().min(max_scroll);
        if self.toc_selected < start {
            start = self.toc_selected;
        } else if self.toc_selected >= start + visible {
            start = self.toc_selected.saturating_sub(visible.saturating_sub(1));
        }
        self.toc_scroll.set(start);

        let end = (start + visible).min(toc_entries.len());
        let is_focused = self.focus == FocusPanel::Toc;

        for (i, entry) in toc_entries[start..end].iter().enumerate() {
            let row = inner.y + i as u16;
            let row_area = Rect::new(inner.x, row, inner.width, 1);
            let label = truncate_to_width(&entry.title, inner.width);
            if start + i == self.toc_selected {
                if is_focused {
                    StyledText::new(format!("▶ {label}"))
                        .effect(TextEffect::PulsingGlow {
                            color: theme::accent::PRIMARY.into(),
                            speed: 1.6,
                        })
                        .bold()
                        .time(self.time)
                        .render(row_area, frame);
                } else {
                    Paragraph::new(format!("▶ {label}"))
                        .style(
                            Style::new()
                                .fg(theme::fg::PRIMARY)
                                .bg(theme::alpha::SURFACE),
                        )
                        .render(row_area, frame);
                }
            } else {
                Paragraph::new(format!("  {label}"))
                    .style(Style::new().fg(theme::fg::SECONDARY))
                    .render(row_area, frame);
            }
        }
    }

    fn render_match_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Match Navigator")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Navigator,
                theme::screen_accent::SHAKESPEARE,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(1),
                Constraint::Fixed(1),
            ])
            .split(inner);

        let summary = if self.search_matches.is_empty() {
            "No matches yet".to_string()
        } else {
            format!(
                "{}/{} matches · density {:.0}%",
                self.current_match + 1,
                self.search_matches.len(),
                self.match_density
                    .iter()
                    .copied()
                    .fold(0.0, |a, b| if b > a { b } else { a })
                    * 100.0
            )
        };
        let summary = truncate_to_width(&summary, rows[0].width);
        StyledText::new(summary)
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::ocean(),
                speed: 0.45,
            })
            .time(self.time)
            .render(rows[0], frame);

        if !rows[1].is_empty() {
            let list_height = rows[1].height as usize;
            let start = self.current_match.saturating_sub(list_height / 2);
            let end = (start + list_height).min(self.search_matches.len());
            let is_focused = self.focus == FocusPanel::Navigator;
            for (i, match_idx) in self.search_matches[start..end].iter().enumerate() {
                let y = rows[1].y + i as u16;
                let row_area = Rect::new(rows[1].x, y, rows[1].width, 1);
                let line_text = self.lines[*match_idx];
                let snippet = truncate_to_width(line_text, row_area.width.saturating_sub(10));
                let label = format!("{:>6} {snippet}", match_idx + 1);
                if start + i == self.current_match {
                    if is_focused {
                        StyledText::new(format!("▶ {label}"))
                            .effect(TextEffect::PulsingGlow {
                                color: theme::accent::WARNING.into(),
                                speed: 1.8,
                            })
                            .time(self.time)
                            .render(row_area, frame);
                    } else {
                        Paragraph::new(format!("▶ {label}"))
                            .style(
                                Style::new()
                                    .fg(theme::fg::PRIMARY)
                                    .bg(theme::alpha::SURFACE),
                            )
                            .render(row_area, frame);
                    }
                } else {
                    let subtle = StyledText::new(format!("  {label}"))
                        .effect(TextEffect::Pulse {
                            speed: 0.6,
                            min_alpha: 0.7,
                        })
                        .time(self.time)
                        .seed(*match_idx as u64);
                    subtle.render(row_area, frame);
                }
            }
        }

        if !rows[2].is_empty() {
            if self.search_matches.is_empty() {
                Paragraph::new("Type / to search instantly")
                    .style(theme::muted())
                    .render(rows[2], frame);
            } else {
                let idx = self.search_matches[self.current_match];
                let preview = truncate_to_width(self.lines[idx], rows[2].width);
                let styled = StyledText::new(preview)
                    .effect(TextEffect::ColorWave {
                        color1: theme::accent::PRIMARY.into(),
                        color2: theme::accent::ACCENT_8.into(),
                        speed: 1.2,
                        wavelength: 12.0,
                    })
                    .time(self.time);
                styled.render(rows[2], frame);
            }
        }
    }

    fn render_spotlight_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Spotlight Stage")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Navigator,
                theme::screen_accent::SHAKESPEARE,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Min(1),
            ])
            .split(inner);

        let header = StyledText::new(format!("{} · {}", self.mode.label(), self.mode.subtitle()))
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::cyberpunk(),
                speed: 0.55,
            })
            .effect(TextEffect::Glow {
                color: theme::accent::ACCENT_7.into(),
                intensity: 0.4,
            })
            .time(self.time);
        header.render(rows[0], frame);

        let spotlight = self.current_spotlight_line();
        let spotlight = truncate_to_width(spotlight, rows[1].width);
        let spotlight_fx = StyledText::new(spotlight)
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::sunset(),
                speed: 0.8,
            })
            .effect(TextEffect::PulsingGlow {
                color: PackedRgba::rgb(255, 180, 120),
                speed: 1.6,
            })
            .effect(TextEffect::ChromaticAberration {
                offset: 1,
                direction: Direction::Right,
                animated: true,
                speed: 0.6,
            })
            .bold()
            .time(self.time);
        spotlight_fx.render(rows[1], frame);

        if rows[2].is_empty() {
            return;
        }

        let fx_rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .split(rows[2]);

        let cue_one = StyledText::new("CURTAIN UP · emotional arc ignites")
            .effect(TextEffect::ColorWave {
                color1: theme::accent::PRIMARY.into(),
                color2: theme::accent::ACCENT_8.into(),
                speed: 1.1,
                wavelength: 10.0,
            })
            .time(self.time);
        cue_one.render(fx_rows[0], frame);

        let cue_two = StyledText::new("SPOTLIGHT · semantic resonance rising")
            .effect(TextEffect::Reveal {
                mode: RevealMode::LeftToRight,
                progress: ((self.time * 0.5).sin() * 0.5 + 0.5).clamp(0.0, 1.0),
                seed: 13,
            })
            .effect(TextEffect::Glow {
                color: theme::accent::WARNING.into(),
                intensity: 0.3,
            })
            .time(self.time);
        cue_two.render(fx_rows[1], frame);

        let cue_three = StyledText::new("ECHO LENS · multi-match halo engaged")
            .effect(TextEffect::Scanline {
                intensity: 0.25,
                line_gap: 2,
                scroll: true,
                scroll_speed: 0.8,
                flicker: 0.05,
            })
            .time(self.time);
        cue_three.render(fx_rows[2], frame);
    }

    fn render_stagecraft_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Stagecraft Console")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Insights,
                theme::screen_accent::SHAKESPEARE,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Min(1),
            ])
            .split(inner);

        let cue_header = StyledText::new("LIVE CUES · timing + intensity")
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::ocean(),
                speed: 0.5,
            })
            .time(self.time);
        cue_header.render(rows[0], frame);

        let control_line = format!(
            "Matches: {:>4} · Focus: {:>7}",
            self.search_matches.len(),
            format!("{:?}", self.focus)
        );
        Paragraph::new(truncate_to_width(&control_line, rows[1].width))
            .style(theme::muted())
            .render(rows[1], frame);

        if rows[2].is_empty() {
            return;
        }

        let cues = [
            ("Entrance", 0.35),
            ("Conflict", 0.55),
            ("Reversal", 0.45),
            ("Finale", 0.7),
        ];
        for (i, (label, base)) in cues.iter().enumerate() {
            let row_y = rows[2].y + i as u16;
            if row_y >= rows[2].y + rows[2].height {
                break;
            }
            let intensity = (base + (self.time * 0.3 + i as f64).sin() * 0.1).clamp(0.1, 0.95);
            let bar_width = ((rows[2].width.saturating_sub(12)) as f64 * intensity) as usize;
            let bar = "█".repeat(bar_width);
            let line = format!("{label:>8} ▏{bar}");
            Paragraph::new(truncate_to_width(&line, rows[2].width))
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(Rect::new(rows[2].x, row_y, rows[2].width, 1), frame);
        }
    }

    fn render_concordance_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Concordance Engine")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Navigator,
                theme::screen_accent::SHAKESPEARE,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Min(1),
            ])
            .split(inner);

        let headline = StyledText::new("INSTANT SEARCH INTELLIGENCE")
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::gold(),
                speed: 0.6,
            })
            .time(self.time);
        headline.render(rows[0], frame);

        let query = self.search_input.value();
        let query = if query.is_empty() { "…" } else { query };
        let summary = format!(
            "Query: \"{}\" · Matches: {}",
            truncate_to_width(query, 18),
            self.search_matches.len()
        );
        let summary = truncate_to_width(&summary, rows[1].width);
        if query.len() >= 2 {
            let summary_fx = StyledText::new(summary)
                .effect(TextEffect::AnimatedGradient {
                    gradient: ColorGradient::sunset(),
                    speed: 0.55,
                })
                .effect(TextEffect::Pulse {
                    speed: 0.9,
                    min_alpha: 0.6,
                })
                .time(self.time);
            summary_fx.render(rows[1], frame);
        } else {
            Paragraph::new(summary)
                .style(theme::muted())
                .render(rows[1], frame);
        }

        if rows[2].is_empty() {
            return;
        }

        let density = if self.match_density.is_empty() {
            vec![0.0; 20]
        } else {
            self.match_density.clone()
        };
        Sparkline::new(&density)
            .style(Style::new().fg(theme::accent::PRIMARY))
            .gradient(
                theme::accent::PRIMARY.into(),
                theme::accent::ACCENT_8.into(),
            )
            .render(rows[2], frame);
    }

    fn render_insights_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Search Insights")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Insights,
                theme::screen_accent::SHAKESPEARE,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Min(1),
            ])
            .split(inner);

        let section = truncate_to_width(self.current_section(), rows[0].width);
        StyledText::new(format!("Section: {section}"))
            .effect(TextEffect::Reveal {
                mode: RevealMode::LeftToRight,
                progress: ((self.time * 0.4).sin() * 0.5 + 0.5).clamp(0.0, 1.0),
                seed: 7,
            })
            .time(self.time)
            .render(rows[0], frame);

        let stats = format!(
            "Matches: {} · View: {} lines",
            self.search_matches.len(),
            self.viewport_height.get()
        );
        Paragraph::new(truncate_to_width(&stats, rows[1].width))
            .style(theme::muted())
            .render(rows[1], frame);

        if !rows[2].is_empty() {
            let density = if self.match_density.is_empty() {
                vec![0.0; 20]
            } else {
                self.match_density.clone()
            };
            Sparkline::new(&density)
                .style(Style::new().fg(theme::accent::PRIMARY))
                .gradient(
                    theme::accent::PRIMARY.into(),
                    theme::accent::ACCENT_8.into(),
                )
                .render(rows[2], frame);
        }
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        frame.buffer.fill(
            area,
            RenderCell::default().with_bg(theme::alpha::SURFACE.into()),
        );

        let section = self.current_section();
        let total = self.total_lines();
        let pos = self.scroll_offset + 1;
        let pct = (self.scroll_offset * 100).checked_div(total).unwrap_or(0);

        let section_label = truncate_to_width(section, 40);
        let status = format!(
            " Mode: {} · Line {pos}/{total} ({pct}%) · Matches: {} | {}",
            self.mode.label(),
            self.search_matches.len(),
            section_label
        );

        let status_line = truncate_to_width(&status, area.width);
        if self.search_matches.is_empty() {
            Paragraph::new(status_line)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(area, frame);
        } else {
            StyledText::new(status_line)
                .base_color(theme::fg::PRIMARY.into())
                .effect(TextEffect::ColorWave {
                    color1: theme::accent::PRIMARY.into(),
                    color2: theme::accent::ACCENT_8.into(),
                    speed: 0.8,
                    wavelength: 10.0,
                })
                .time(self.time)
                .render(area, frame);
        }
    }
}

fn truncate_to_width(text: &str, max_width: u16) -> String {
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0usize;
    let max = max_width as usize;
    for grapheme in graphemes(text) {
        let w = grapheme_width(grapheme);
        if width + w > max {
            break;
        }
        out.push_str(grapheme);
        width += w;
    }
    out
}

/// Find ASCII case-insensitive matches without allocating lowercase haystacks per line.
fn search_ascii_case_insensitive_ranges(haystack: &str, needle_lower: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
    if needle_lower.is_empty() {
        return results;
    }
    let haystack_bytes = haystack.as_bytes();
    let needle_bytes = needle_lower.as_bytes();
    if needle_bytes.len() > haystack_bytes.len() {
        return results;
    }

    let mut i = 0;
    while i <= haystack_bytes.len() - needle_bytes.len() {
        let mut match_found = true;
        for j in 0..needle_bytes.len() {
            if haystack_bytes[i + j].to_ascii_lowercase() != needle_bytes[j] {
                match_found = false;
                break;
            }
        }
        if match_found {
            results.push(SearchResult::new(i, i + needle_bytes.len()));
            i += needle_bytes.len();
        } else {
            i += 1;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shakespeare_initial_state() {
        let s = Shakespeare::new();
        assert_eq!(s.scroll_offset, 0);
        assert!(s.total_lines() > 100_000, "Should have 100K+ lines");
        assert!(!s.toc_entries().is_empty(), "Should have TOC entries");
    }

    #[test]
    fn shakespeare_search_to_be() {
        let mut s = Shakespeare::new();
        s.search_input.set_value("To be, or not to be");
        s.perform_search();
        assert!(
            !s.search_matches.is_empty(),
            "Should find 'To be or not to be' in Shakespeare"
        );
    }

    #[test]
    fn shakespeare_search_ranges_case_insensitive() {
        let ranges = search_ascii_case_insensitive_ranges("To Be or not", "to");
        assert!(!ranges.is_empty());
        let first = &ranges[0];
        assert_eq!(first.range.start, 0);
        assert_eq!(first.range.end, 2);
    }

    #[test]
    fn shakespeare_scroll_to_end() {
        let mut s = Shakespeare::new();
        s.viewport_height.set(40);
        s.scroll_to(s.total_lines());
        let max_offset = s
            .total_lines()
            .saturating_sub(s.viewport_height.get() as usize);
        assert_eq!(s.scroll_offset, max_offset);
    }

    #[test]
    fn shakespeare_scroll_navigation() {
        let mut s = Shakespeare::new();
        s.viewport_height.set(40);
        s.scroll_by(10);
        assert_eq!(s.scroll_offset, 10);
        s.scroll_by(-5);
        assert_eq!(s.scroll_offset, 5);
        // Can't scroll below 0
        s.scroll_by(-100);
        assert_eq!(s.scroll_offset, 0);
    }

    #[test]
    fn shakespeare_toc_has_plays() {
        let s = Shakespeare::new();
        let titles: Vec<&str> = s.toc_entries().iter().map(|e| e.title.as_str()).collect();
        // Should find major plays
        assert!(
            titles.iter().any(|t| t.contains("HAMLET")),
            "TOC should contain Hamlet, found: {:?}",
            &titles[..titles.len().min(20)]
        );
    }
}
