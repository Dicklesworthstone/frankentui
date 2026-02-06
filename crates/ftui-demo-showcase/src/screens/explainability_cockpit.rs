#![forbid(unsafe_code)]

//! Explainability cockpit — unified evidence ledger view.
//!
//! This screen consolidates diff strategy, resize regime, and budget decisions
//! into a single panel with a compact timeline for debugging.

use std::cell::Cell as StdCell;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::time::SystemTime;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::{Line, Span, Text, WrapMode};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use serde_json::Value;

use super::{HelpEntry, Screen};
use crate::theme;

const MAX_EVIDENCE_LINES: usize = 400;
const MAX_TIMELINE_ROWS: usize = 10;
const REFRESH_EVERY_TICKS: u64 = 5;
const MIN_PANEL_HEIGHT: u16 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceKind {
    Diff,
    Resize,
    Budget,
}

#[derive(Debug, Clone)]
struct TimelineEntry {
    seq: u64,
    kind: EvidenceKind,
    index: u64,
    summary: String,
    posterior: Option<String>,
}

#[derive(Debug, Clone)]
struct DiffSummary {
    event_idx: u64,
    strategy: String,
    posterior_mean: Option<f64>,
    posterior_variance: Option<f64>,
    alpha: Option<f64>,
    beta: Option<f64>,
    guard_reason: Option<String>,
    fallback_reason: Option<String>,
    hysteresis_applied: bool,
    hysteresis_ratio: Option<f64>,
    dirty_rows: Option<u64>,
    total_rows: Option<u64>,
    dirty_tile_ratio: Option<f64>,
    dirty_cell_ratio: Option<f64>,
}

#[derive(Debug, Clone)]
struct ResizeEvidenceSummary {
    log_bayes_factor: f64,
    regime_contribution: f64,
    timing_contribution: f64,
    rate_contribution: f64,
    explanation: String,
}

#[derive(Debug, Clone)]
struct ResizeSummary {
    event_idx: u64,
    action: String,
    regime: String,
    dt_ms: Option<f64>,
    event_rate: Option<f64>,
    time_since_render_ms: Option<f64>,
    forced: bool,
    evidence: Option<ResizeEvidenceSummary>,
}

#[derive(Debug, Clone)]
struct BudgetSummary {
    frame_idx: u64,
    decision: String,
    controller_decision: Option<String>,
    degradation_before: Option<String>,
    degradation_after: Option<String>,
    frame_time_us: Option<f64>,
    budget_us: Option<f64>,
    e_value: Option<f64>,
    in_warmup: Option<bool>,
    conformal_alpha: Option<f64>,
    conformal_q_b: Option<f64>,
    conformal_upper_us: Option<f64>,
    conformal_risk: Option<bool>,
}

#[derive(Debug, Clone)]
struct ParsedEvidence {
    diff: Option<DiffSummary>,
    resize: Option<ResizeSummary>,
    budget: Option<BudgetSummary>,
    timeline: Vec<TimelineEntry>,
    line_count: usize,
    parsed_count: usize,
}

#[derive(Debug, Clone)]
struct SourceStatus {
    label: String,
    status: String,
    hint_lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct ExplainabilityData {
    source: SourceStatus,
    diff: Option<DiffSummary>,
    resize: Option<ResizeSummary>,
    budget: Option<BudgetSummary>,
    timeline: Vec<TimelineEntry>,
}

impl ExplainabilityData {
    fn is_empty(&self) -> bool {
        self.diff.is_none() && self.resize.is_none() && self.budget.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPanel {
    Diff,
    Resize,
    Budget,
    Timeline,
}

#[derive(Debug, Clone, Copy)]
enum CockpitMode {
    Full,
    Overlay,
}

/// Explainability cockpit screen state.
pub struct ExplainabilityCockpit {
    data: ExplainabilityData,
    evidence_path: Option<PathBuf>,
    last_refresh_tick: u64,
    last_modified: Option<SystemTime>,
    last_size: Option<u64>,
    /// When true, auto-refresh is paused.
    paused: bool,
    /// Currently focused panel.
    focused_panel: FocusPanel,
    /// Timeline scroll offset (number of entries scrolled from the bottom).
    timeline_scroll: usize,
    // Cached layout rects for mouse hit testing.
    layout_diff: StdCell<Rect>,
    layout_resize: StdCell<Rect>,
    layout_budget: StdCell<Rect>,
    layout_timeline: StdCell<Rect>,
}

impl Default for ExplainabilityCockpit {
    fn default() -> Self {
        Self::new()
    }
}

impl ExplainabilityCockpit {
    pub fn new() -> Self {
        Self::with_evidence_path(resolve_evidence_path())
    }

    pub fn with_evidence_path(evidence_path: Option<PathBuf>) -> Self {
        let mut cockpit = Self {
            data: empty_data(SourceStatus {
                label: "source: (disabled)".to_string(),
                status: "Evidence source disabled".to_string(),
                hint_lines: default_hint_lines(None),
            }),
            evidence_path,
            last_refresh_tick: 0,
            last_modified: None,
            last_size: None,
            paused: false,
            focused_panel: FocusPanel::Timeline,
            timeline_scroll: 0,
            layout_diff: StdCell::new(Rect::default()),
            layout_resize: StdCell::new(Rect::default()),
            layout_budget: StdCell::new(Rect::default()),
            layout_timeline: StdCell::new(Rect::default()),
        };
        cockpit.refresh(true);
        cockpit
    }

    pub fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        let overlay_width = 74u16.min(area.width.saturating_sub(4));
        let overlay_height = 18u16.min(area.height.saturating_sub(4));
        if overlay_width < 40 || overlay_height < 10 {
            return;
        }
        let x = area.x + area.width.saturating_sub(overlay_width).saturating_sub(1);
        let y = area.y + area.height.saturating_sub(overlay_height).saturating_sub(1);
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);
        self.render(frame, overlay_area, CockpitMode::Overlay);
    }

    fn refresh(&mut self, force: bool) {
        let Some(path) = self.evidence_path.as_ref() else {
            self.data = empty_data(SourceStatus {
                label: "source: (disabled)".to_string(),
                status: "Evidence source disabled".to_string(),
                hint_lines: default_hint_lines(None),
            });
            return;
        };

        let metadata = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(_) => {
                self.data = empty_data(SourceStatus {
                    label: format!("source: {}", path.display()),
                    status: "Evidence file not found".to_string(),
                    hint_lines: default_hint_lines(Some(path)),
                });
                return;
            }
        };

        if !force {
            let modified = metadata.modified().ok();
            let size = Some(metadata.len());
            if modified.is_some() && modified == self.last_modified && size == self.last_size {
                return;
            }
        }

        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(err) => {
                self.data = empty_data(SourceStatus {
                    label: format!("source: {}", path.display()),
                    status: format!("Failed to read evidence: {err}"),
                    hint_lines: default_hint_lines(Some(path)),
                });
                return;
            }
        };

        let reader = io::BufReader::new(file);
        let mut ring: VecDeque<String> = VecDeque::new();
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(err) => {
                    self.data = empty_data(SourceStatus {
                        label: format!("source: {}", path.display()),
                        status: format!("Failed to read evidence: {err}"),
                        hint_lines: default_hint_lines(Some(path)),
                    });
                    return;
                }
            };
            if ring.len() == MAX_EVIDENCE_LINES {
                ring.pop_front();
            }
            ring.push_back(line);
        }
        let lines: Vec<&str> = ring.iter().map(|line| line.as_str()).collect();
        let parsed = parse_evidence_lines(&lines);

        self.last_modified = metadata.modified().ok();
        self.last_size = Some(metadata.len());

        let status = if parsed.parsed_count == 0 {
            "No evidence entries parsed".to_string()
        } else {
            format!(
                "Loaded {} entries ({} lines)",
                parsed.parsed_count, parsed.line_count
            )
        };

        let data = ExplainabilityData {
            source: SourceStatus {
                label: format!("source: {}", path.display()),
                status,
                hint_lines: Vec::new(),
            },
            diff: parsed.diff,
            resize: parsed.resize,
            budget: parsed.budget,
            timeline: parsed.timeline,
        };

        if data.is_empty() {
            self.data = empty_data(SourceStatus {
                label: format!("source: {}", path.display()),
                status: "Evidence log is empty".to_string(),
                hint_lines: default_hint_lines(Some(path)),
            });
        } else {
            self.data = data;
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, mode: CockpitMode) {
        if area.is_empty() {
            return;
        }

        let header_height = match mode {
            CockpitMode::Full => 2,
            CockpitMode::Overlay => 1,
        };
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(header_height),
                Constraint::Min(MIN_PANEL_HEIGHT),
                Constraint::Min(6),
            ])
            .split(area);

        self.render_header(frame, rows[0], mode);

        if self.data.is_empty() {
            self.render_empty_state(frame, rows[1]);
            self.render_timeline(frame, rows[2], mode);
            return;
        }

        let cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(34.0),
                Constraint::Percentage(33.0),
                Constraint::Percentage(33.0),
            ])
            .split(rows[1]);

        // Cache layout rects for mouse hit testing.
        self.layout_diff.set(cols[0]);
        self.layout_resize.set(cols[1]);
        self.layout_budget.set(cols[2]);
        self.layout_timeline.set(rows[2]);

        self.render_diff_panel(frame, cols[0], self.focused_panel == FocusPanel::Diff);
        self.render_resize_panel(frame, cols[1], self.focused_panel == FocusPanel::Resize);
        self.render_budget_panel(frame, cols[2], self.focused_panel == FocusPanel::Budget);
        self.render_timeline(frame, rows[2], mode);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect, mode: CockpitMode) {
        if area.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        lines.push(Line::from_spans([
            Span::styled("Explainability Cockpit", theme::title()),
            Span::styled(" · ", theme::muted()),
            Span::styled(self.data.source.label.clone(), theme::muted()),
        ]));
        if matches!(mode, CockpitMode::Full) {
            lines.push(Line::from_spans([Span::styled(
                self.data.source.status.clone(),
                theme::subtitle(),
            )]));
        }
        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(area, frame);
    }

    fn render_empty_state(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Explainability Cockpit")
            .title_alignment(Alignment::Center)
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        lines.push(Line::from_spans([Span::styled(
            self.data.source.status.clone(),
            theme::muted(),
        )]));
        for hint in &self.data.source.hint_lines {
            lines.push(Line::from_spans([Span::styled(hint, theme::body())]));
        }
        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    fn render_diff_panel(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let block = panel_block("Diff Strategy", theme::accent::PRIMARY, focused);
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        if let Some(summary) = &self.data.diff {
            lines.extend(diff_lines(summary));
        } else {
            lines.push(empty_panel_line("No diff evidence yet."));
        }
        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    fn render_resize_panel(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let block = panel_block("Resize Regime (BOCPD)", theme::accent::INFO, focused);
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        if let Some(summary) = &self.data.resize {
            lines.extend(resize_lines(summary));
        } else {
            lines.push(empty_panel_line("No resize evidence yet."));
        }
        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    fn render_budget_panel(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let block = panel_block("Budget Decisions", theme::accent::WARNING, focused);
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        if let Some(summary) = &self.data.budget {
            lines.extend(budget_lines(summary));
        } else {
            lines.push(empty_panel_line("No budget evidence yet."));
        }
        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    fn render_timeline(&self, frame: &mut Frame, area: Rect, mode: CockpitMode) {
        if area.is_empty() {
            return;
        }
        let max_rows = match mode {
            CockpitMode::Full => MAX_TIMELINE_ROWS,
            CockpitMode::Overlay => MAX_TIMELINE_ROWS.min(6),
        };
        let focused = self.focused_panel == FocusPanel::Timeline;
        let border = if focused {
            BorderType::Double
        } else {
            BorderType::Rounded
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(border)
            .title("Decision Timeline")
            .title_alignment(Alignment::Center)
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }

        let mut lines = Vec::new();
        if self.data.timeline.is_empty() {
            lines.push(empty_panel_line("No recent decisions."));
        } else {
            let total = self.data.timeline.len();
            let scroll = self.timeline_scroll.min(total.saturating_sub(max_rows));
            let end = total.saturating_sub(scroll);
            let start = end.saturating_sub(max_rows);
            for entry in &self.data.timeline[start..end] {
                let label = match entry.kind {
                    EvidenceKind::Diff => "diff",
                    EvidenceKind::Resize => "resize",
                    EvidenceKind::Budget => "budget",
                };
                let accent = match entry.kind {
                    EvidenceKind::Diff => theme::accent::PRIMARY,
                    EvidenceKind::Resize => theme::accent::INFO,
                    EvidenceKind::Budget => theme::accent::WARNING,
                };
                let mut spans = vec![
                    Span::styled(format!("{label:<6}"), Style::new().fg(accent).bold()),
                    Span::styled(format!(" #{:>3} ", entry.index), theme::muted()),
                    Span::styled(entry.summary.clone(), theme::body()),
                ];
                if let Some(posterior) = &entry.posterior {
                    spans.push(Span::styled(format!(" · {posterior}"), theme::muted()));
                }
                lines.push(Line::from_spans(spans));
            }
            // Show scroll indicator when not at the bottom.
            if scroll > 0 {
                lines.push(Line::from_spans([Span::styled(
                    format!("  ({scroll} more below)"),
                    theme::muted(),
                )]));
            }
        }
        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }
}

impl Screen for ExplainabilityCockpit {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        // Mouse: click to focus panels, wheel to scroll timeline.
        if let Event::Mouse(mouse) = event {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let diff = self.layout_diff.get();
                    let resize = self.layout_resize.get();
                    let budget = self.layout_budget.get();
                    let timeline = self.layout_timeline.get();
                    if diff.contains(mouse.x, mouse.y) {
                        self.focused_panel = FocusPanel::Diff;
                    } else if resize.contains(mouse.x, mouse.y) {
                        self.focused_panel = FocusPanel::Resize;
                    } else if budget.contains(mouse.x, mouse.y) {
                        self.focused_panel = FocusPanel::Budget;
                    } else if timeline.contains(mouse.x, mouse.y) {
                        self.focused_panel = FocusPanel::Timeline;
                    }
                }
                MouseEventKind::ScrollUp => {
                    let timeline = self.layout_timeline.get();
                    if timeline.contains(mouse.x, mouse.y) {
                        let max = self.data.timeline.len().saturating_sub(1);
                        self.timeline_scroll = (self.timeline_scroll + 1).min(max);
                    }
                }
                MouseEventKind::ScrollDown => {
                    let timeline = self.layout_timeline.get();
                    if timeline.contains(mouse.x, mouse.y) {
                        self.timeline_scroll = self.timeline_scroll.saturating_sub(1);
                    }
                }
                _ => {}
            }
        }

        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                KeyCode::Char('r') => self.refresh(true),
                KeyCode::Char(' ') => self.paused = !self.paused,
                KeyCode::Char('c') => {
                    // Clear accumulated evidence and re-read from source.
                    self.data.timeline.clear();
                    self.data.diff = None;
                    self.data.resize = None;
                    self.data.budget = None;
                    self.refresh(true);
                }
                // Panel focus via number keys.
                KeyCode::Char('1') => self.focused_panel = FocusPanel::Diff,
                KeyCode::Char('2') => self.focused_panel = FocusPanel::Resize,
                KeyCode::Char('3') => self.focused_panel = FocusPanel::Budget,
                KeyCode::Char('4') => self.focused_panel = FocusPanel::Timeline,
                // Navigate timeline: n = scroll up (older), p = scroll down (newer).
                KeyCode::Char('n') => {
                    let max = self.data.timeline.len().saturating_sub(1);
                    self.timeline_scroll = (self.timeline_scroll + 1).min(max);
                }
                KeyCode::Char('p') => {
                    self.timeline_scroll = self.timeline_scroll.saturating_sub(1);
                }
                // Arrow keys for timeline scroll.
                KeyCode::Up => {
                    let max = self.data.timeline.len().saturating_sub(1);
                    self.timeline_scroll = (self.timeline_scroll + 1).min(max);
                }
                KeyCode::Down => {
                    self.timeline_scroll = self.timeline_scroll.saturating_sub(1);
                }
                _ => {}
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        self.render(frame, area, CockpitMode::Full);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "r",
                action: "Refresh evidence log",
            },
            HelpEntry {
                key: "Space",
                action: if self.paused {
                    "Resume auto-refresh"
                } else {
                    "Pause auto-refresh"
                },
            },
            HelpEntry {
                key: "c",
                action: "Clear evidence and re-read",
            },
            HelpEntry {
                key: "1/2/3/4",
                action: "Focus panel (Diff/Resize/Budget/Timeline)",
            },
            HelpEntry {
                key: "n/p",
                action: "Scroll timeline (older/newer)",
            },
            HelpEntry {
                key: "\u{2191}/\u{2193}",
                action: "Scroll timeline",
            },
        ]
    }

    fn tick(&mut self, tick_count: u64) {
        if self.paused {
            return;
        }
        if tick_count.saturating_sub(self.last_refresh_tick) >= REFRESH_EVERY_TICKS {
            self.last_refresh_tick = tick_count;
            self.refresh(false);
        }
    }

    fn title(&self) -> &'static str {
        "Explainability Cockpit"
    }

    fn tab_label(&self) -> &'static str {
        "Explain"
    }
}

fn panel_block(title: &str, accent: theme::ColorToken, focused: bool) -> Block<'_> {
    let border = if focused {
        BorderType::Double
    } else {
        BorderType::Rounded
    };
    Block::new()
        .borders(Borders::ALL)
        .border_type(border)
        .title(title)
        .title_alignment(Alignment::Center)
        .style(Style::new().fg(accent))
}

fn empty_panel_line(message: &str) -> Line {
    Line::from_spans([Span::styled(message, theme::muted())])
}

fn diff_lines(summary: &DiffSummary) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut why_parts = Vec::new();
    if let Some(reason) = summary.guard_reason.as_ref().filter(|s| !s.is_empty()) {
        why_parts.push(format!("guard: {reason}"));
    }
    if let Some(reason) = summary.fallback_reason.as_ref().filter(|s| !s.is_empty()) {
        why_parts.push(format!("fallback: {reason}"));
    }
    if why_parts.is_empty() {
        why_parts.push(format!("strategy {}", summary.strategy));
    }
    let why_line = why_parts.join(" · ");
    lines.push(Line::from_spans([
        Span::styled("Decision: ", theme::muted()),
        Span::styled(
            summary.strategy.clone(),
            Style::new().fg(theme::accent::PRIMARY).bold(),
        ),
    ]));
    lines.push(Line::from_spans([
        Span::styled("Why: ", theme::muted()),
        Span::styled(why_line, theme::body()),
    ]));
    if let (Some(mean), Some(var), Some(alpha), Some(beta)) = (
        summary.posterior_mean,
        summary.posterior_variance,
        summary.alpha,
        summary.beta,
    ) {
        lines.push(Line::from_spans([
            Span::styled("Posterior: ", theme::muted()),
            Span::styled(
                format!("μ={mean:.3} σ²={var:.3} α={alpha:.2} β={beta:.2}"),
                theme::body(),
            ),
        ]));
    }
    if let (Some(dirty), Some(total)) = (summary.dirty_rows, summary.total_rows) {
        let ratio = (dirty as f64 / total.max(1) as f64) * 100.0;
        lines.push(Line::from_spans([
            Span::styled("Dirty rows: ", theme::muted()),
            Span::styled(format!("{dirty}/{total} ({ratio:.1}%)"), theme::body()),
        ]));
    }
    if summary.hysteresis_applied {
        let ratio = summary.hysteresis_ratio.unwrap_or(1.0);
        lines.push(Line::from_spans([
            Span::styled("Hysteresis: ", theme::muted()),
            Span::styled(format!("applied ({ratio:.2}x)"), theme::body()),
        ]));
    }
    if let (Some(tile_ratio), Some(cell_ratio)) =
        (summary.dirty_tile_ratio, summary.dirty_cell_ratio)
    {
        lines.push(Line::from_spans([
            Span::styled("Coverage: ", theme::muted()),
            Span::styled(
                format!(
                    "tiles={:.1}% cells={:.1}%",
                    tile_ratio * 100.0,
                    cell_ratio * 100.0
                ),
                theme::body(),
            ),
        ]));
    }
    lines
}

fn resize_lines(summary: &ResizeSummary) -> Vec<Line> {
    let mut lines = Vec::new();
    lines.push(Line::from_spans([
        Span::styled("Decision: ", theme::muted()),
        Span::styled(
            format!("{} ({})", summary.action, summary.regime),
            Style::new().fg(theme::accent::INFO).bold(),
        ),
    ]));
    if let Some(evidence) = &summary.evidence {
        lines.push(Line::from_spans([
            Span::styled("Why: ", theme::muted()),
            Span::styled(evidence.explanation.clone(), theme::body()),
        ]));
        lines.push(Line::from_spans([
            Span::styled("Evidence: ", theme::muted()),
            Span::styled(
                format!(
                    "LBF={:.2} (regime {:.2}, timing {:.2}, rate {:.2})",
                    evidence.log_bayes_factor,
                    evidence.regime_contribution,
                    evidence.timing_contribution,
                    evidence.rate_contribution
                ),
                theme::body(),
            ),
        ]));
    } else if let Some(dt_ms) = summary.dt_ms {
        let rate = summary.event_rate.unwrap_or(0.0);
        lines.push(Line::from_spans([
            Span::styled("Why: ", theme::muted()),
            Span::styled(format!("Δt={dt_ms:.1}ms · rate={rate:.1}/s"), theme::body()),
        ]));
    }
    if let Some(time_since_render_ms) = summary.time_since_render_ms {
        lines.push(Line::from_spans([
            Span::styled("Render gap: ", theme::muted()),
            Span::styled(format!("{time_since_render_ms:.1}ms"), theme::body()),
        ]));
    }
    if summary.forced {
        lines.push(Line::from_spans([Span::styled(
            "Forced apply",
            Style::new().fg(theme::accent::WARNING),
        )]));
    }
    lines
}

fn budget_lines(summary: &BudgetSummary) -> Vec<Line> {
    let mut lines = Vec::new();
    let decision = summary.decision.clone();
    lines.push(Line::from_spans([
        Span::styled("Decision: ", theme::muted()),
        Span::styled(decision, Style::new().fg(theme::accent::WARNING).bold()),
    ]));
    if let (Some(frame_us), Some(budget_us)) = (summary.frame_time_us, summary.budget_us) {
        lines.push(Line::from_spans([
            Span::styled("Frame: ", theme::muted()),
            Span::styled(
                format!("{:.2}ms / {:.2}ms", frame_us / 1000.0, budget_us / 1000.0),
                theme::body(),
            ),
        ]));
    }
    if let Some(e_value) = summary.e_value {
        lines.push(Line::from_spans([
            Span::styled("E-value: ", theme::muted()),
            Span::styled(format!("{e_value:.3}"), theme::body()),
        ]));
    }
    if let Some(in_warmup) = summary.in_warmup {
        let status = if in_warmup { "warmup" } else { "steady" };
        lines.push(Line::from_spans([
            Span::styled("Phase: ", theme::muted()),
            Span::styled(status, theme::body()),
        ]));
    }
    if let (Some(alpha), Some(q_b), Some(upper_us)) = (
        summary.conformal_alpha,
        summary.conformal_q_b,
        summary.conformal_upper_us,
    ) {
        let risk = summary
            .conformal_risk
            .map(|risk| if risk { "risk" } else { "safe" })
            .unwrap_or("safe");
        lines.push(Line::from_spans([
            Span::styled("Conformal: ", theme::muted()),
            Span::styled(
                format!(
                    "α={alpha:.2} q={q_b:.1} upper={:.1}ms ({risk})",
                    upper_us / 1000.0
                ),
                theme::body(),
            ),
        ]));
    }
    if let Some(controller) = summary.controller_decision.as_ref() {
        lines.push(Line::from_spans([
            Span::styled("Controller: ", theme::muted()),
            Span::styled(controller, theme::body()),
        ]));
    }
    lines
}

fn empty_data(source: SourceStatus) -> ExplainabilityData {
    ExplainabilityData {
        source,
        diff: None,
        resize: None,
        budget: None,
        timeline: Vec::new(),
    }
}

fn default_hint_lines(path: Option<&PathBuf>) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("Enable evidence logging to populate this cockpit.".to_string());
    lines.push("Set FTUI_DEMO_EVIDENCE_JSONL to a writable path.".to_string());
    if let Some(path) = path {
        lines.push(format!(
            "Example: FTUI_DEMO_EVIDENCE_JSONL={} cargo run -p ftui-demo-showcase",
            path.display()
        ));
    } else {
        lines.push("Example: FTUI_DEMO_EVIDENCE_JSONL=/tmp/ftui_evidence.jsonl cargo run -p ftui-demo-showcase".to_string());
    }
    lines
}

fn resolve_evidence_path() -> Option<PathBuf> {
    for key in ["FTUI_DEMO_EVIDENCE_JSONL", "FTUI_HARNESS_EVIDENCE_JSONL"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    None
}

fn parse_evidence_lines(lines: &[&str]) -> ParsedEvidence {
    let mut diff: Option<DiffSummary> = None;
    let mut resize: Option<ResizeSummary> = None;
    let mut budget: Option<BudgetSummary> = None;
    let mut timeline = Vec::new();
    let mut resize_evidence: HashMap<u64, ResizeEvidenceSummary> = HashMap::new();
    let mut parsed_count = 0;

    for (seq, line) in lines.iter().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(event) = value.get("event").and_then(Value::as_str) else {
            continue;
        };
        match event {
            "diff_decision" => {
                if let Some(summary) = diff_from_value(&value) {
                    let posterior = summary
                        .posterior_mean
                        .zip(summary.posterior_variance)
                        .map(|(mean, var)| format!("μ={mean:.2} σ²={var:.2}"));
                    timeline.push(TimelineEntry {
                        seq: seq as u64,
                        kind: EvidenceKind::Diff,
                        index: summary.event_idx,
                        summary: format!("strategy {}", summary.strategy),
                        posterior,
                    });
                    diff = Some(summary);
                    parsed_count += 1;
                }
            }
            "decision" => {
                if value.get("regime").is_some()
                    && value.get("action").is_some()
                    && let Some(mut summary) = resize_from_value(&value)
                {
                    if let Some(evidence) = resize_evidence.get(&summary.event_idx) {
                        summary.evidence = Some(evidence.clone());
                    }
                    let posterior = summary
                        .evidence
                        .as_ref()
                        .map(|e| format!("LBF={:.2}", e.log_bayes_factor));
                    timeline.push(TimelineEntry {
                        seq: seq as u64,
                        kind: EvidenceKind::Resize,
                        index: summary.event_idx,
                        summary: format!("{} {}", summary.action, summary.regime),
                        posterior,
                    });
                    resize = Some(summary);
                    parsed_count += 1;
                }
            }
            "decision_evidence" => {
                if let Some(summary) = resize_evidence_from_value(&value) {
                    resize_evidence.insert(summary.0, summary.1);
                    parsed_count += 1;
                }
            }
            "budget_decision" => {
                if let Some(summary) = budget_from_value(&value) {
                    let posterior = summary.e_value.map(|e| format!("e={e:.2}"));
                    timeline.push(TimelineEntry {
                        seq: seq as u64,
                        kind: EvidenceKind::Budget,
                        index: summary.frame_idx,
                        summary: format!("{} budget", summary.decision),
                        posterior,
                    });
                    budget = Some(summary);
                    parsed_count += 1;
                }
            }
            _ => {}
        }
    }

    if let Some(summary) = resize.as_mut()
        && summary.evidence.is_none()
        && let Some(evidence) = resize_evidence.get(&summary.event_idx)
    {
        summary.evidence = Some(evidence.clone());
    }

    let mut timeline_sorted = timeline;
    timeline_sorted.sort_by_key(|entry| entry.seq);
    let timeline = if timeline_sorted.len() > MAX_TIMELINE_ROWS * 2 {
        timeline_sorted
            .into_iter()
            .rev()
            .take(MAX_TIMELINE_ROWS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    } else {
        timeline_sorted
    };

    ParsedEvidence {
        diff,
        resize,
        budget,
        timeline,
        line_count: lines.len(),
        parsed_count,
    }
}

fn diff_from_value(value: &Value) -> Option<DiffSummary> {
    Some(DiffSummary {
        event_idx: value_u64(value, "event_idx")?,
        strategy: value_string(value, "strategy")?,
        posterior_mean: value_f64(value, "posterior_mean"),
        posterior_variance: value_f64(value, "posterior_variance"),
        alpha: value_f64(value, "alpha"),
        beta: value_f64(value, "beta"),
        guard_reason: value_string(value, "guard_reason"),
        fallback_reason: value_string(value, "fallback_reason"),
        hysteresis_applied: value_bool(value, "hysteresis_applied").unwrap_or(false),
        hysteresis_ratio: value_f64(value, "hysteresis_ratio"),
        dirty_rows: value_u64(value, "dirty_rows"),
        total_rows: value_u64(value, "total_rows"),
        dirty_tile_ratio: value_f64(value, "dirty_tile_ratio"),
        dirty_cell_ratio: value_f64(value, "dirty_cell_ratio"),
    })
}

fn resize_from_value(value: &Value) -> Option<ResizeSummary> {
    Some(ResizeSummary {
        event_idx: value_u64(value, "event_idx")?,
        action: value_string(value, "action")?,
        regime: value_string(value, "regime")?,
        dt_ms: value_f64(value, "dt_ms"),
        event_rate: value_f64(value, "event_rate"),
        time_since_render_ms: value_f64(value, "time_since_render_ms"),
        forced: value_bool(value, "forced").unwrap_or(false),
        evidence: None,
    })
}

fn resize_evidence_from_value(value: &Value) -> Option<(u64, ResizeEvidenceSummary)> {
    let event_idx = value_u64(value, "event_idx")?;
    let summary = ResizeEvidenceSummary {
        log_bayes_factor: value_f64(value, "log_bayes_factor")?,
        regime_contribution: value_f64(value, "regime_contribution").unwrap_or(0.0),
        timing_contribution: value_f64(value, "timing_contribution").unwrap_or(0.0),
        rate_contribution: value_f64(value, "rate_contribution").unwrap_or(0.0),
        explanation: value_string(value, "explanation").unwrap_or_else(|| "n/a".to_string()),
    };
    Some((event_idx, summary))
}

fn budget_from_value(value: &Value) -> Option<BudgetSummary> {
    Some(BudgetSummary {
        frame_idx: value_u64(value, "frame_idx")?,
        decision: value_string(value, "decision")?,
        controller_decision: value_string(value, "decision_controller"),
        degradation_before: value_string(value, "degradation_before"),
        degradation_after: value_string(value, "degradation_after"),
        frame_time_us: value_f64(value, "frame_time_us"),
        budget_us: value_f64(value, "budget_us"),
        e_value: value_f64(value, "e_value"),
        in_warmup: value_bool(value, "in_warmup"),
        conformal_alpha: value_f64(value, "alpha"),
        conformal_q_b: value_f64(value, "q_b"),
        conformal_upper_us: value_f64(value, "upper_us"),
        conformal_risk: value_bool(value, "risk"),
    })
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

fn value_f64(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_u64().map(|v| v as f64))
            .or_else(|| {
                v.as_str().and_then(|raw| {
                    let trimmed = raw.trim();
                    let lowered = trimmed.to_ascii_lowercase();
                    match lowered.as_str() {
                        "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
                        "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
                        _ => trimmed.parse::<f64>().ok(),
                    }
                })
            })
    })
}

fn value_u64(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|v| v as u64)))
}

fn value_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::MouseEvent;

    fn sample_lines() -> Vec<&'static str> {
        vec![
            r#"{"schema_version":"ftui-evidence-v1","event":"diff_decision","run_id":"diff-1","event_idx":4,"screen_mode":"alt","cols":80,"rows":24,"strategy":"dirty","posterior_mean":0.33,"posterior_variance":0.12,"alpha":1.2,"beta":2.3,"guard_reason":"","fallback_reason":"","hysteresis_applied":true,"hysteresis_ratio":1.1,"dirty_rows":5,"total_rows":24,"dirty_tile_ratio":0.07,"dirty_cell_ratio":0.08}"#,
            r#"{"schema_version":"ftui-evidence-v1","event":"decision_evidence","run_id":"resize-1","event_idx":7,"screen_mode":"alt","cols":80,"rows":24,"log_bayes_factor":1.23,"regime_contribution":0.5,"timing_contribution":0.3,"rate_contribution":0.2,"explanation":"burst regime"}"#,
            r#"{"schema_version":"ftui-evidence-v1","event":"decision","run_id":"resize-1","event_idx":7,"screen_mode":"alt","cols":80,"rows":24,"idx":7,"elapsed_ms":10.0,"dt_ms":5.0,"event_rate":20.0,"regime":"burst","action":"coalesce","pending_w":80,"pending_h":24,"applied_w":80,"applied_h":24,"time_since_render_ms":3.0,"coalesce_ms":12.0,"forced":false}"#,
            r#"{"event":"budget_decision","frame_idx":42,"decision":"degrade","decision_controller":"degrade","degradation_before":"full","degradation_after":"lite","frame_time_us":20000.0,"budget_us":16000.0,"pid_output":0.2,"pid_p":0.1,"pid_i":0.05,"pid_d":0.02,"e_value":0.4,"frames_observed":10,"frames_since_change":2,"in_warmup":false,"bucket_key":null,"n_b":null,"alpha":null,"q_b":null,"y_hat":null,"upper_us":null,"risk":null,"fallback_level":null,"window_size":null,"reset_count":null}"#,
        ]
    }

    #[test]
    fn parse_evidence_lines_maps_latest_entries() {
        let parsed = parse_evidence_lines(&sample_lines());
        let diff = parsed.diff.expect("diff summary");
        assert_eq!(diff.strategy, "dirty");
        assert_eq!(diff.event_idx, 4);

        let resize = parsed.resize.expect("resize summary");
        assert_eq!(resize.regime, "burst");
        assert_eq!(resize.action, "coalesce");
        assert!(resize.evidence.is_some());

        let budget = parsed.budget.expect("budget summary");
        assert_eq!(budget.decision, "degrade");
        assert_eq!(budget.frame_idx, 42);
    }

    #[test]
    fn empty_state_has_hint_lines() {
        let data = empty_data(SourceStatus {
            label: "source: (disabled)".to_string(),
            status: "Evidence source disabled".to_string(),
            hint_lines: default_hint_lines(None),
        });
        assert!(data.is_empty());
        assert!(!data.source.hint_lines.is_empty());
    }

    fn make_cockpit_with_timeline() -> ExplainabilityCockpit {
        let mut cockpit = ExplainabilityCockpit::with_evidence_path(None);
        let parsed = parse_evidence_lines(&sample_lines());
        cockpit.data = ExplainabilityData {
            source: SourceStatus {
                label: "test".to_string(),
                status: "ok".to_string(),
                hint_lines: vec![],
            },
            diff: parsed.diff,
            resize: parsed.resize,
            budget: parsed.budget,
            timeline: parsed.timeline,
        };
        cockpit
    }

    fn key_event(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c)))
    }

    fn arrow_event(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code))
    }

    fn mouse_click(x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            x,
            y,
        ))
    }

    fn mouse_scroll_up(x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent::new(MouseEventKind::ScrollUp, x, y))
    }

    fn mouse_scroll_down(x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent::new(MouseEventKind::ScrollDown, x, y))
    }

    #[test]
    fn default_focus_is_timeline() {
        let cockpit = ExplainabilityCockpit::with_evidence_path(None);
        assert_eq!(cockpit.focused_panel, FocusPanel::Timeline);
    }

    #[test]
    fn number_keys_change_focus() {
        let mut cockpit = make_cockpit_with_timeline();
        cockpit.update(&key_event('1'));
        assert_eq!(cockpit.focused_panel, FocusPanel::Diff);
        cockpit.update(&key_event('2'));
        assert_eq!(cockpit.focused_panel, FocusPanel::Resize);
        cockpit.update(&key_event('3'));
        assert_eq!(cockpit.focused_panel, FocusPanel::Budget);
        cockpit.update(&key_event('4'));
        assert_eq!(cockpit.focused_panel, FocusPanel::Timeline);
    }

    #[test]
    fn n_key_scrolls_timeline_up() {
        let mut cockpit = make_cockpit_with_timeline();
        assert_eq!(cockpit.timeline_scroll, 0);
        cockpit.update(&key_event('n'));
        assert_eq!(cockpit.timeline_scroll, 1);
        cockpit.update(&key_event('n'));
        assert_eq!(cockpit.timeline_scroll, 2);
    }

    #[test]
    fn p_key_scrolls_timeline_down() {
        let mut cockpit = make_cockpit_with_timeline();
        cockpit.timeline_scroll = 3;
        cockpit.update(&key_event('p'));
        assert_eq!(cockpit.timeline_scroll, 2);
        cockpit.update(&key_event('p'));
        assert_eq!(cockpit.timeline_scroll, 1);
    }

    #[test]
    fn p_key_clamps_at_zero() {
        let mut cockpit = make_cockpit_with_timeline();
        assert_eq!(cockpit.timeline_scroll, 0);
        cockpit.update(&key_event('p'));
        assert_eq!(cockpit.timeline_scroll, 0);
    }

    #[test]
    fn arrow_up_scrolls_timeline() {
        let mut cockpit = make_cockpit_with_timeline();
        cockpit.update(&arrow_event(KeyCode::Up));
        assert_eq!(cockpit.timeline_scroll, 1);
    }

    #[test]
    fn arrow_down_scrolls_timeline() {
        let mut cockpit = make_cockpit_with_timeline();
        cockpit.timeline_scroll = 2;
        cockpit.update(&arrow_event(KeyCode::Down));
        assert_eq!(cockpit.timeline_scroll, 1);
    }

    #[test]
    fn n_key_clamps_at_max() {
        let mut cockpit = make_cockpit_with_timeline();
        let max = cockpit.data.timeline.len().saturating_sub(1);
        for _ in 0..100 {
            cockpit.update(&key_event('n'));
        }
        assert_eq!(cockpit.timeline_scroll, max);
    }

    #[test]
    fn mouse_click_focuses_panel() {
        let mut cockpit = make_cockpit_with_timeline();
        // Simulate cached layout rects as if view() was called.
        cockpit.layout_diff.set(Rect::new(0, 3, 27, 6));
        cockpit.layout_resize.set(Rect::new(27, 3, 26, 6));
        cockpit.layout_budget.set(Rect::new(53, 3, 27, 6));
        cockpit.layout_timeline.set(Rect::new(0, 9, 80, 6));

        cockpit.update(&mouse_click(5, 5)); // Inside diff panel
        assert_eq!(cockpit.focused_panel, FocusPanel::Diff);

        cockpit.update(&mouse_click(30, 5)); // Inside resize panel
        assert_eq!(cockpit.focused_panel, FocusPanel::Resize);

        cockpit.update(&mouse_click(60, 5)); // Inside budget panel
        assert_eq!(cockpit.focused_panel, FocusPanel::Budget);

        cockpit.update(&mouse_click(40, 12)); // Inside timeline
        assert_eq!(cockpit.focused_panel, FocusPanel::Timeline);
    }

    #[test]
    fn mouse_click_outside_panels_no_change() {
        let mut cockpit = make_cockpit_with_timeline();
        cockpit.layout_diff.set(Rect::new(0, 3, 27, 6));
        cockpit.focused_panel = FocusPanel::Diff;

        // Click outside all cached rects (default Rect(0,0,0,0) for others).
        cockpit.update(&mouse_click(200, 200));
        assert_eq!(cockpit.focused_panel, FocusPanel::Diff);
    }

    #[test]
    fn mouse_scroll_up_on_timeline() {
        let mut cockpit = make_cockpit_with_timeline();
        cockpit.layout_timeline.set(Rect::new(0, 10, 80, 8));

        cockpit.update(&mouse_scroll_up(40, 14)); // Inside timeline
        assert_eq!(cockpit.timeline_scroll, 1);
        cockpit.update(&mouse_scroll_up(40, 14));
        assert_eq!(cockpit.timeline_scroll, 2);
    }

    #[test]
    fn mouse_scroll_down_on_timeline() {
        let mut cockpit = make_cockpit_with_timeline();
        cockpit.layout_timeline.set(Rect::new(0, 10, 80, 8));
        cockpit.timeline_scroll = 3;

        cockpit.update(&mouse_scroll_down(40, 14)); // Inside timeline
        assert_eq!(cockpit.timeline_scroll, 2);
    }

    #[test]
    fn mouse_scroll_outside_timeline_no_effect() {
        let mut cockpit = make_cockpit_with_timeline();
        cockpit.layout_timeline.set(Rect::new(0, 10, 80, 8));

        cockpit.update(&mouse_scroll_up(40, 5)); // Above timeline
        assert_eq!(cockpit.timeline_scroll, 0);
    }

    #[test]
    fn space_toggles_pause() {
        let mut cockpit = make_cockpit_with_timeline();
        assert!(!cockpit.paused);
        cockpit.update(&key_event(' '));
        assert!(cockpit.paused);
        cockpit.update(&key_event(' '));
        assert!(!cockpit.paused);
    }

    #[test]
    fn c_key_clears_evidence() {
        let mut cockpit = make_cockpit_with_timeline();
        assert!(!cockpit.data.timeline.is_empty());
        cockpit.update(&key_event('c'));
        assert!(cockpit.data.timeline.is_empty());
        assert!(cockpit.data.diff.is_none());
    }

    #[test]
    fn keybindings_returns_entries() {
        let cockpit = ExplainabilityCockpit::with_evidence_path(None);
        let bindings = cockpit.keybindings();
        assert!(bindings.len() >= 6);
        let keys: Vec<&str> = bindings.iter().map(|e| e.key).collect();
        assert!(keys.contains(&"r"));
        assert!(keys.contains(&"n/p"));
        assert!(keys.contains(&"1/2/3/4"));
    }

    #[test]
    fn tick_does_not_refresh_when_paused() {
        let mut cockpit = ExplainabilityCockpit::with_evidence_path(None);
        cockpit.paused = true;
        let initial_tick = cockpit.last_refresh_tick;
        cockpit.tick(100);
        assert_eq!(cockpit.last_refresh_tick, initial_tick);
    }
}
