#![forbid(unsafe_code)]

//! Command Palette Evidence Lab — explainable ranking + evidence ledger.
//!
//! Demonstrates:
//! - Command palette evidence ledger (Bayesian scoring)
//! - Match-mode filtering (exact/prefix/substring/fuzzy)
//! - Deterministic micro-bench (queries/sec)
//! - HintRanker evidence ledger for keybinding hints

use std::cell::Cell;
use std::time::Instant;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
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

/// Target search latency threshold in microseconds (1ms).
const LATENCY_THRESHOLD_US: u64 = 1_000;

#[derive(Debug, Clone)]
struct BenchState {
    enabled: bool,
    start_tick: u64,
    last_step_tick: u64,
    processed: u64,
    query_index: usize,
    /// Last measured query latency in microseconds.
    last_query_us: u64,
}

impl BenchState {
    fn new() -> Self {
        Self {
            enabled: false,
            start_tick: 0,
            last_step_tick: 0,
            processed: 0,
            query_index: 0,
            last_query_us: 0,
        }
    }

    fn reset(&mut self, tick_count: u64) {
        self.enabled = true;
        self.start_tick = tick_count;
        self.last_step_tick = 0;
        self.processed = 0;
        self.query_index = 0;
        self.last_query_us = 0;
    }
}

pub struct CommandPaletteEvidenceLab {
    palette: CommandPalette,
    filter_mode: FilterMode,
    bench: BenchState,
    hint_ranker: HintRanker,
    hint_ledger: Vec<RankingEvidence>,
    tick_count: u64,
    /// Cached palette area for mouse hit-testing.
    layout_palette: Cell<Rect>,
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
            layout_palette: Cell::new(Rect::default()),
        };

        lab.apply_filter();
        lab
    }

    /// Handle mouse interactions on the palette area.
    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let area = self.layout_palette.get();
        if !area.contains(x, y) {
            return;
        }
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Clicking navigates results via keyboard events
                let event = Event::Key(KeyEvent {
                    code: KeyCode::Enter,
                    modifiers: Modifiers::NONE,
                    kind: KeyEventKind::Press,
                });
                let _ = self.palette.handle_event(&event);
            }
            MouseEventKind::ScrollUp => {
                let event = Event::Key(KeyEvent {
                    code: KeyCode::Up,
                    modifiers: Modifiers::NONE,
                    kind: KeyEventKind::Press,
                });
                for _ in 0..3 {
                    let _ = self.palette.handle_event(&event);
                }
            }
            MouseEventKind::ScrollDown => {
                let event = Event::Key(KeyEvent {
                    code: KeyCode::Down,
                    modifiers: Modifiers::NONE,
                    kind: KeyEventKind::Press,
                });
                for _ in 0..3 {
                    let _ = self.palette.handle_event(&event);
                }
            }
            _ => {}
        }
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

        let latency_us = self.bench.last_query_us;
        let within_budget = latency_us <= LATENCY_THRESHOLD_US;
        let latency_color = if !self.bench.enabled {
            theme::fg::SECONDARY
        } else if within_budget {
            theme::accent::SUCCESS
        } else {
            theme::accent::ERROR
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
            Line::from_spans([
                Span::styled("Latency: ", theme::muted()),
                Span::styled(format!("{latency_us}us"), Style::new().fg(latency_color)),
                Span::styled(format!("  (< {}us)", LATENCY_THRESHOLD_US), theme::muted()),
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
        if let Event::Mouse(mouse) = event {
            self.handle_mouse(mouse.kind, mouse.x, mouse.y);
            return Cmd::None;
        }
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

        self.layout_palette.set(cols[0]);
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
                Constraint::Fixed(7),
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
            HelpEntry {
                key: "Click",
                action: "Execute selected",
            },
            HelpEntry {
                key: "Scroll",
                action: "Navigate results",
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
                let start = Instant::now();
                self.palette.set_query(query);
                self.bench.last_query_us = start.elapsed().as_micros() as u64;
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

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_widgets::command_palette::MatchType;

    #[test]
    fn lab_creates_with_default_query() {
        let lab = CommandPaletteEvidenceLab::new();
        assert_eq!(lab.filter_mode, FilterMode::All);
        assert!(!lab.bench.enabled);
        assert_eq!(lab.tick_count, 0);
    }

    #[test]
    fn filter_mode_cycles_correctly() {
        let mut mode = FilterMode::All;
        let expected = [
            FilterMode::Exact,
            FilterMode::Prefix,
            FilterMode::WordStart,
            FilterMode::Substring,
            FilterMode::Fuzzy,
            FilterMode::All,
        ];
        for expected_mode in expected {
            mode = mode.next();
            assert_eq!(mode, expected_mode);
        }
    }

    #[test]
    fn filter_mode_labels_all_unique() {
        let modes = [
            FilterMode::All,
            FilterMode::Exact,
            FilterMode::Prefix,
            FilterMode::WordStart,
            FilterMode::Substring,
            FilterMode::Fuzzy,
        ];
        let labels: Vec<&str> = modes.iter().map(|m| m.label()).collect();
        for (i, a) in labels.iter().enumerate() {
            for (j, b) in labels.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "duplicate label at index {i} and {j}");
                }
            }
        }
    }

    #[test]
    fn evidence_scores_ordered_correctly() {
        let lab = CommandPaletteEvidenceLab::new();
        let results: Vec<_> = lab.palette.results().collect();
        for window in results.windows(2) {
            assert!(
                window[0].result.score >= window[1].result.score,
                "results not in descending score order: {} vs {}",
                window[0].result.score,
                window[1].result.score,
            );
        }
    }

    #[test]
    fn evidence_ledger_has_entries_for_matches() {
        let lab = CommandPaletteEvidenceLab::new();
        for matched in lab.palette.results() {
            if matched.result.match_type != MatchType::NoMatch {
                assert!(
                    !matched.result.evidence.entries().is_empty(),
                    "matched result '{}' has empty evidence",
                    matched.action.title,
                );
            }
        }
    }

    #[test]
    fn filter_mode_to_match_filter_roundtrips() {
        let modes = [
            FilterMode::All,
            FilterMode::Exact,
            FilterMode::Prefix,
            FilterMode::WordStart,
            FilterMode::Substring,
            FilterMode::Fuzzy,
        ];
        for mode in modes {
            let filter = mode.to_match_filter();
            let label = mode.label();
            assert!(!label.is_empty());
            let _ = format!("{filter:?}");
        }
    }

    #[test]
    fn bench_state_reset_clears_processed() {
        let mut bench = BenchState::new();
        bench.processed = 42;
        bench.last_query_us = 999;
        bench.reset(100);
        assert!(bench.enabled);
        assert_eq!(bench.processed, 0);
        assert_eq!(bench.start_tick, 100);
        assert_eq!(bench.last_query_us, 0);
    }

    #[test]
    fn bench_qps_returns_zero_when_disabled() {
        let lab = CommandPaletteEvidenceLab::new();
        assert_eq!(lab.bench_qps(), 0.0);
    }

    #[test]
    fn toggle_bench_enables_then_disables() {
        let mut lab = CommandPaletteEvidenceLab::new();
        assert!(!lab.bench.enabled);
        lab.toggle_bench();
        assert!(lab.bench.enabled);
        lab.toggle_bench();
        assert!(!lab.bench.enabled);
    }

    #[test]
    fn tick_advances_bench_query_index() {
        let mut lab = CommandPaletteEvidenceLab::new();
        lab.toggle_bench();
        assert_eq!(lab.bench.query_index, 0);
        for t in 1..=(BENCH_STEP_TICKS + 1) {
            lab.tick(t);
        }
        assert!(lab.bench.processed > 0);
    }

    #[test]
    fn sample_actions_all_have_ids_and_titles() {
        let actions = sample_actions();
        assert!(
            actions.len() >= 10,
            "should have at least 10 sample actions"
        );
        for action in &actions {
            assert!(!action.id.is_empty(), "action missing id");
            assert!(!action.title.is_empty(), "action missing title");
        }
    }

    #[test]
    fn hint_ranker_produces_evidence() {
        let lab = CommandPaletteEvidenceLab::new();
        assert!(
            !lab.hint_ledger.is_empty(),
            "hint ledger should have entries",
        );
        for entry in &lab.hint_ledger {
            assert!(!entry.label.is_empty());
        }
    }

    #[test]
    fn keybindings_not_empty() {
        let lab = CommandPaletteEvidenceLab::new();
        let keys = lab.keybindings();
        assert!(keys.len() >= 3, "should have at least 3 keybindings");
    }

    #[test]
    fn render_does_not_panic_on_small_area() {
        let lab = CommandPaletteEvidenceLab::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        let area = Rect::new(0, 0, 10, 5);
        lab.view(&mut frame, area);
    }

    #[test]
    fn render_does_not_panic_on_zero_area() {
        let lab = CommandPaletteEvidenceLab::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 0, 0);
        lab.view(&mut frame, area);
    }

    #[test]
    fn scroll_down_on_palette_navigates() {
        use super::Screen;
        use ftui_core::event::MouseEvent;
        let mut lab = CommandPaletteEvidenceLab::new();
        // Clear query to get all results (default "log" query has only 1 match)
        lab.palette.set_query("");
        lab.layout_palette.set(Rect::new(0, 0, 40, 20));
        let initial = lab.palette.selected_index();
        lab.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            10,
            10,
        )));
        assert!(
            lab.palette.selected_index() > initial,
            "selected {} should be > initial {}",
            lab.palette.selected_index(),
            initial
        );
    }

    #[test]
    fn scroll_up_on_palette_navigates() {
        use super::Screen;
        use ftui_core::event::MouseEvent;
        let mut lab = CommandPaletteEvidenceLab::new();
        // Clear query to get all results
        lab.palette.set_query("");
        lab.layout_palette.set(Rect::new(0, 0, 40, 20));
        // Scroll down first to get past index 0
        lab.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            10,
            10,
        )));
        let after_down = lab.palette.selected_index();
        assert!(after_down > 0, "should have scrolled down");
        lab.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            10,
            10,
        )));
        assert!(lab.palette.selected_index() < after_down);
    }

    #[test]
    fn mouse_outside_palette_ignored() {
        use super::Screen;
        use ftui_core::event::MouseEvent;
        let mut lab = CommandPaletteEvidenceLab::new();
        lab.layout_palette.set(Rect::new(0, 0, 40, 20));
        let initial = lab.palette.selected_index();
        lab.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            50, // outside palette
            10,
        )));
        assert_eq!(lab.palette.selected_index(), initial);
    }

    #[test]
    fn click_on_palette_executes() {
        use super::Screen;
        use ftui_core::event::MouseEvent;
        let mut lab = CommandPaletteEvidenceLab::new();
        lab.layout_palette.set(Rect::new(0, 0, 40, 20));
        // Click should not panic
        lab.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            10,
            10,
        )));
    }

    #[test]
    fn mouse_move_ignored() {
        use super::Screen;
        use ftui_core::event::MouseEvent;
        let mut lab = CommandPaletteEvidenceLab::new();
        lab.layout_palette.set(Rect::new(0, 0, 40, 20));
        let initial = lab.palette.selected_index();
        lab.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::Moved,
            10,
            10,
        )));
        assert_eq!(lab.palette.selected_index(), initial);
    }

}
