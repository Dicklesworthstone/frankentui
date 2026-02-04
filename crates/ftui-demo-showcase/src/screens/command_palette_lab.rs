#![forbid(unsafe_code)]

//! Command Palette Evidence Lab — explainable ranking + evidence ledger.
//!
//! Demonstrates:
//! - Command palette evidence ledger (Bayesian scoring)
//! - Match-mode filtering (exact/prefix/substring/fuzzy)
//! - Deterministic micro-bench (queries/sec)
//! - HintRanker evidence ledger for keybinding hints

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::WrapMode;
use ftui_text::text::{Line, Span, Text};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::command_palette::{ActionItem, CommandPalette, MatchFilter};
use ftui_widgets::hint_ranker::{HintContext, HintRanker, RankerConfig, RankingEvidence};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

const BENCH_QUERIES: &[&str] = &[
    "open", "theme", "perf", "markdown", "log", "palette", "inline", "help",
];
const BENCH_STEP_TICKS: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterMode {
    All,
    Exact,
    Prefix,
    WordStart,
    Substring,
    Fuzzy,
}

impl FilterMode {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Exact,
            Self::Exact => Self::Prefix,
            Self::Prefix => Self::WordStart,
            Self::WordStart => Self::Substring,
            Self::Substring => Self::Fuzzy,
            Self::Fuzzy => Self::All,
        }
    }

    fn to_match_filter(self) -> MatchFilter {
        match self {
            Self::All => MatchFilter::All,
            Self::Exact => MatchFilter::Exact,
            Self::Prefix => MatchFilter::Prefix,
            Self::WordStart => MatchFilter::WordStart,
            Self::Substring => MatchFilter::Substring,
            Self::Fuzzy => MatchFilter::Fuzzy,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Exact => "Exact",
            Self::Prefix => "Prefix",
            Self::WordStart => "WordStart",
            Self::Substring => "Substring",
            Self::Fuzzy => "Fuzzy",
        }
    }
}

#[derive(Debug, Clone)]
struct BenchState {
    enabled: bool,
    start_tick: u64,
    last_step_tick: u64,
    processed: u64,
    query_index: usize,
}

impl BenchState {
    fn new() -> Self {
        Self {
            enabled: false,
            start_tick: 0,
            last_step_tick: 0,
            processed: 0,
            query_index: 0,
        }
    }

    fn reset(&mut self, tick_count: u64) {
        self.enabled = true;
        self.start_tick = tick_count;
        self.last_step_tick = 0;
        self.processed = 0;
        self.query_index = 0;
    }
}

pub struct CommandPaletteEvidenceLab {
    palette: CommandPalette,
    filter_mode: FilterMode,
    bench: BenchState,
    hint_ranker: HintRanker,
    hint_ledger: Vec<RankingEvidence>,
    tick_count: u64,
}

impl Default for CommandPaletteEvidenceLab {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPaletteEvidenceLab {
    pub fn new() -> Self {
        let mut palette = CommandPalette::new().with_max_visible(8);
        palette.enable_evidence_tracking(true);
        palette.replace_actions(sample_actions());
        palette.open();
        palette.set_query("log");

        let mut hint_ranker = build_hint_ranker();
        let (_, hint_ledger) = hint_ranker.rank(None);

        let mut lab = Self {
            palette,
            filter_mode: FilterMode::All,
            bench: BenchState::new(),
            hint_ranker,
            hint_ledger,
            tick_count: 0,
        };

        lab.apply_filter();
        lab
    }

    fn apply_filter(&mut self) {
        self.palette
            .set_match_filter(self.filter_mode.to_match_filter());
    }

    fn toggle_bench(&mut self) {
        if self.bench.enabled {
            self.bench.enabled = false;
        } else {
            self.bench.reset(self.tick_count);
            let query = BENCH_QUERIES[self.bench.query_index];
            self.palette.set_query(query);
        }
    }

    fn bench_qps(&self) -> f64 {
        if !self.bench.enabled {
            return 0.0;
        }
        let elapsed_ticks = self.tick_count.saturating_sub(self.bench.start_tick);
        let elapsed_secs = elapsed_ticks as f64 * 0.1;
        if elapsed_secs <= 0.0 {
            0.0
        } else {
            self.bench.processed as f64 / elapsed_secs
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let accent = Style::new().fg(theme::screen_accent::ADVANCED).bold();
        let muted = theme::muted();
        let mut spans = Vec::new();
        spans.push(Span::styled("Match Mode: ", muted));

        let modes = [
            (FilterMode::All, "0 All"),
            (FilterMode::Exact, "1 Exact"),
            (FilterMode::Prefix, "2 Prefix"),
            (FilterMode::WordStart, "3 WordStart"),
            (FilterMode::Substring, "4 Substring"),
            (FilterMode::Fuzzy, "5 Fuzzy"),
        ];

        for (idx, (mode, label)) in modes.iter().enumerate() {
            if idx > 0 {
                spans.push(Span::raw("  "));
            }
            let style = if *mode == self.filter_mode {
                accent
            } else {
                Style::new().fg(theme::fg::SECONDARY)
            };
            spans.push(Span::styled(*label, style));
        }

        let line1 = Line::from_spans(spans);
        let line2 = Line::from_spans([
            Span::styled("Type to filter · ", muted),
            Span::styled("↑/↓", Style::new().fg(theme::accent::INFO).bold()),
            Span::styled(" navigate · ", muted),
            Span::styled("Enter", Style::new().fg(theme::accent::SUCCESS).bold()),
            Span::styled(" execute · ", muted),
            Span::styled("b", Style::new().fg(theme::accent::WARNING).bold()),
            Span::styled(" bench · ", muted),
            Span::styled("m", Style::new().fg(theme::accent::PRIMARY).bold()),
            Span::styled(" cycle", muted),
        ]);

        Paragraph::new(Text::from_lines([line1, line2]))
            .wrap(WrapMode::Word)
            .render(area, frame);
    }

    fn render_summary(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let mut lines = Vec::new();
        if let Some(selected) = self.palette.selected_match() {
            let score = selected.result.score * 100.0;
            let bf = selected.result.evidence.combined_bayes_factor();
            lines.push(Line::from_spans([
                Span::styled("Selected: ", theme::muted()),
                Span::styled(
                    selected.action.title.as_str(),
                    Style::new().fg(theme::fg::PRIMARY).bold(),
                ),
            ]));
            lines.push(Line::from_spans([
                Span::styled("Match: ", theme::muted()),
                Span::styled(
                    format!("{:?}", selected.result.match_type),
                    Style::new().fg(theme::accent::INFO),
                ),
                Span::styled("  P=", theme::muted()),
                Span::styled(
                    format!("{score:.1}%"),
                    Style::new().fg(theme::accent::SUCCESS).bold(),
                ),
                Span::styled("  BF=", theme::muted()),
                Span::styled(format!("{bf:.2}"), Style::new().fg(theme::accent::PRIMARY)),
            ]));
        } else {
            lines.push(Line::from_spans([Span::styled(
                "No matching results.",
                theme::muted(),
            )]));
        }

        if let Some(top) = self.palette.results().next()
            && let Some(entry) = top.result.evidence.entries().first()
        {
            lines.push(Line::from_spans([
                Span::styled("Why this won: ", theme::muted()),
                Span::styled(
                    format!("{} · {}", top.action.title, entry.description),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
            ]));
        }

        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(area, frame);
    }

    fn render_ledger(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let mut lines = Vec::new();
        if let Some(selected) = self.palette.selected_match() {
            for entry in selected.result.evidence.entries() {
                lines.push(Line::from_spans([Span::raw(format!("{entry}"))]));
            }
        } else {
            lines.push(Line::from_spans([Span::styled(
                "No evidence (no matches).",
                theme::muted(),
            )]));
        }

        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(area, frame);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let cols = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);

        self.render_bench_panel(frame, cols[0]);
        self.render_hint_panel(frame, cols[1]);
    }

    fn render_bench_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Bench (deterministic)")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let status = if self.bench.enabled { "ON" } else { "OFF" };
        let qps = self.bench_qps();
        let current_query = if self.bench.enabled {
            BENCH_QUERIES[self.bench.query_index]
        } else {
            "-"
        };

        let lines = [
            Line::from_spans([
                Span::styled("Status: ", theme::muted()),
                Span::styled(
                    status,
                    Style::new()
                        .fg(if self.bench.enabled {
                            theme::accent::SUCCESS
                        } else {
                            theme::accent::ERROR
                        })
                        .bold(),
                ),
                Span::styled("  QPS: ", theme::muted()),
                Span::styled(format!("{qps:.1}"), Style::new().fg(theme::accent::INFO)),
            ]),
            Line::from_spans([
                Span::styled("Processed: ", theme::muted()),
                Span::styled(
                    format!("{}", self.bench.processed),
                    Style::new().fg(theme::fg::PRIMARY),
                ),
                Span::styled("  Query: ", theme::muted()),
                Span::styled(current_query, Style::new().fg(theme::accent::PRIMARY)),
            ]),
        ];

        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    fn render_hint_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Hint Ranker")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines = Vec::new();
        for entry in self.hint_ledger.iter().take(3) {
            lines.push(Line::from_spans([
                Span::styled(
                    format!("{}. ", entry.rank + 1),
                    Style::new().fg(theme::fg::SECONDARY),
                ),
                Span::styled(entry.label.as_str(), Style::new().fg(theme::fg::PRIMARY)),
            ]));
            lines.push(Line::from_spans([
                Span::styled("EU=", theme::muted()),
                Span::styled(
                    format!("{:.2}", entry.expected_utility),
                    Style::new().fg(theme::accent::SUCCESS),
                ),
                Span::styled("  V=", theme::muted()),
                Span::styled(
                    format!("{:.2}", entry.net_value),
                    Style::new().fg(theme::accent::INFO),
                ),
            ]));
        }

        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }
}

impl Screen for CommandPaletteEvidenceLab {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }) = event
            && *modifiers == Modifiers::NONE
        {
            match code {
                KeyCode::Char('b') => {
                    self.toggle_bench();
                    return Cmd::None;
                }
                KeyCode::Char('m') => {
                    self.filter_mode = self.filter_mode.next();
                    self.apply_filter();
                    return Cmd::None;
                }
                KeyCode::Char('0') => {
                    self.filter_mode = FilterMode::All;
                    self.apply_filter();
                    return Cmd::None;
                }
                KeyCode::Char('1') => {
                    self.filter_mode = FilterMode::Exact;
                    self.apply_filter();
                    return Cmd::None;
                }
                KeyCode::Char('2') => {
                    self.filter_mode = FilterMode::Prefix;
                    self.apply_filter();
                    return Cmd::None;
                }
                KeyCode::Char('3') => {
                    self.filter_mode = FilterMode::WordStart;
                    self.apply_filter();
                    return Cmd::None;
                }
                KeyCode::Char('4') => {
                    self.filter_mode = FilterMode::Substring;
                    self.apply_filter();
                    return Cmd::None;
                }
                KeyCode::Char('5') => {
                    self.filter_mode = FilterMode::Fuzzy;
                    self.apply_filter();
                    return Cmd::None;
                }
                KeyCode::Escape => {
                    self.palette.set_query("");
                    return Cmd::None;
                }
                _ => {}
            }
        }

        let _ = self.palette.handle_event(event);
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(2), Constraint::Min(6)])
            .split(area);

        self.render_header(frame, rows[0]);

        let cols = Flex::horizontal()
            .gap(theme::spacing::SM)
            .constraints([Constraint::Percentage(55.0), Constraint::Fill])
            .split(rows[1]);

        self.palette.render(cols[0], frame);

        let evidence_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Evidence Ledger")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));
        let inner = evidence_block.inner(cols[1]);
        evidence_block.render(cols[1], frame);
        if inner.is_empty() {
            return;
        }

        let right_rows = Flex::vertical()
            .gap(theme::spacing::XS)
            .constraints([
                Constraint::Fixed(4),
                Constraint::Min(6),
                Constraint::Fixed(6),
            ])
            .split(inner);

        self.render_summary(frame, right_rows[0]);
        self.render_ledger(frame, right_rows[1]);
        self.render_footer(frame, right_rows[2]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "0-5",
                action: "Match filter",
            },
            HelpEntry {
                key: "m",
                action: "Cycle filter",
            },
            HelpEntry {
                key: "b",
                action: "Toggle bench",
            },
            HelpEntry {
                key: "\u{2191}/\u{2193}",
                action: "Navigate results",
            },
            HelpEntry {
                key: "Enter",
                action: "Execute (demo)",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Command Palette Evidence Lab"
    }

    fn tab_label(&self) -> &'static str {
        "Palette"
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;

        if self.bench.enabled {
            let elapsed = self.tick_count.saturating_sub(self.bench.start_tick);
            if elapsed > 0
                && elapsed.is_multiple_of(BENCH_STEP_TICKS)
                && elapsed != self.bench.last_step_tick
            {
                self.bench.last_step_tick = elapsed;
                self.bench.query_index = (self.bench.query_index + 1) % BENCH_QUERIES.len();
                let query = BENCH_QUERIES[self.bench.query_index];
                self.palette.set_query(query);
                self.bench.processed = self.bench.processed.saturating_add(1);
            }
        }

        let (_, ledger) = self.hint_ranker.rank(None);
        self.hint_ledger = ledger;
    }
}

fn sample_actions() -> Vec<ActionItem> {
    vec![
        ActionItem::new("cmd:open", "Open File")
            .with_description("Open a file from disk")
            .with_tags(&["file", "open"])
            .with_category("File"),
        ActionItem::new("cmd:save", "Save File")
            .with_description("Save current buffer")
            .with_tags(&["file", "save"])
            .with_category("File"),
        ActionItem::new("cmd:find", "Find in Files")
            .with_description("Search across project")
            .with_tags(&["search", "grep", "rg"])
            .with_category("Search"),
        ActionItem::new("cmd:palette", "Open Command Palette")
            .with_description("Quick actions and navigation")
            .with_tags(&["palette", "command", "search"])
            .with_category("Navigation"),
        ActionItem::new("cmd:markdown", "Go to Markdown")
            .with_description("Switch to Markdown screen")
            .with_tags(&["markdown", "docs"])
            .with_category("Navigation"),
        ActionItem::new("cmd:logs", "Go to Log Search")
            .with_description("Filter live logs")
            .with_tags(&["logs", "search"])
            .with_category("Navigation"),
        ActionItem::new("cmd:perf", "Toggle Performance HUD")
            .with_description("Show render budget overlay")
            .with_tags(&["perf", "hud"])
            .with_category("View"),
        ActionItem::new("cmd:inline", "Inline Mode")
            .with_description("Switch to inline mode story")
            .with_tags(&["inline", "scrollback"])
            .with_category("View"),
        ActionItem::new("cmd:theme", "Cycle Theme")
            .with_description("Rotate theme palette")
            .with_tags(&["theme", "colors"])
            .with_category("View"),
        ActionItem::new("cmd:help", "Show Help")
            .with_description("Display keybinding overlay")
            .with_tags(&["help", "keys"])
            .with_category("App"),
        ActionItem::new("cmd:quit", "Quit")
            .with_description("Exit the application")
            .with_tags(&["exit"])
            .with_category("App"),
        ActionItem::new("cmd:reload", "Reload Workspace")
            .with_description("Refresh indexes and caches")
            .with_tags(&["reload", "refresh"])
            .with_category("System"),
    ]
}

fn build_hint_ranker() -> HintRanker {
    let mut ranker = HintRanker::new(RankerConfig::default());
    let open_id = ranker.register("Ctrl+P Open Palette", 14.0, HintContext::Global, 1);
    let exec_id = ranker.register("Enter Execute", 10.0, HintContext::Global, 2);
    let nav_id = ranker.register("↑/↓ Navigate", 10.0, HintContext::Global, 3);
    let bench_id = ranker.register("b Toggle Bench", 12.0, HintContext::Global, 4);
    let mode_id = ranker.register("0-5 Match Filter", 14.0, HintContext::Global, 5);

    for _ in 0..6 {
        ranker.record_usage(open_id);
    }
    for _ in 0..4 {
        ranker.record_usage(exec_id);
    }
    for _ in 0..3 {
        ranker.record_usage(nav_id);
    }
    for _ in 0..2 {
        ranker.record_usage(mode_id);
    }
    ranker.record_shown_not_used(bench_id);

    ranker
}
