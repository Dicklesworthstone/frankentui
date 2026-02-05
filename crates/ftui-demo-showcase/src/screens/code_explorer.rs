#![forbid(unsafe_code)]

//! Code Explorer screen — SQLite C source with syntax highlighting and search.

use std::cell::Cell;

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::charts::Sparkline;
use ftui_extras::filesize;
use ftui_extras::syntax::{GenericTokenizer, GenericTokenizerConfig, SyntaxHighlighter};
use ftui_extras::text_effects::{
    ColorGradient, Direction, StyledMultiLine, StyledText, TextEffect,
};
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::{Cell as RenderCell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::search::search_ascii_case_insensitive;
use ftui_text::{display_width, grapheme_count, grapheme_width, graphemes};
use ftui_widgets::StatefulWidget;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::json_view::JsonView;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::{MiniBar, MiniBarColors};
use ftui_widgets::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};

use super::{HelpEntry, Screen, line_contains_ignore_case};
use crate::app::ScreenId;
use crate::chrome;
use crate::theme;

/// Embedded SQLite amalgamation source.
const SQLITE_SOURCE: &str = include_str!("../../data/sqlite3.c");

struct Hotspot {
    label: &'static str,
    line: usize,
}

struct ResultRow {
    play: &'static str,
    speaker: &'static str,
    line: &'static str,
}

const HOTSPOT_QUERIES: &[(&str, &str)] = &[
    ("sqlite3_open", "sqlite3_open"),
    ("sqlite3_prepare_v2", "sqlite3_prepare_v2"),
    ("sqlite3_step", "sqlite3_step"),
    ("sqlite3_finalize", "sqlite3_finalize"),
    ("sqlite3_exec", "sqlite3_exec"),
    ("sqlite3_close", "sqlite3_close"),
    ("pager", "Pager"),
    ("btree", "Btree"),
    ("vdb", "Vdbe"),
];

const FEATURE_SPOTLIGHT: &[&str] = &[
    "One-writer rule · deterministic terminal output",
    "Inline mode · scrollback preserved",
    "Buffer → Diff → Presenter pipeline",
    "16-byte Cell · SIMD-friendly rendering",
    "Frame budget · graceful degradation",
];

const QUERY_SNIPPETS: &[&str] = &[
    "SELECT play, line, speaker\nFROM shakespeare\nWHERE line LIKE '%love%'\nORDER BY line;\n",
    "WITH ranked AS (\n  SELECT line, rank() OVER (ORDER BY length(line) DESC) AS r\n  FROM shakespeare\n)\nSELECT * FROM ranked WHERE r <= 5;\n",
    "SELECT speaker, count(*) AS lines\nFROM shakespeare\nGROUP BY speaker\nORDER BY lines DESC\nLIMIT 5;\n",
];

const RESULT_PREVIEWS: &[&str] = &[
    "rows=128 · p95=2.4ms · cache=99%",
    "rows=5 · p95=0.9ms · cache=97%",
    "rows=5 · p95=1.6ms · cache=95%",
];

const RESULT_SETS: &[&[ResultRow]] = &[
    &[
        ResultRow {
            play: "HAMLET",
            speaker: "HAMLET",
            line: "To be, or not to be, that is the question:",
        },
        ResultRow {
            play: "HAMLET",
            speaker: "OPHELIA",
            line: "O, what a noble mind is here o'erthrown!",
        },
        ResultRow {
            play: "ROMEO",
            speaker: "ROMEO",
            line: "But, soft! what light through yonder window breaks?",
        },
    ],
    &[
        ResultRow {
            play: "MACBETH",
            speaker: "MACBETH",
            line: "Is this a dagger which I see before me?",
        },
        ResultRow {
            play: "MACBETH",
            speaker: "LADY M.",
            line: "Out, damned spot! out, I say!",
        },
        ResultRow {
            play: "LEAR",
            speaker: "LEAR",
            line: "How sharper than a serpent's tooth it is",
        },
    ],
    &[
        ResultRow {
            play: "JULIUS",
            speaker: "BRUTUS",
            line: "Not that I loved Caesar less, but that I loved Rome more.",
        },
        ResultRow {
            play: "JULIUS",
            speaker: "CAESAR",
            line: "Et tu, Brute? Then fall, Caesar!",
        },
        ResultRow {
            play: "OTHELLO",
            speaker: "OTHELLO",
            line: "Put out the light, and then put out the light.",
        },
    ],
];

const SCHEMA_PREVIEW: &[&str] = &[
    "table shakespeare(play TEXT, line TEXT, speaker TEXT)",
    "index idx_speaker ON shakespeare(speaker)",
    "index idx_play ON shakespeare(play)",
    "virtual table fts_shakespeare using fts5(line, speaker)",
    "table plays(id INTEGER PRIMARY KEY, title TEXT, year INT)",
];

const PLAN_GRAPH: &[&str] = &[
    "SCAN shakespeare",
    " ├─ FILTER line LIKE ?",
    " ├─ PROJECT speaker,line",
    " └─ SORT (line, speaker)",
];

/// C language tokenizer configuration.
fn c_tokenizer() -> GenericTokenizer {
    GenericTokenizer::new(GenericTokenizerConfig {
        name: "C",
        extensions: &["c", "h"],
        keywords: &[
            "auto",
            "break",
            "case",
            "const",
            "continue",
            "default",
            "do",
            "else",
            "enum",
            "extern",
            "for",
            "goto",
            "if",
            "inline",
            "register",
            "restrict",
            "return",
            "sizeof",
            "static",
            "struct",
            "switch",
            "typedef",
            "union",
            "volatile",
            "while",
            "_Alignas",
            "_Alignof",
            "_Atomic",
            "_Bool",
            "_Complex",
            "_Generic",
            "_Imaginary",
            "_Noreturn",
            "_Static_assert",
            "_Thread_local",
        ],
        control_keywords: &[
            "if", "else", "for", "while", "do", "switch", "case", "default", "break", "continue",
            "return", "goto",
        ],
        type_keywords: &[
            "void",
            "char",
            "short",
            "int",
            "long",
            "float",
            "double",
            "signed",
            "unsigned",
            "size_t",
            "ssize_t",
            "int8_t",
            "int16_t",
            "int32_t",
            "int64_t",
            "uint8_t",
            "uint16_t",
            "uint32_t",
            "uint64_t",
            "ptrdiff_t",
            "intptr_t",
            "uintptr_t",
            "FILE",
            "NULL",
        ],
        line_comment: "//",
        block_comment_start: "/*",
        block_comment_end: "*/",
    })
}

/// Code Explorer screen state.
pub struct CodeExplorer {
    /// All source lines.
    lines: Vec<&'static str>,
    /// Current scroll offset (top visible line).
    scroll_offset: usize,
    /// Viewport height in lines.
    viewport_height: Cell<u16>,
    /// Syntax highlighter with C language support.
    highlighter: SyntaxHighlighter,
    /// Search input.
    search_input: TextInput,
    /// Whether search bar is active.
    search_active: bool,
    /// Goto-line input.
    goto_input: TextInput,
    /// Whether goto-line is active.
    goto_active: bool,
    /// Line indices matching search query.
    search_matches: Vec<usize>,
    /// Search match density (bucketed) for radar/sparkline.
    match_density: Vec<f64>,
    /// Current match index.
    current_match: usize,
    /// File metadata as JSON string.
    metadata_json: String,
    /// Animation tick counter.
    tick_count: u64,
    /// Animation time (seconds).
    time: f64,
    /// Hotspot locations in the SQLite source.
    hotspots: Vec<Hotspot>,
    /// Current hotspot index.
    current_hotspot: usize,
    /// Feature spotlight index.
    feature_index: usize,
    /// Current view mode.
    mode: ExplorerMode,
    /// Focused panel for interaction.
    focus: FocusPanel,
    /// Layout hit areas for mouse focus.
    layout_input: Cell<Rect>,
    layout_code: Cell<Rect>,
    layout_telemetry: Cell<Rect>,
    layout_info: Cell<Rect>,
    layout_context: Cell<Rect>,
    layout_hotspots: Cell<Rect>,
    layout_radar: Cell<Rect>,
    layout_spotlight: Cell<Rect>,
}

impl Default for CodeExplorer {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeExplorer {
    pub fn new() -> Self {
        let lines: Vec<&'static str> = SQLITE_SOURCE.lines().collect();
        let line_count = lines.len();
        let byte_size = SQLITE_SOURCE.len() as u64;

        let mut highlighter = SyntaxHighlighter::new();
        highlighter.register_tokenizer(Box::new(c_tokenizer()));
        highlighter.set_theme(theme::syntax_theme());

        let metadata_json = format!(
            "{{\n  \"filename\": \"sqlite3.c\",\n  \"lines\": {},\n  \"size\": \"{}\",\n  \"size_bytes\": {},\n  \"language\": \"C\",\n  \"description\": \"SQLite amalgamation\"\n}}",
            line_count,
            filesize::decimal(byte_size),
            byte_size,
        );

        let hotspots = Self::build_hotspots(&lines);

        Self {
            lines,
            scroll_offset: 0,
            viewport_height: Cell::new(30),
            highlighter,
            search_input: TextInput::new()
                .with_placeholder("Search code... (/ to focus)")
                .with_style(
                    Style::new()
                        .fg(theme::fg::PRIMARY)
                        .bg(theme::alpha::SURFACE),
                )
                .with_placeholder_style(Style::new().fg(theme::fg::MUTED)),
            search_active: false,
            goto_input: TextInput::new()
                .with_placeholder("Line number...")
                .with_style(
                    Style::new()
                        .fg(theme::fg::PRIMARY)
                        .bg(theme::alpha::SURFACE),
                )
                .with_placeholder_style(Style::new().fg(theme::fg::MUTED)),
            goto_active: false,
            search_matches: Vec::new(),
            match_density: Vec::new(),
            current_match: 0,
            metadata_json,
            tick_count: 0,
            time: 0.0,
            hotspots,
            current_hotspot: 0,
            feature_index: 0,
            mode: ExplorerMode::Source,
            focus: FocusPanel::Code,
            layout_input: Cell::new(Rect::default()),
            layout_code: Cell::new(Rect::default()),
            layout_telemetry: Cell::new(Rect::default()),
            layout_info: Cell::new(Rect::default()),
            layout_context: Cell::new(Rect::default()),
            layout_hotspots: Cell::new(Rect::default()),
            layout_radar: Cell::new(Rect::default()),
            layout_spotlight: Cell::new(Rect::default()),
        }
    }

    pub fn apply_theme(&mut self) {
        self.highlighter.set_theme(theme::syntax_theme());
        let input_style = Style::new()
            .fg(theme::fg::PRIMARY)
            .bg(theme::alpha::SURFACE);
        let placeholder_style = Style::new().fg(theme::fg::MUTED);
        self.search_input = self
            .search_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style);
        self.goto_input = self
            .goto_input
            .clone()
            .with_style(input_style)
            .with_placeholder_style(placeholder_style);
    }

    fn total_lines(&self) -> usize {
        self.lines.len()
    }

    fn build_hotspots(lines: &[&'static str]) -> Vec<Hotspot> {
        let mut hotspots = Vec::new();
        for (label, needle) in HOTSPOT_QUERIES {
            let mut found = None;
            for (idx, line) in lines.iter().enumerate() {
                if line.contains(needle) {
                    found = Some(idx);
                    break;
                }
            }
            if let Some(line) = found {
                hotspots.push(Hotspot { label, line });
            }
        }
        hotspots
    }

    fn current_hotspot(&self) -> Option<&Hotspot> {
        if self.hotspots.is_empty() {
            None
        } else {
            Some(&self.hotspots[self.current_hotspot % self.hotspots.len()])
        }
    }

    fn next_hotspot(&mut self) {
        if !self.hotspots.is_empty() {
            self.current_hotspot = (self.current_hotspot + 1) % self.hotspots.len();
            let line = self.hotspots[self.current_hotspot].line;
            self.scroll_to(line.saturating_sub(3));
        }
    }

    fn prev_hotspot(&mut self) {
        if !self.hotspots.is_empty() {
            self.current_hotspot =
                (self.current_hotspot + self.hotspots.len() - 1) % self.hotspots.len();
            let line = self.hotspots[self.current_hotspot].line;
            self.scroll_to(line.saturating_sub(3));
        }
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
        // for every single line in the text.
        let query_lower = query.to_ascii_lowercase();

        for (i, line) in self.lines.iter().enumerate() {
            if line_contains_ignore_case(line, &query_lower) {
                self.search_matches.push(i);
            }
        }
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

    fn goto_line(&mut self) {
        if let Ok(line_num) = self.goto_input.value().trim().parse::<usize>()
            && line_num > 0
        {
            self.scroll_to(line_num.saturating_sub(1));
        }
    }

    /// Find the nearest function context from comments/declarations near current position.
    fn current_context(&self) -> &str {
        // Walk backwards from scroll position looking for a function-like declaration
        let start = self.scroll_offset;
        for i in (start.saturating_sub(200)..=start).rev() {
            if i >= self.lines.len() {
                continue;
            }
            let line = self.lines[i].trim();
            // C function definition: something like `int foo(` or `static void bar(`
            if line.contains('(')
                && !line.starts_with(' ')
                && !line.starts_with('\t')
                && !line.starts_with("/*")
                && !line.starts_with("*")
                && !line.starts_with("#")
                && line.len() > 5
            {
                return self.lines[i];
            }
        }
        "Top of file"
    }

    fn set_focus(&mut self, focus: FocusPanel) {
        self.focus = focus;
        if matches!(focus, FocusPanel::Input) {
            self.search_active = true;
            self.search_input.set_focused(true);
        }
    }

    fn reset_sidebar_layouts(&self) {
        self.layout_info.set(Rect::default());
        self.layout_context.set(Rect::default());
        self.layout_hotspots.set(Rect::default());
        self.layout_radar.set(Rect::default());
        self.layout_spotlight.set(Rect::default());
    }

    fn query_index(&self) -> usize {
        if QUERY_SNIPPETS.is_empty() {
            0
        } else {
            (self.tick_count / 12) as usize % QUERY_SNIPPETS.len()
        }
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

    fn synthetic_series(
        &self,
        len: usize,
        base: f64,
        amp: f64,
        speed: f64,
        phase: f64,
    ) -> Vec<f64> {
        let mut out = Vec::with_capacity(len.max(1));
        for i in 0..len.max(1) {
            let t = self.time * speed + i as f64 * 0.2 + phase;
            let v = base + amp * t.sin();
            out.push(v.clamp(0.0, 100.0));
        }
        out
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
        self.search_active = false;
        self.search_input.set_focused(false);
        self.goto_active = false;
        self.goto_input.set_focused(false);
        let input = self.layout_input.get();
        let code = self.layout_code.get();
        let telemetry = self.layout_telemetry.get();
        let info = self.layout_info.get();
        let context = self.layout_context.get();
        let hotspots = self.layout_hotspots.get();
        let radar = self.layout_radar.get();
        let spotlight = self.layout_spotlight.get();

        if !input.is_empty() && input.contains(x, y) {
            self.set_focus(FocusPanel::Input);
            return;
        }
        if code.contains(x, y) {
            self.focus = FocusPanel::Code;
            return;
        }
        if !telemetry.is_empty() && telemetry.contains(x, y) {
            self.focus = FocusPanel::Telemetry;
            return;
        }
        if info.contains(x, y) {
            self.focus = FocusPanel::Info;
            return;
        }
        if context.contains(x, y) {
            self.focus = FocusPanel::Context;
            return;
        }
        if hotspots.contains(x, y) {
            self.focus = FocusPanel::Hotspots;
            return;
        }
        if radar.contains(x, y) {
            self.focus = FocusPanel::Radar;
            return;
        }
        if spotlight.contains(x, y) {
            self.focus = FocusPanel::Spotlight;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplorerMode {
    Source,
    QueryLab,
    ExecutionPlan,
}

impl ExplorerMode {
    fn next(self) -> Self {
        match self {
            Self::Source => Self::QueryLab,
            Self::QueryLab => Self::ExecutionPlan,
            Self::ExecutionPlan => Self::Source,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Source => Self::ExecutionPlan,
            Self::QueryLab => Self::Source,
            Self::ExecutionPlan => Self::QueryLab,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Source => "Source",
            Self::QueryLab => "Query Lab",
            Self::ExecutionPlan => "Exec Plan",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Source => "Explorer + hotspots",
            Self::QueryLab => "SQL studio + live preview",
            Self::ExecutionPlan => "Plan graph + IO telemetry",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusPanel {
    Input,
    Code,
    Telemetry,
    Info,
    Context,
    Hotspots,
    Radar,
    Spotlight,
}

impl Screen for CodeExplorer {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.focus_from_point(mouse.x, mouse.y);
                    if self.focus == FocusPanel::Hotspots {
                        let area = self.layout_hotspots.get();
                        let rel_y = mouse.y.saturating_sub(area.y);
                        let idx = rel_y as usize;
                        if idx < self.hotspots.len() {
                            self.current_hotspot = idx;
                            let line = self.hotspots[self.current_hotspot].line;
                            self.scroll_to(line.saturating_sub(3));
                        }
                    } else if self.focus == FocusPanel::Radar {
                        let area = self.layout_radar.get();
                        let rel_y = mouse.y.saturating_sub(area.y);
                        let idx = self.current_match.saturating_sub(2) + rel_y as usize;
                        self.set_current_match(idx);
                    }
                }
                MouseEventKind::ScrollUp => {
                    if self.focus == FocusPanel::Code {
                        self.scroll_by(-3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if self.focus == FocusPanel::Code {
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

            // Goto-line mode
            if self.goto_active {
                match (key.code, key.modifiers) {
                    (KeyCode::Escape, _) => {
                        self.goto_active = false;
                        self.goto_input.set_focused(false);
                        return Cmd::None;
                    }
                    (KeyCode::Enter, _) => {
                        self.goto_line();
                        self.goto_active = false;
                        self.goto_input.set_focused(false);
                        return Cmd::None;
                    }
                    _ => {
                        self.goto_input.handle_event(event);
                        return Cmd::None;
                    }
                }
            }

            // Search mode
            if self.search_active {
                match (key.code, key.modifiers) {
                    (KeyCode::Escape, _) => {
                        self.search_active = false;
                        self.search_input.set_focused(false);
                        return Cmd::None;
                    }
                    (KeyCode::Enter, _) | (KeyCode::Down, _) | (KeyCode::Tab, _) => {
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
                    _ => {
                        let handled = self.search_input.handle_event(event);
                        if handled {
                            self.perform_search();
                        }
                        return Cmd::None;
                    }
                }
            }

            // Normal mode
            match (key.code, key.modifiers) {
                (KeyCode::Char('/'), Modifiers::NONE) => {
                    self.search_active = true;
                    self.search_input.set_focused(true);
                    self.focus = FocusPanel::Input;
                }
                (KeyCode::Char('g'), Modifiers::CTRL) => {
                    self.goto_active = true;
                    self.goto_input.set_focused(true);
                    self.goto_input.set_value("");
                }
                (KeyCode::Char('n'), Modifiers::NONE) => self.next_match(),
                (KeyCode::Char('N'), Modifiers::NONE) | (KeyCode::Char('n'), Modifiers::SHIFT) => {
                    self.prev_match();
                }
                (KeyCode::Char('['), Modifiers::NONE) => self.prev_hotspot(),
                (KeyCode::Char(']'), Modifiers::NONE) => self.next_hotspot(),
                (KeyCode::Char('f'), Modifiers::NONE) => {
                    self.feature_index = (self.feature_index + 1) % FEATURE_SPOTLIGHT.len();
                }
                (KeyCode::Char('m'), Modifiers::NONE) => {
                    self.mode = self.mode.next();
                }
                (KeyCode::Char('M'), _) | (KeyCode::Char('m'), Modifiers::SHIFT) => {
                    self.mode = self.mode.prev();
                }
                (KeyCode::Down, _) => match self.focus {
                    FocusPanel::Hotspots => self.next_hotspot(),
                    FocusPanel::Radar => self.next_match(),
                    _ => self.scroll_by(1),
                },
                (KeyCode::Up, _) => match self.focus {
                    FocusPanel::Hotspots => self.prev_hotspot(),
                    FocusPanel::Radar => self.prev_match(),
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
                // Vim: g or Home for top
                (KeyCode::Home, _) | (KeyCode::Char('g'), Modifiers::NONE) => self.scroll_to(0),
                // Vim: G or End for bottom
                (KeyCode::End, _) | (KeyCode::Char('G'), Modifiers::NONE) => {
                    self.scroll_to(self.total_lines())
                }
                (KeyCode::Enter, _) => match self.focus {
                    FocusPanel::Hotspots => {
                        if let Some(hotspot) = self.current_hotspot() {
                            self.scroll_to(hotspot.line.saturating_sub(3));
                        }
                    }
                    FocusPanel::Radar => {
                        if !self.search_matches.is_empty() {
                            let line = self.search_matches[self.current_match];
                            self.scroll_to(line.saturating_sub(3));
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.height < 6 || area.width < 40 {
            Paragraph::new("Terminal too small")
                .style(theme::muted())
                .render(area, frame);
            return;
        }

        // Vertical: search/goto bar (optional) + body + status
        let v_constraints = if self.search_active || self.goto_active {
            vec![
                Constraint::Fixed(3),
                Constraint::Min(4),
                Constraint::Fixed(1),
            ]
        } else {
            vec![Constraint::Min(4), Constraint::Fixed(1)]
        };
        let v_chunks = Flex::vertical().constraints(v_constraints).split(area);

        let (body_area, status_area) = if self.search_active || self.goto_active {
            self.render_input_bar(frame, v_chunks[0]);
            self.layout_input.set(v_chunks[0]);
            (v_chunks[1], v_chunks[2])
        } else {
            self.layout_input.set(Rect::default());
            (v_chunks[0], v_chunks[1])
        };

        // Body: code (72%) + sidebar (28%)
        let h_chunks = Flex::horizontal()
            .constraints([Constraint::Percentage(72.0), Constraint::Percentage(28.0)])
            .split(body_area);

        let (code_area, telemetry_area) = if h_chunks[0].height >= 12 {
            let rows = Flex::vertical()
                .constraints([Constraint::Percentage(72.0), Constraint::Percentage(28.0)])
                .split(h_chunks[0]);
            (rows[0], Some(rows[1]))
        } else {
            (h_chunks[0], None)
        };

        self.layout_code.set(code_area);
        self.render_code_panel(frame, code_area);
        if let Some(telemetry) = telemetry_area {
            self.layout_telemetry.set(telemetry);
            self.render_telemetry_panel(frame, telemetry);
            chrome::register_pane_hit(frame, telemetry, ScreenId::CodeExplorer);
        } else {
            self.layout_telemetry.set(Rect::default());
        }
        match self.mode {
            ExplorerMode::Source => self.render_sidebar_source(frame, h_chunks[1]),
            ExplorerMode::QueryLab => self.render_sidebar_query_lab(frame, h_chunks[1]),
            ExplorerMode::ExecutionPlan => self.render_sidebar_exec_plan(frame, h_chunks[1]),
        }
        self.render_status_bar(frame, status_area);

        chrome::register_pane_hit(frame, code_area, ScreenId::CodeExplorer);
        chrome::register_pane_hit(frame, h_chunks[1], ScreenId::CodeExplorer);
        chrome::register_pane_hit(frame, status_area, ScreenId::CodeExplorer);
        if self.search_active || self.goto_active {
            chrome::register_pane_hit(frame, v_chunks[0], ScreenId::CodeExplorer);
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "/",
                action: "Search",
            },
            HelpEntry {
                key: "Ctrl+G",
                action: "Goto line",
            },
            HelpEntry {
                key: "Enter/Tab/↓",
                action: "Next match (search)",
            },
            HelpEntry {
                key: "↑",
                action: "Prev match (search)",
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
                key: "[/]",
                action: "Jump hotspots",
            },
            HelpEntry {
                key: "f",
                action: "Cycle feature spotlight",
            },
            HelpEntry {
                key: "m/M",
                action: "Cycle mode",
            },
            HelpEntry {
                key: "j/k",
                action: "Scroll",
            },
            HelpEntry {
                key: "g/G",
                action: "Top/bottom",
            },
            HelpEntry {
                key: "Ctrl+D/U",
                action: "Page scroll",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click pane to focus",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Code Explorer"
    }

    fn tab_label(&self) -> &'static str {
        "Code"
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.time = tick_count as f64 * 0.1;
        if tick_count.is_multiple_of(40) {
            self.feature_index = (self.feature_index + 1) % FEATURE_SPOTLIGHT.len();
        }
    }
}

impl CodeExplorer {
    fn render_input_bar(&self, frame: &mut Frame, area: Rect) {
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

        // Header row: animated title + context
        let header_cols = Flex::horizontal()
            .constraints([Constraint::Min(12), Constraint::Fixed(28)])
            .split(rows[0]);
        let title = StyledText::new("SQLITE CODE EXPLORER")
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::cyberpunk(),
                speed: 0.5,
            })
            .effect(TextEffect::PulsingGlow {
                color: PackedRgba::rgb(120, 220, 255),
                speed: 1.9,
            })
            .bold()
            .time(self.time);
        title.render(header_cols[0], frame);

        let ctx_hint = truncate_to_width(
            &format!("mode: {} · hotspots: [ ] · feature: f", self.mode.label()),
            header_cols[1].width,
        );
        let ctx = StyledText::new(ctx_hint)
            .effect(TextEffect::ColorWave {
                color1: theme::accent::PRIMARY.into(),
                color2: theme::accent::ACCENT_8.into(),
                speed: 1.1,
                wavelength: 8.0,
            })
            .time(self.time);
        ctx.render(header_cols[1], frame);

        // Input row
        let input_cols = Flex::horizontal()
            .constraints([
                Constraint::Fixed(10),
                Constraint::Min(10),
                Constraint::Fixed(20),
            ])
            .split(rows[1]);

        if self.goto_active {
            let label = StyledText::new("Goto")
                .effect(TextEffect::Pulse {
                    speed: 1.2,
                    min_alpha: 0.35,
                })
                .bold()
                .time(self.time);
            label.render(input_cols[0], frame);
            self.goto_input.render(input_cols[1], frame);
        } else {
            let label = StyledText::new("Search")
                .effect(TextEffect::Pulse {
                    speed: 1.4,
                    min_alpha: 0.35,
                })
                .bold()
                .time(self.time);
            label.render(input_cols[0], frame);
            self.search_input.render(input_cols[1], frame);
        }

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
                intensity: 0.4,
            })
            .time(self.time);
        match_fx.render(input_cols[2], frame);

        // Footer row
        let footer_cols = Flex::horizontal()
            .constraints([Constraint::Min(10), Constraint::Fixed(28)])
            .split(rows[2]);
        let mode = if self.goto_active {
            "Goto line mode"
        } else if self.search_active {
            "Live search mode"
        } else {
            self.mode.subtitle()
        };
        Paragraph::new(format!("Mode: {mode}"))
            .style(theme::muted())
            .render(footer_cols[0], frame);

        let nav = truncate_to_width("M switches mode · Enter/Tab next", footer_cols[1].width);
        Paragraph::new(nav)
            .style(theme::muted())
            .render(footer_cols[1], frame);
    }

    fn render_code_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .title("sqlite3.c")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Code,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 || inner.width < 10 {
            return;
        }

        self.viewport_height.set(inner.height);

        let text_width = inner.width.saturating_sub(1);
        let text_area = Rect::new(inner.x, inner.y, text_width, inner.height);
        let scrollbar_area = Rect::new(inner.x + text_width, inner.y, 1, inner.height);
        let vh = inner.height as usize;

        let query = self.search_input.value();
        let has_query = query.len() >= 2;

        // Render visible lines with syntax highlighting and match effects
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

            // Line number + marker
            let num_width = 8u16.min(text_area.width);
            let content_width = text_area.width.saturating_sub(num_width);
            let marker_area = Rect::new(line_area.x, line_area.y, 1.min(num_width), 1);
            let num_area = Rect::new(
                line_area.x.saturating_add(1),
                line_area.y,
                num_width.saturating_sub(1),
                1,
            );
            let content_area = Rect::new(
                line_area.x.saturating_add(num_width),
                line_area.y,
                content_width,
                1,
            );

            // Determine if this is a search match
            let matches = if has_query {
                search_ascii_case_insensitive(line, query)
            } else {
                Vec::new()
            };
            let is_current_match = has_query
                && !self.search_matches.is_empty()
                && self.search_matches.get(self.current_match) == Some(&line_idx);
            let is_any_match = !matches.is_empty();

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

            if !is_any_match || query.is_empty() {
                // Syntax-highlighted line
                let highlighted = self.highlighter.highlight(line, "c");
                Paragraph::new(highlighted).render(content_area, frame);
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
                            speed: 2.0,
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
                    Paragraph::new(clipped)
                        .style(
                            Style::new()
                                .fg(theme::fg::PRIMARY)
                                .bg(theme::alpha::HIGHLIGHT)
                                .attrs(StyleFlags::UNDERLINE),
                        )
                        .render(Rect::new(cursor_x, line_y, width.min(remaining), 1), frame);
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

    fn render_telemetry_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 3 || area.width < 12 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Telemetry Deck")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Telemetry,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let cols = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([
                Constraint::Percentage(34.0),
                Constraint::Percentage(33.0),
                Constraint::Percentage(33.0),
            ])
            .split(inner);

        let series_len = inner.width.max(8) as usize;
        let io_series = self.synthetic_series(series_len, 45.0, 20.0, 0.35, 0.0);
        let cache_series = self.synthetic_series(series_len, 70.0, 15.0, 0.25, 1.1);

        for (idx, col) in cols.iter().enumerate() {
            if col.is_empty() || col.height < 2 {
                continue;
            }
            let rows = Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Min(1)])
                .split(*col);
            let label = match idx {
                0 => "IO Lane",
                1 => "Cache Hit",
                _ => "Lock + TX",
            };
            Paragraph::new(truncate_to_width(label, rows[0].width))
                .style(theme::muted())
                .render(rows[0], frame);

            if rows[1].is_empty() {
                continue;
            }

            match idx {
                0 => {
                    Sparkline::new(&io_series)
                        .style(Style::new().fg(theme::accent::PRIMARY))
                        .gradient(
                            theme::accent::PRIMARY.into(),
                            theme::accent::ACCENT_7.into(),
                        )
                        .render(rows[1], frame);
                }
                1 => {
                    Sparkline::new(&cache_series)
                        .style(Style::new().fg(theme::accent::SUCCESS))
                        .gradient(
                            theme::accent::SUCCESS.into(),
                            theme::accent::ACCENT_9.into(),
                        )
                        .render(rows[1], frame);
                }
                _ => {
                    let sub_rows = Flex::vertical()
                        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
                        .split(rows[1]);
                    let colors = MiniBarColors::new(
                        theme::accent::PRIMARY.into(),
                        theme::accent::SUCCESS.into(),
                        theme::accent::WARNING.into(),
                        theme::accent::ACCENT_10.into(),
                    );
                    let lock = (0.2 + (self.time * 0.8).sin() * 0.1).clamp(0.0, 1.0);
                    let tx = (0.6 + (self.time * 0.5).cos() * 0.2).clamp(0.0, 1.0);
                    let bar_width = sub_rows[0].width.saturating_sub(6);
                    if bar_width > 0 {
                        Paragraph::new("LCK").style(theme::muted()).render(
                            Rect::new(sub_rows[0].x, sub_rows[0].y, 3, sub_rows[0].height),
                            frame,
                        );
                        MiniBar::new(lock, bar_width).colors(colors).render(
                            Rect::new(
                                sub_rows[0].x + 3,
                                sub_rows[0].y,
                                bar_width,
                                sub_rows[0].height,
                            ),
                            frame,
                        );

                        Paragraph::new("TX").style(theme::muted()).render(
                            Rect::new(sub_rows[1].x, sub_rows[1].y, 3, sub_rows[1].height),
                            frame,
                        );
                        MiniBar::new(tx, bar_width).colors(colors).render(
                            Rect::new(
                                sub_rows[1].x + 3,
                                sub_rows[1].y,
                                bar_width,
                                sub_rows[1].height,
                            ),
                            frame,
                        );
                    }
                }
            }
        }
    }

    fn render_sidebar_source(&self, frame: &mut Frame, area: Rect) {
        self.reset_sidebar_layouts();
        let rows = if area.height >= 28 {
            Flex::vertical()
                .constraints([
                    Constraint::Percentage(20.0),
                    Constraint::Percentage(20.0),
                    Constraint::Percentage(20.0),
                    Constraint::Percentage(20.0),
                    Constraint::Percentage(20.0),
                ])
                .split(area)
        } else if area.height >= 22 {
            Flex::vertical()
                .constraints([
                    Constraint::Percentage(25.0),
                    Constraint::Percentage(25.0),
                    Constraint::Percentage(20.0),
                    Constraint::Percentage(30.0),
                ])
                .split(area)
        } else {
            Flex::vertical()
                .constraints([
                    Constraint::Percentage(35.0),
                    Constraint::Percentage(30.0),
                    Constraint::Percentage(35.0),
                ])
                .split(area)
        };

        // Panel 1: JSON metadata
        self.layout_info.set(rows[0]);
        chrome::register_pane_hit(frame, rows[0], ScreenId::CodeExplorer);
        let json_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("File Info")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Info,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let json_inner = json_block.inner(rows[0]);
        json_block.render(rows[0], frame);
        let json_view = JsonView::new(&self.metadata_json)
            .with_indent(2)
            .with_key_style(Style::new().fg(theme::accent::PRIMARY))
            .with_string_style(Style::new().fg(theme::accent::SUCCESS))
            .with_number_style(Style::new().fg(theme::accent::WARNING))
            .with_punct_style(Style::new().fg(theme::fg::MUTED));
        json_view.render(json_inner, frame);

        // Panel 2: Context
        self.layout_context.set(rows[1]);
        chrome::register_pane_hit(frame, rows[1], ScreenId::CodeExplorer);
        let ctx_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Context")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Context,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let ctx_inner = ctx_block.inner(rows[1]);
        ctx_block.render(rows[1], frame);

        let context = truncate_to_width(self.current_context(), ctx_inner.width);
        let ctx_text = format!(
            "Line {}\n\nNearest function:\n{}",
            self.scroll_offset + 1,
            context,
        );
        Paragraph::new(ctx_text)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(ctx_inner, frame);

        if rows.len() == 5 {
            // Panel 3: Hotspots
            self.layout_hotspots.set(rows[2]);
            chrome::register_pane_hit(frame, rows[2], ScreenId::CodeExplorer);
            self.render_hotspot_panel(frame, rows[2]);

            // Panel 4: Match radar
            self.layout_radar.set(rows[3]);
            chrome::register_pane_hit(frame, rows[3], ScreenId::CodeExplorer);
            let radar_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Match Radar")
                .title_alignment(Alignment::Center)
                .style(theme::panel_border_style(
                    self.focus == FocusPanel::Radar,
                    theme::screen_accent::CODE_EXPLORER,
                ));
            let radar_inner = radar_block.inner(rows[3]);
            radar_block.render(rows[3], frame);
            self.render_match_radar(frame, radar_inner);

            // Panel 5: Feature spotlight
            self.layout_spotlight.set(rows[4]);
            chrome::register_pane_hit(frame, rows[4], ScreenId::CodeExplorer);
            let feature_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("FrankenTUI Spotlight")
                .title_alignment(Alignment::Center)
                .style(theme::panel_border_style(
                    self.focus == FocusPanel::Spotlight,
                    theme::screen_accent::CODE_EXPLORER,
                ));
            let feature_inner = feature_block.inner(rows[4]);
            feature_block.render(rows[4], frame);
            self.render_feature_spotlight(frame, feature_inner);
        } else if rows.len() == 4 {
            // Panel 3: Hotspots
            self.layout_hotspots.set(rows[2]);
            chrome::register_pane_hit(frame, rows[2], ScreenId::CodeExplorer);
            self.render_hotspot_panel(frame, rows[2]);

            // Panel 4: combined radar + spotlight
            self.layout_radar.set(rows[3]);
            self.layout_spotlight.set(rows[3]);
            chrome::register_pane_hit(frame, rows[3], ScreenId::CodeExplorer);
            let combo_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Radar + Spotlight")
                .title_alignment(Alignment::Center)
                .style(theme::panel_border_style(
                    self.focus == FocusPanel::Radar || self.focus == FocusPanel::Spotlight,
                    theme::screen_accent::CODE_EXPLORER,
                ));
            let combo_inner = combo_block.inner(rows[3]);
            combo_block.render(rows[3], frame);
            let combo_rows = Flex::vertical()
                .constraints([Constraint::Percentage(45.0), Constraint::Percentage(55.0)])
                .split(combo_inner);
            self.render_match_radar(frame, combo_rows[0]);
            self.render_feature_spotlight(frame, combo_rows[1]);
        } else {
            // Panel 3: combo hotspot + radar + spotlight
            self.layout_hotspots.set(rows[2]);
            self.layout_radar.set(rows[2]);
            self.layout_spotlight.set(rows[2]);
            chrome::register_pane_hit(frame, rows[2], ScreenId::CodeExplorer);
            let combo_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Hotspots + Radar + Spotlight")
                .title_alignment(Alignment::Center)
                .style(theme::panel_border_style(
                    matches!(
                        self.focus,
                        FocusPanel::Hotspots | FocusPanel::Radar | FocusPanel::Spotlight
                    ),
                    theme::screen_accent::CODE_EXPLORER,
                ));
            let combo_inner = combo_block.inner(rows[2]);
            combo_block.render(rows[2], frame);
            let combo_rows = Flex::vertical()
                .constraints([
                    Constraint::Percentage(35.0),
                    Constraint::Percentage(30.0),
                    Constraint::Percentage(35.0),
                ])
                .split(combo_inner);
            self.render_hotspot_panel(frame, combo_rows[0]);
            self.render_match_radar(frame, combo_rows[1]);
            self.render_feature_spotlight(frame, combo_rows[2]);
        }
    }

    fn render_sidebar_query_lab(&self, frame: &mut Frame, area: Rect) {
        self.reset_sidebar_layouts();
        let rows = if area.height >= 28 {
            Flex::vertical()
                .constraints([
                    Constraint::Percentage(35.0),
                    Constraint::Percentage(25.0),
                    Constraint::Percentage(20.0),
                    Constraint::Percentage(20.0),
                ])
                .split(area)
        } else {
            Flex::vertical()
                .constraints([
                    Constraint::Percentage(40.0),
                    Constraint::Percentage(30.0),
                    Constraint::Percentage(30.0),
                ])
                .split(area)
        };

        // Panel 1: SQL Studio
        self.layout_info.set(rows[0]);
        chrome::register_pane_hit(frame, rows[0], ScreenId::CodeExplorer);
        let studio_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("SQL Studio")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Info,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let studio_inner = studio_block.inner(rows[0]);
        studio_block.render(rows[0], frame);
        let query = QUERY_SNIPPETS[self.query_index()];
        let max_lines = studio_inner.height as usize;
        let mut lines = Vec::new();
        for line in query.lines().take(max_lines) {
            lines.push(truncate_to_width(line, studio_inner.width));
        }
        if !lines.is_empty() {
            let preview = lines.join("\n");
            let total_chars = grapheme_count(&preview).max(1);
            let progress = ((self.time * 0.6).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
            let visible_chars = (total_chars as f64 * progress) as usize;
            let typed = truncate_to_grapheme_count(&preview, visible_chars);
            let highlighted = self.highlighter.highlight(&typed, "sql");
            Paragraph::new(highlighted).render(studio_inner, frame);
        }

        // Panel 2: Result Preview
        self.layout_context.set(rows[1]);
        chrome::register_pane_hit(frame, rows[1], ScreenId::CodeExplorer);
        let preview_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Result Preview")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Context,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let preview_inner = preview_block.inner(rows[1]);
        preview_block.render(rows[1], frame);
        self.render_query_results_panel(frame, preview_inner);

        if rows.len() == 4 {
            // Panel 3: Schema Inspector
            self.layout_hotspots.set(rows[2]);
            chrome::register_pane_hit(frame, rows[2], ScreenId::CodeExplorer);
            let schema_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Schema")
                .title_alignment(Alignment::Center)
                .style(theme::panel_border_style(
                    self.focus == FocusPanel::Hotspots,
                    theme::screen_accent::CODE_EXPLORER,
                ));
            let schema_inner = schema_block.inner(rows[2]);
            schema_block.render(rows[2], frame);
            self.render_schema_panel(frame, schema_inner);

            // Panel 4: Index Map
            self.layout_radar.set(rows[3]);
            chrome::register_pane_hit(frame, rows[3], ScreenId::CodeExplorer);
            let map_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Index Map")
                .title_alignment(Alignment::Center)
                .style(theme::panel_border_style(
                    self.focus == FocusPanel::Radar,
                    theme::screen_accent::CODE_EXPLORER,
                ));
            let map_inner = map_block.inner(rows[3]);
            map_block.render(rows[3], frame);
            self.render_index_map(frame, map_inner);
        } else {
            // Panel 3: Index Map
            self.layout_radar.set(rows[2]);
            chrome::register_pane_hit(frame, rows[2], ScreenId::CodeExplorer);
            let map_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Index Map")
                .title_alignment(Alignment::Center)
                .style(theme::panel_border_style(
                    self.focus == FocusPanel::Radar,
                    theme::screen_accent::CODE_EXPLORER,
                ));
            let map_inner = map_block.inner(rows[2]);
            map_block.render(rows[2], frame);
            self.render_index_map(frame, map_inner);
        }
    }

    fn render_sidebar_exec_plan(&self, frame: &mut Frame, area: Rect) {
        self.reset_sidebar_layouts();
        let rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(40.0),
                Constraint::Percentage(30.0),
                Constraint::Percentage(30.0),
            ])
            .split(area);

        // Panel 1: Plan graph
        self.layout_info.set(rows[0]);
        chrome::register_pane_hit(frame, rows[0], ScreenId::CodeExplorer);
        let plan_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Query Plan Graph")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Info,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let plan_inner = plan_block.inner(rows[0]);
        plan_block.render(rows[0], frame);
        let max_lines = plan_inner.height as usize;
        let mut plan_lines = Vec::new();
        for line in PLAN_GRAPH.iter().take(max_lines) {
            plan_lines.push(truncate_to_width(line, plan_inner.width));
        }
        if !plan_lines.is_empty() {
            let fx = StyledMultiLine::new(plan_lines)
                .effect(TextEffect::AnimatedGradient {
                    gradient: ColorGradient::ice(),
                    speed: 0.4,
                })
                .effect(TextEffect::Scanline {
                    intensity: 0.15,
                    line_gap: 2,
                    scroll: true,
                    scroll_speed: 0.45,
                    flicker: 0.04,
                })
                .base_color(theme::fg::PRIMARY.into())
                .time(self.time);
            fx.render(plan_inner, frame);
        }

        // Panel 2: IO + Cache telemetry
        self.layout_context.set(rows[1]);
        chrome::register_pane_hit(frame, rows[1], ScreenId::CodeExplorer);
        let io_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("IO + Cache Telemetry")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Context,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let io_inner = io_block.inner(rows[1]);
        io_block.render(rows[1], frame);
        let io_lines = [
            format!("reads: {:>4}/s", 320 + (self.time.sin() * 40.0) as i32),
            format!("writes:{:>4}/s", 90 + (self.time.cos() * 20.0) as i32),
        ];
        for (i, line) in io_lines.iter().enumerate() {
            let y = io_inner.y + i as u16;
            if y >= io_inner.y + io_inner.height {
                break;
            }
            Paragraph::new(truncate_to_width(line, io_inner.width))
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(Rect::new(io_inner.x, y, io_inner.width, 1), frame);
        }

        // Panel 3: Hot path
        self.layout_hotspots.set(rows[2]);
        chrome::register_pane_hit(frame, rows[2], ScreenId::CodeExplorer);
        let hot_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Hot Path")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Hotspots,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let hot_inner = hot_block.inner(rows[2]);
        hot_block.render(rows[2], frame);
        let hot_lines = [
            "pager.c: fetchPage()",
            "btree.c: btreeNext()",
            "vdbe.c: sqlite3VdbeExec()",
        ];
        for (i, line) in hot_lines.iter().enumerate() {
            let y = hot_inner.y + i as u16;
            if y >= hot_inner.y + hot_inner.height {
                break;
            }
            let fx = StyledText::new(truncate_to_width(line, hot_inner.width))
                .effect(TextEffect::PulsingGlow {
                    color: theme::accent::WARNING.into(),
                    speed: 1.6,
                })
                .time(self.time);
            fx.render(Rect::new(hot_inner.x, y, hot_inner.width, 1), frame);
        }
    }

    fn render_match_radar(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let rows = if area.height >= 3 {
            Flex::vertical()
                .constraints([
                    Constraint::Fixed(1),
                    Constraint::Fixed(1),
                    Constraint::Min(1),
                ])
                .split(area)
        } else {
            Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Min(1)])
                .split(area)
        };
        let hint = if self.search_matches.is_empty() {
            "Type / for instant highlights".to_string()
        } else {
            format!(
                "{}/{} matches",
                self.current_match + 1,
                self.search_matches.len()
            )
        };
        Paragraph::new(truncate_to_width(&hint, rows[0].width))
            .style(theme::muted())
            .render(rows[0], frame);

        if rows.len() > 2 && !rows[1].is_empty() && !self.match_density.is_empty() {
            Sparkline::new(&self.match_density)
                .style(Style::new().fg(theme::accent::PRIMARY))
                .gradient(
                    theme::accent::PRIMARY.into(),
                    theme::accent::ACCENT_8.into(),
                )
                .render(rows[1], frame);
        }

        let list_area = if rows.len() > 2 { rows[2] } else { rows[1] };
        if !list_area.is_empty() {
            let mut lines = Vec::new();
            if self.search_matches.is_empty() {
                lines.push("Awaiting query...".to_owned());
            } else {
                let visible = list_area.height as usize;
                let mut start = self.current_match.saturating_sub(visible / 2);
                if start + visible > self.search_matches.len() {
                    start = self.search_matches.len().saturating_sub(visible);
                }
                let end = (start + visible).min(self.search_matches.len());
                for (i, idx) in self.search_matches[start..end].iter().enumerate() {
                    let marker = if start + i == self.current_match {
                        "▶"
                    } else {
                        "•"
                    };
                    lines.push(format!("{marker} line {}", idx + 1));
                }
            }
            for (i, line) in lines.iter().enumerate() {
                if i as u16 >= list_area.height {
                    break;
                }
                let y = list_area.y + i as u16;
                let line_area = Rect::new(list_area.x, y, list_area.width, 1);
                Paragraph::new(truncate_to_width(line, list_area.width))
                    .style(Style::new().fg(theme::fg::SECONDARY))
                    .render(line_area, frame);
            }
        }
    }

    fn render_hotspot_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Hotspots")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.focus == FocusPanel::Hotspots,
                theme::screen_accent::CODE_EXPLORER,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let list_height = inner.height.saturating_sub(1).max(1);
        let visible = list_height as usize;
        let mut start = self.current_hotspot.saturating_sub(visible / 2);
        if start + visible > self.hotspots.len() {
            start = self.hotspots.len().saturating_sub(visible);
        }
        let end = (start + visible).min(self.hotspots.len());
        let is_focused = self.focus == FocusPanel::Hotspots;

        for (i, hotspot) in self.hotspots[start..end].iter().enumerate() {
            let y = inner.y + i as u16;
            if y >= inner.y + list_height {
                break;
            }
            let row_area = Rect::new(inner.x, y, inner.width, 1);
            let label = truncate_to_width(
                &format!("{:>6}  {}", hotspot.line + 1, hotspot.label),
                inner.width.saturating_sub(2),
            );
            if start + i == self.current_hotspot {
                if is_focused {
                    StyledText::new(format!("▶ {label}"))
                        .effect(TextEffect::PulsingGlow {
                            color: theme::accent::PRIMARY.into(),
                            speed: 1.6,
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
                Paragraph::new(format!("  {label}"))
                    .style(Style::new().fg(theme::fg::SECONDARY))
                    .render(row_area, frame);
            }
        }

        if !inner.is_empty() && !self.match_density.is_empty() {
            let spark_area = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
            Sparkline::new(&self.match_density)
                .style(Style::new().fg(theme::accent::PRIMARY))
                .gradient(
                    theme::accent::PRIMARY.into(),
                    theme::accent::ACCENT_8.into(),
                )
                .render(spark_area, frame);
        }
    }

    fn render_query_results_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Min(1),
            ])
            .split(area);

        let preview_idx = self
            .query_index()
            .min(RESULT_PREVIEWS.len().saturating_sub(1));
        let preview = RESULT_PREVIEWS
            .get(preview_idx)
            .copied()
            .unwrap_or("rows=0");
        let preview_line = truncate_to_width(preview, rows[0].width);
        let preview_fx = StyledText::new(preview_line)
            .effect(TextEffect::ColorWave {
                color1: theme::accent::SUCCESS.into(),
                color2: theme::accent::ACCENT_8.into(),
                speed: 1.1,
                wavelength: 12.0,
            })
            .time(self.time);
        preview_fx.render(rows[0], frame);

        if rows[1].is_empty() {
            return;
        }

        let total_width = rows[1].width;
        if total_width < 8 {
            return;
        }
        let col_play = (total_width.saturating_mul(20))
            .checked_div(100)
            .unwrap_or(6)
            .max(6);
        let col_speaker = (total_width.saturating_mul(20))
            .checked_div(100)
            .unwrap_or(8)
            .max(8);
        let col_line = total_width
            .saturating_sub(col_play)
            .saturating_sub(col_speaker)
            .saturating_sub(2);

        let header = format!(
            "{} {} {}",
            pad_to_width("play", col_play),
            pad_to_width("speaker", col_speaker),
            truncate_to_width("line", col_line),
        );
        Paragraph::new(truncate_to_width(&header, rows[1].width))
            .style(Style::new().fg(theme::accent::PRIMARY))
            .render(rows[1], frame);

        if rows[2].is_empty() {
            return;
        }

        let results_idx = self.query_index().min(RESULT_SETS.len().saturating_sub(1));
        let results = RESULT_SETS.get(results_idx).copied().unwrap_or(&[]);
        let max_rows = rows[2].height as usize;
        for (i, row) in results.iter().take(max_rows).enumerate() {
            let y = rows[2].y + i as u16;
            if y >= rows[2].y + rows[2].height {
                break;
            }
            let line = format!(
                "{} {} {}",
                pad_to_width(row.play, col_play),
                pad_to_width(row.speaker, col_speaker),
                truncate_to_width(row.line, col_line),
            );
            Paragraph::new(truncate_to_width(&line, rows[2].width))
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(Rect::new(rows[2].x, y, rows[2].width, 1), frame);
        }
    }

    fn render_schema_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let max_lines = area.height as usize;
        for (i, line) in SCHEMA_PREVIEW.iter().take(max_lines).enumerate() {
            let y = area.y + i as u16;
            let text = truncate_to_width(line, area.width);
            let fx = StyledText::new(text)
                .effect(TextEffect::Pulse {
                    speed: 0.7,
                    min_alpha: 0.7,
                })
                .time(self.time)
                .seed(i as u64);
            fx.render(Rect::new(area.x, y, area.width, 1), frame);
        }
    }

    fn render_index_map(&self, frame: &mut Frame, area: Rect) {
        let map = if self.match_density.is_empty() {
            vec![0.2, 0.4, 0.6, 0.8, 0.5, 0.3, 0.7, 0.9, 0.4, 0.2]
        } else {
            self.match_density.clone()
        };
        Sparkline::new(&map)
            .style(Style::new().fg(theme::accent::PRIMARY))
            .gradient(
                theme::accent::PRIMARY.into(),
                theme::accent::ACCENT_8.into(),
            )
            .render(area, frame);
    }

    fn render_feature_spotlight(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let feature = FEATURE_SPOTLIGHT[self.feature_index % FEATURE_SPOTLIGHT.len()];
        let text = truncate_to_width(feature, area.width);
        let styled = StyledText::new(text)
            .effect(TextEffect::AnimatedGradient {
                gradient: ColorGradient::ocean(),
                speed: 0.4,
            })
            .effect(TextEffect::PulsingGlow {
                color: PackedRgba::rgb(120, 200, 255),
                speed: 1.6,
            })
            .effect(TextEffect::Scanline {
                intensity: 0.2,
                line_gap: 2,
                scroll: true,
                scroll_speed: 0.6,
                flicker: 0.05,
            })
            .time(self.time);
        styled.render(area, frame);
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        frame.buffer.fill(
            area,
            RenderCell::default().with_bg(theme::alpha::SURFACE.into()),
        );

        let total = self.total_lines();
        let pos = self.scroll_offset + 1;
        let pct = (self.scroll_offset * 100).checked_div(total).unwrap_or(0);
        let size = filesize::decimal(SQLITE_SOURCE.len() as u64);

        let status = format!(
            " Mode: {} · Line {pos}/{total} ({pct}%) · Matches: {} | {size} | C",
            self.mode.label(),
            self.search_matches.len()
        );
        let status_line = truncate_to_width(&status, area.width);
        if self.search_matches.is_empty() {
            Paragraph::new(status_line)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(area, frame);
        } else {
            StyledText::new(status_line)
                .base_color(theme::fg::PRIMARY.into())
                .effect(TextEffect::AnimatedGradient {
                    gradient: ColorGradient::sunset(),
                    speed: 0.5,
                })
                .effect(TextEffect::Pulse {
                    speed: 1.2,
                    min_alpha: 0.6,
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
    let max = max_width as usize;
    graphemes(text)
        .scan(0usize, |width, grapheme| {
            let w = grapheme_width(grapheme);
            if *width + w > max {
                return None;
            }
            *width += w;
            Some(grapheme)
        })
        .for_each(|grapheme| out.push_str(grapheme));
    out
}

fn pad_to_width(text: &str, width: u16) -> String {
    let mut out = truncate_to_width(text, width);
    let current = display_width(out.as_str()) as u16;
    if current < width {
        out.push_str(&" ".repeat((width - current) as usize));
    }
    out
}

fn truncate_to_grapheme_count(text: &str, max_count: usize) -> String {
    if max_count == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (idx, grapheme) in graphemes(text).enumerate() {
        if idx >= max_count {
            break;
        }
        out.push_str(grapheme);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_explorer_initial_render() {
        let ce = CodeExplorer::new();
        assert_eq!(ce.scroll_offset, 0);
        assert!(
            ce.total_lines() > 100_000,
            "sqlite3.c should have 100K+ lines"
        );
        // First line should contain a comment
        assert!(
            ce.lines[0].contains("/*") || ce.lines[0].contains("/***"),
            "First line: {}",
            ce.lines[0]
        );
    }

    #[test]
    fn code_explorer_goto_line() {
        let mut ce = CodeExplorer::new();
        ce.viewport_height.set(40);
        ce.goto_input.set_value("1000");
        ce.goto_line();
        assert_eq!(ce.scroll_offset, 999);
    }

    #[test]
    fn code_explorer_search() {
        let mut ce = CodeExplorer::new();
        ce.search_input.set_value("sqlite3_open");
        ce.perform_search();
        assert!(
            !ce.search_matches.is_empty(),
            "Should find 'sqlite3_open' in sqlite3.c"
        );
    }

    #[test]
    fn code_explorer_json_metadata() {
        let ce = CodeExplorer::new();
        assert!(ce.metadata_json.contains("\"filename\": \"sqlite3.c\""));
        assert!(ce.metadata_json.contains("\"language\": \"C\""));
        assert!(ce.metadata_json.contains("\"lines\":"));
    }

    #[test]
    fn code_explorer_line_numbers() {
        let ce = CodeExplorer::new();
        // Verify line count matches actual file
        let actual_lines = SQLITE_SOURCE.lines().count();
        assert_eq!(ce.total_lines(), actual_lines);
    }

    #[test]
    fn truncate_grapheme_count_handles_unicode() {
        let text = "a😀b";
        assert_eq!(truncate_to_grapheme_count(text, 0), "");
        assert_eq!(truncate_to_grapheme_count(text, 1), "a");
        assert_eq!(truncate_to_grapheme_count(text, 2), "a😀");
        assert_eq!(truncate_to_grapheme_count(text, 3), "a😀b");
    }

    #[test]
    fn code_explorer_result_sets_populated() {
        assert!(!RESULT_SETS.is_empty());
        for set in RESULT_SETS {
            assert!(!set.is_empty());
            for row in *set {
                assert!(!row.play.is_empty());
                assert!(!row.speaker.is_empty());
                assert!(!row.line.is_empty());
            }
        }
    }

    #[test]
    fn code_explorer_schema_preview_populated() {
        assert!(!SCHEMA_PREVIEW.is_empty());
        for line in SCHEMA_PREVIEW {
            assert!(!line.is_empty());
        }
    }
}
