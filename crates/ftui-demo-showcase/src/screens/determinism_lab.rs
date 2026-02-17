#![forbid(unsafe_code)]

//! Determinism Lab — checksum equivalence across diff strategies.
//!
//! Demonstrates:
//! - Full vs DirtyRows vs FullRedraw equivalence
//! - Per-frame change counts
//! - Mismatch detection (first coordinate + delta count)
//! - Deterministic checksum timeline
//! - JSONL export of verification reports

use std::cell::Cell as StdCell;
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;

use ftui_core::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::diff::BufferDiff;
use ftui_render::frame::Frame;
use ftui_render::frame::HitId;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::WrapMode;
use ftui_text::text::{Line, Span, Text};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::determinism;
use crate::theme;

const SCENE_WIDTH: u16 = 60;
const SCENE_HEIGHT: u16 = 18;
const HISTORY_LEN: usize = 12;
const DEFAULT_SEED: u64 = 7;

const RUNS_MAX: usize = 20;
const HIT_DETERMINISM_LAB_SCENARIO_BASE: u32 = 20_000;
const HIT_DETERMINISM_LAB_RUN_BASE: u32 = 20_100;

#[derive(Debug, Clone, Copy)]
struct Scenario {
    title: &'static str,
    frames: u64,
    seed_delta: i64,
    inject_fault: bool,
}

impl Scenario {
    const fn seed_for(self, base: u64) -> u64 {
        if self.seed_delta < 0 {
            base.saturating_sub(self.seed_delta.unsigned_abs())
        } else {
            base.saturating_add(self.seed_delta as u64)
        }
    }
}

const SCENARIOS: [Scenario; 3] = [
    Scenario {
        title: "Baseline (10f)",
        frames: 10,
        seed_delta: 0,
        inject_fault: false,
    },
    Scenario {
        title: "Drift (30f)",
        frames: 30,
        seed_delta: 1,
        inject_fault: false,
    },
    Scenario {
        title: "Fault Injection (1f)",
        frames: 1,
        seed_delta: 0,
        inject_fault: true,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrategyKind {
    Full,
    DirtyRows,
    FullRedraw,
}

impl StrategyKind {
    const ALL: [StrategyKind; 3] = [Self::Full, Self::DirtyRows, Self::FullRedraw];

    fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::DirtyRows => "DirtyRows",
            Self::FullRedraw => "FullRedraw",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::DirtyRows => "Dirty",
            Self::FullRedraw => "Redraw",
        }
    }

    fn key_hint(self) -> &'static str {
        match self {
            Self::Full => "1",
            Self::DirtyRows => "2",
            Self::FullRedraw => "3",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MismatchInfo {
    x: u16,
    y: u16,
    count: usize,
}

#[derive(Debug, Clone, Copy)]
struct StrategyResult {
    checksum: u64,
    change_count: usize,
    mismatch: Option<MismatchInfo>,
}

impl StrategyResult {
    fn status_label(self) -> &'static str {
        if self.mismatch.is_some() {
            "MISMATCH"
        } else {
            "OK"
        }
    }
}

#[derive(Debug, Clone)]
struct StrategyResults {
    full: StrategyResult,
    dirty: StrategyResult,
    redraw: StrategyResult,
}

impl StrategyResults {
    fn get(&self, strategy: StrategyKind) -> StrategyResult {
        match strategy {
            StrategyKind::Full => self.full,
            StrategyKind::DirtyRows => self.dirty,
            StrategyKind::FullRedraw => self.redraw,
        }
    }

    fn first_mismatch(&self) -> Option<(StrategyKind, MismatchInfo)> {
        if let Some(info) = self.full.mismatch {
            return Some((StrategyKind::Full, info));
        }
        if let Some(info) = self.dirty.mismatch {
            return Some((StrategyKind::DirtyRows, info));
        }
        if let Some(info) = self.redraw.mismatch {
            return Some((StrategyKind::FullRedraw, info));
        }
        None
    }
}

#[derive(Debug, Clone)]
struct ExportStatus {
    path: String,
    ok: bool,
    message: String,
}

#[derive(Debug, Clone)]
struct RunRecord {
    title: &'static str,
    seed: u64,
    frames: u64,
    inject_fault: bool,
    results: StrategyResults,
    first_mismatch: Option<(u64, StrategyKind, MismatchInfo)>,
    history_full: VecDeque<u64>,
    history_dirty: VecDeque<u64>,
    history_redraw: VecDeque<u64>,
}

impl RunRecord {
    fn status_label(&self) -> &'static str {
        if self.first_mismatch.is_some() {
            "MISMATCH"
        } else {
            "OK"
        }
    }
}

pub struct DeterminismLab {
    seed: u64,
    frame_index: u64,
    paused: bool,
    active_strategy: StrategyKind,
    inject_fault: bool,
    prev_buffer: Buffer,
    current_buffer: Buffer,
    results: StrategyResults,
    history_full: VecDeque<u64>,
    history_dirty: VecDeque<u64>,
    history_redraw: VecDeque<u64>,
    last_export: Option<ExportStatus>,
    export_path: String,
    selected_scenario: usize,
    runs: VecDeque<RunRecord>,
    selected_run: Option<usize>,
    details_scroll_y: u16,
    layout_scenario_rows: StdCell<Rect>,
    layout_run_rows: StdCell<Rect>,
    layout_details: StdCell<Rect>,
}

impl Default for DeterminismLab {
    fn default() -> Self {
        Self::new()
    }
}

impl DeterminismLab {
    pub fn new() -> Self {
        let base = Buffer::new(SCENE_WIDTH, SCENE_HEIGHT);
        let mut lab = Self {
            seed: determinism::demo_seed(DEFAULT_SEED),
            frame_index: 0,
            paused: false,
            active_strategy: StrategyKind::DirtyRows,
            inject_fault: false,
            prev_buffer: base.clone(),
            current_buffer: base,
            results: StrategyResults {
                full: StrategyResult {
                    checksum: 0,
                    change_count: 0,
                    mismatch: None,
                },
                dirty: StrategyResult {
                    checksum: 0,
                    change_count: 0,
                    mismatch: None,
                },
                redraw: StrategyResult {
                    checksum: 0,
                    change_count: 0,
                    mismatch: None,
                },
            },
            history_full: VecDeque::with_capacity(HISTORY_LEN),
            history_dirty: VecDeque::with_capacity(HISTORY_LEN),
            history_redraw: VecDeque::with_capacity(HISTORY_LEN),
            last_export: None,
            export_path: std::env::var("FTUI_DETERMINISM_LAB_REPORT")
                .unwrap_or_else(|_| "determinism_lab_report.jsonl".to_string()),
            selected_scenario: 0,
            runs: VecDeque::with_capacity(RUNS_MAX),
            selected_run: None,
            details_scroll_y: 0,
            layout_scenario_rows: StdCell::new(Rect::default()),
            layout_run_rows: StdCell::new(Rect::default()),
            layout_details: StdCell::new(Rect::default()),
        };

        lab.reset_scene();
        lab
    }

    fn reset_scene(&mut self) {
        let base = Buffer::new(SCENE_WIDTH, SCENE_HEIGHT);
        let next = self.generate_next_buffer(&base, self.seed, 0);
        self.prev_buffer = base;
        self.current_buffer = next;
        self.frame_index = 0;
        self.history_full.clear();
        self.history_dirty.clear();
        self.history_redraw.clear();
        let results =
            self.compute_results(&self.prev_buffer, &self.current_buffer, self.inject_fault);
        self.push_history(&results);
        self.results = results;
    }

    fn generate_next_buffer(&self, base: &Buffer, seed: u64, frame_index: u64) -> Buffer {
        let mut next = base.clone();
        next.clear_dirty();

        let mut state = seed ^ frame_index.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let width = next.width();
        let height = next.height();

        let row_count = 3 + (state % 2) as usize;
        let mut rows = Vec::with_capacity(row_count);
        for _ in 0..row_count {
            state = lcg_next(state);
            rows.push((state as u16) % height);
        }

        let cells_per_row = (width as usize / 3).clamp(6, 20);
        for (row_idx, &y) in rows.iter().enumerate() {
            for _ in 0..cells_per_row {
                state = lcg_next(state);
                let x = (state as u16) % width;
                let ch = char::from_u32(('A' as u32) + (state as u32 % 26)).unwrap_or('X');
                let fg = match (row_idx + (state as usize)) % 4 {
                    0 => theme::accent::INFO,
                    1 => theme::accent::SUCCESS,
                    2 => theme::accent::WARNING,
                    _ => theme::accent::ERROR,
                };
                let cell = Cell::from_char(ch).with_fg(fg.into());
                next.set_raw(x, y, cell);
            }
        }

        let cursor_x = ((frame_index * 5) % (width as u64)) as u16;
        let cursor_y = ((frame_index * 3) % (height as u64)) as u16;
        let cursor = Cell::from_char('O').with_fg(theme::accent::ACCENT_1.into());
        next.set_raw(cursor_x, cursor_y, cursor);

        next
    }

    fn advance_frame(&mut self) {
        if self.paused {
            return;
        }
        let next = self.generate_next_buffer(&self.current_buffer, self.seed, self.frame_index + 1);
        let results = self.compute_results(&self.current_buffer, &next, self.inject_fault);
        self.prev_buffer = self.current_buffer.clone();
        self.current_buffer = next;
        self.frame_index = self.frame_index.wrapping_add(1);
        self.push_history(&results);
        self.results = results;
    }

    fn compute_results(&self, prev: &Buffer, next: &Buffer, inject_fault: bool) -> StrategyResults {
        let full_diff = BufferDiff::compute(prev, next);
        let dirty_diff = BufferDiff::compute_dirty(prev, next);

        let full_applied = apply_diff(prev, next, &full_diff);
        let mut dirty_applied = apply_diff(prev, next, &dirty_diff);
        let redraw_applied = next.clone();

        if inject_fault {
            let fault_cell = Cell::from_char('!').with_fg(theme::accent::ERROR.into());
            dirty_applied.set_raw(0, 0, fault_cell);
        }

        let full_mismatch = compare_buffers(next, &full_applied);
        let dirty_mismatch = compare_buffers(next, &dirty_applied);
        let redraw_mismatch = compare_buffers(next, &redraw_applied);

        StrategyResults {
            full: StrategyResult {
                checksum: checksum_buffer(&full_applied),
                change_count: full_diff.len(),
                mismatch: full_mismatch,
            },
            dirty: StrategyResult {
                checksum: checksum_buffer(&dirty_applied),
                change_count: dirty_diff.len(),
                mismatch: dirty_mismatch,
            },
            redraw: StrategyResult {
                checksum: checksum_buffer(&redraw_applied),
                change_count: (next.width() as usize) * (next.height() as usize),
                mismatch: redraw_mismatch,
            },
        }
    }

    fn push_history(&mut self, results: &StrategyResults) {
        push_history(&mut self.history_full, results.full.checksum);
        push_history(&mut self.history_dirty, results.dirty.checksum);
        push_history(&mut self.history_redraw, results.redraw.checksum);
    }

    fn history_for(&self, strategy: StrategyKind) -> &VecDeque<u64> {
        match strategy {
            StrategyKind::Full => &self.history_full,
            StrategyKind::DirtyRows => &self.history_dirty,
            StrategyKind::FullRedraw => &self.history_redraw,
        }
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
    }

    fn bump_seed(&mut self, delta: i64) {
        if delta.is_negative() {
            self.seed = self.seed.saturating_sub(1);
        } else {
            self.seed = self.seed.saturating_add(1);
        }
        self.seed = self.seed.max(1);
        self.reset_scene();
    }

    fn toggle_fault(&mut self) {
        self.inject_fault = !self.inject_fault;
        self.results =
            self.compute_results(&self.prev_buffer, &self.current_buffer, self.inject_fault);
    }

    fn selected_run_record(&self) -> Option<&RunRecord> {
        self.selected_run.and_then(|idx| self.runs.get(idx))
    }

    fn clamp_details_scroll(&mut self) {
        let details = self.layout_details.get();
        if details.is_empty() {
            self.details_scroll_y = 0;
            return;
        }

        let line_count = self.build_details_lines().len();
        let max_scroll = line_count.saturating_sub(details.height as usize) as u16;
        self.details_scroll_y = self.details_scroll_y.min(max_scroll);
    }

    fn push_run(&mut self, record: RunRecord) {
        if self.runs.len() >= RUNS_MAX {
            self.runs.pop_front();
            self.selected_run = self.selected_run.and_then(|idx| idx.checked_sub(1));
        }

        self.runs.push_back(record);
        self.selected_run = Some(self.runs.len().saturating_sub(1));
        self.details_scroll_y = 0;
        self.clamp_details_scroll();
    }

    fn run_scenario(&mut self, scenario_idx: usize) {
        let scenario_idx = scenario_idx.min(SCENARIOS.len().saturating_sub(1));
        self.selected_scenario = scenario_idx;
        let scenario = SCENARIOS[scenario_idx];
        let seed = scenario.seed_for(self.seed);

        let record = self.simulate_run(scenario, seed);
        self.push_run(record);
    }

    fn run_all_scenarios(&mut self) {
        for (idx, _) in SCENARIOS.iter().enumerate() {
            self.run_scenario(idx);
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent, cmd: &mut Cmd<()>) {
        match mouse.kind {
            MouseEventKind::Up(MouseButton::Left) => {
                let scenarios = self.layout_scenario_rows.get();
                if scenarios.contains(mouse.x, mouse.y) {
                    let idx = (mouse.y - scenarios.y) as usize;
                    if idx < SCENARIOS.len() {
                        self.run_scenario(idx);
                        *cmd = Cmd::log(format!(
                            "[determinism] ran scenario: {}",
                            SCENARIOS[idx].title
                        ));
                        return;
                    }
                }

                let runs = self.layout_run_rows.get();
                if runs.contains(mouse.x, mouse.y) {
                    let row = (mouse.y - runs.y) as usize;
                    let visible = runs.height as usize;
                    let start = self.runs.len().saturating_sub(visible);
                    let idx = start + row;
                    if idx < self.runs.len() {
                        self.selected_run = Some(idx);
                        self.details_scroll_y = 0;
                        self.clamp_details_scroll();
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                let details = self.layout_details.get();
                if details.contains(mouse.x, mouse.y) {
                    self.details_scroll_y = self.details_scroll_y.saturating_sub(1);
                    self.clamp_details_scroll();
                }
            }
            MouseEventKind::ScrollDown => {
                let details = self.layout_details.get();
                if details.contains(mouse.x, mouse.y) {
                    self.details_scroll_y = self.details_scroll_y.saturating_add(1);
                    self.clamp_details_scroll();
                }
            }
            _ => {}
        }
    }

    fn simulate_run(&self, scenario: Scenario, seed: u64) -> RunRecord {
        let frames = scenario.frames.max(1);
        let base = Buffer::new(SCENE_WIDTH, SCENE_HEIGHT);
        let mut current = self.generate_next_buffer(&base, seed, 0);

        let mut history_full = VecDeque::with_capacity(HISTORY_LEN);
        let mut history_dirty = VecDeque::with_capacity(HISTORY_LEN);
        let mut history_redraw = VecDeque::with_capacity(HISTORY_LEN);

        let mut first_mismatch = None;
        let mut last_results = self.compute_results(&base, &current, scenario.inject_fault);
        push_history(&mut history_full, last_results.full.checksum);
        push_history(&mut history_dirty, last_results.dirty.checksum);
        push_history(&mut history_redraw, last_results.redraw.checksum);

        if let Some((strategy, info)) = last_results.first_mismatch() {
            first_mismatch = Some((0, strategy, info));
        }

        for frame in 1..frames {
            let next = self.generate_next_buffer(&current, seed, frame);
            last_results = self.compute_results(&current, &next, scenario.inject_fault);
            push_history(&mut history_full, last_results.full.checksum);
            push_history(&mut history_dirty, last_results.dirty.checksum);
            push_history(&mut history_redraw, last_results.redraw.checksum);

            if first_mismatch.is_none()
                && let Some((strategy, info)) = last_results.first_mismatch()
            {
                first_mismatch = Some((frame, strategy, info));
            }

            current = next;
        }

        RunRecord {
            title: scenario.title,
            seed,
            frames,
            inject_fault: scenario.inject_fault,
            results: last_results,
            first_mismatch,
            history_full,
            history_dirty,
            history_redraw,
        }
    }

    fn json_escape(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }

    fn export_report(&mut self) {
        let path = self.export_path.clone();
        let mut message = String::new();
        let mut ok = true;

        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut file) => {
                let env_json = determinism::demo_env_json();
                let hash_key = determinism::demo_hash_key(
                    self.current_buffer.width(),
                    self.current_buffer.height(),
                );
                let run_id = determinism::demo_run_id();
                let run_id_json = run_id
                    .as_ref()
                    .map(|value| format!("\"{}\"", Self::json_escape(value)))
                    .unwrap_or_else(|| "null".to_string());
                let env_line = format!(
                    "{{\"event\":\"determinism_env\",\"timestamp\":\"{}\",\"run_id\":{},\"hash_key\":\"{}\",\"seed\":{},\"width\":{},\"height\":{},\"env\":{}}}\n",
                    determinism::chrono_like_timestamp(),
                    run_id_json,
                    Self::json_escape(&hash_key),
                    self.seed,
                    self.current_buffer.width(),
                    self.current_buffer.height(),
                    env_json
                );
                if let Err(err) = file.write_all(env_line.as_bytes()) {
                    ok = false;
                    message = format!("env write failed: {err}");
                }
                if !ok {
                    self.last_export = Some(ExportStatus { path, ok, message });
                    return;
                }
                for strategy in StrategyKind::ALL {
                    let result = self.results.get(strategy);
                    let (mismatch_x, mismatch_y, mismatch_count) = match result.mismatch {
                        Some(info) => (info.x as i64, info.y as i64, info.count as i64),
                        None => (-1, -1, 0),
                    };
                    let timestamp = determinism::chrono_like_timestamp();
                    let line = format!(
                        "{{\"event\":\"determinism_report\",\"timestamp\":\"{}\",\"run_id\":{},\"hash_key\":\"{}\",\"frame\":{},\"seed\":{},\"width\":{},\"height\":{},\"strategy\":\"{}\",\"checksum\":\"0x{:016x}\",\"changes\":{},\"mismatch_count\":{},\"mismatch_x\":{},\"mismatch_y\":{}}}\n",
                        timestamp,
                        run_id_json,
                        Self::json_escape(&hash_key),
                        self.frame_index,
                        self.seed,
                        self.current_buffer.width(),
                        self.current_buffer.height(),
                        strategy.label(),
                        result.checksum,
                        result.change_count,
                        mismatch_count,
                        mismatch_x,
                        mismatch_y
                    );
                    if let Err(err) = file.write_all(line.as_bytes()) {
                        ok = false;
                        message = format!("write failed: {err}");
                        break;
                    }
                }
                if ok {
                    message = "report appended".to_string();
                }
            }
            Err(err) => {
                ok = false;
                message = format!("open failed: {err}");
            }
        }

        self.last_export = Some(ExportStatus { path, ok, message });
    }

    fn build_details_lines(&self) -> Vec<Line> {
        let muted = theme::muted();
        let accent = Style::new().fg(theme::screen_accent::PERFORMANCE).bold();
        let ok_style = Style::new().fg(theme::accent::SUCCESS);
        let warn_style = Style::new().fg(theme::accent::WARNING);
        let bad_style = Style::new().fg(theme::accent::ERROR).bold();

        let mut lines = Vec::new();

        let run = self.selected_run_record();
        if let Some(run) = run {
            lines.push(Line::from_spans(vec![
                Span::styled("Selected run", accent),
                Span::raw(": "),
                Span::raw(run.title),
            ]));
            lines.push(Line::from_spans(vec![
                Span::styled("Seed:", muted),
                Span::raw(format!(" {}", run.seed)),
                Span::raw("  "),
                Span::styled("Frames:", muted),
                Span::raw(format!(" {}", run.frames)),
                Span::raw("  "),
                Span::styled("Fault:", muted),
                Span::raw(if run.inject_fault { " ON" } else { " off" }),
            ]));

            if let Some((frame, strategy, info)) = run.first_mismatch {
                lines.push(Line::from_spans(vec![
                    Span::styled("First mismatch:", bad_style),
                    Span::raw(format!(
                        " frame {} ({}) @ ({},{}) count={}",
                        frame,
                        strategy.label(),
                        info.x,
                        info.y,
                        info.count
                    )),
                ]));
            } else {
                lines.push(Line::from_spans(vec![Span::styled(
                    "No mismatches detected.",
                    ok_style,
                )]));
            }

            lines.push(Line::from(""));
            for strategy in StrategyKind::ALL {
                let result = run.results.get(strategy);
                let status_style = if result.mismatch.is_some() {
                    bad_style
                } else {
                    ok_style
                };
                let history = match strategy {
                    StrategyKind::Full => &run.history_full,
                    StrategyKind::DirtyRows => &run.history_dirty,
                    StrategyKind::FullRedraw => &run.history_redraw,
                };

                lines.push(Line::from_spans(vec![
                    Span::styled(strategy.label(), accent),
                    Span::raw("  "),
                    Span::styled(result.status_label(), status_style),
                    Span::raw("  "),
                    Span::styled("changes=", muted),
                    Span::raw(format!("{}", result.change_count)),
                    Span::raw("  "),
                    Span::styled("checksum=", muted),
                    Span::raw(format!("0x{:016x}", result.checksum)),
                ]));
                lines.push(Line::from_spans(vec![
                    Span::styled("timeline:", muted),
                    Span::raw(" "),
                    Span::styled(format_history(history), warn_style),
                ]));
                lines.push(Line::from(""));
            }
        } else {
            lines.push(Line::from_spans(vec![
                Span::styled("No runs yet.", accent),
                Span::raw(" "),
                Span::styled("Click a scenario or press Enter.", muted),
            ]));
            lines.push(Line::from_spans(vec![
                Span::styled("Keys:", muted),
                Span::raw(" "),
                Span::styled("Enter/R", accent),
                Span::raw(" run selected  "),
                Span::styled("A", accent),
                Span::raw(" run all  "),
                Span::styled("C", accent),
                Span::raw(" log checksum"),
            ]));
        }

        lines
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let accent = Style::new().fg(theme::screen_accent::PERFORMANCE).bold();
        let muted = theme::muted();
        let status = if self.paused { "Paused" } else { "Live" };
        let fault = if self.inject_fault { "ON" } else { "off" };

        let line = Line::from_spans(vec![
            Span::styled("Determinism Lab", accent),
            Span::raw("  "),
            Span::styled("Seed: ", muted),
            Span::raw(format!("{}", self.seed)),
            Span::raw("  "),
            Span::styled("Frame: ", muted),
            Span::raw(format!("{}", self.frame_index)),
            Span::raw("  "),
            Span::styled("Status: ", muted),
            Span::raw(status),
            Span::raw("  "),
            Span::styled("Fault: ", muted),
            Span::raw(fault),
        ]);

        Paragraph::new(Text::from(line))
            .wrap(WrapMode::None)
            .render(area, frame);
    }

    fn render_results(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            self.layout_details.set(Rect::default());
            return;
        }

        let mut summary = Vec::new();
        let muted = theme::muted();
        let accent = Style::new().fg(theme::screen_accent::PERFORMANCE).bold();
        let ok_style = Style::new().fg(theme::accent::SUCCESS);
        let bad_style = Style::new().fg(theme::accent::ERROR).bold();

        let (results, history_full, history_dirty, history_redraw) =
            if let Some(run) = self.selected_run_record() {
                (
                    &run.results,
                    &run.history_full,
                    &run.history_dirty,
                    &run.history_redraw,
                )
            } else {
                (
                    &self.results,
                    &self.history_full,
                    &self.history_dirty,
                    &self.history_redraw,
                )
            };

        summary.push(Line::from_spans(vec![
            Span::styled("Strategy", accent),
            Span::raw("  "),
            Span::styled("Changes", accent),
            Span::raw("   "),
            Span::styled("Checksum", accent),
            Span::raw("             "),
            Span::styled("Status", accent),
        ]));

        for strategy in StrategyKind::ALL {
            let result = results.get(strategy);
            let active = strategy == self.active_strategy;
            let row_style = if active {
                Style::new().fg(theme::accent::INFO).bold()
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            };
            let status_style = if result.mismatch.is_some() {
                bad_style
            } else {
                ok_style
            };
            summary.push(Line::from_spans(vec![
                Span::styled(format!("{: <10}", strategy.label()), row_style),
                Span::raw("  "),
                Span::styled(format!("{: >6}", result.change_count), row_style),
                Span::raw("   "),
                Span::styled(format!("0x{:016x}", result.checksum), row_style),
                Span::raw("   "),
                Span::styled(result.status_label(), status_style),
            ]));
        }

        if let Some((strategy, info)) = results.first_mismatch() {
            summary.push(Line::from(""));
            summary.push(Line::from_spans(vec![
                Span::styled("Mismatch:", bad_style),
                Span::raw(" "),
                Span::styled(strategy.label(), bad_style),
                Span::raw(" "),
                Span::styled(format!("first at ({}, {})", info.x, info.y), bad_style),
                Span::raw(" "),
                Span::styled(format!("delta {}", info.count), bad_style),
            ]));
        }

        let active_history = match self.active_strategy {
            StrategyKind::Full => history_full,
            StrategyKind::DirtyRows => history_dirty,
            StrategyKind::FullRedraw => history_redraw,
        };

        summary.push(Line::from(""));
        summary.push(Line::from_spans(vec![
            Span::styled("Timeline:", muted),
            Span::raw(" "),
            Span::styled(
                format_history(active_history),
                Style::new().fg(theme::accent::ACCENT_2),
            ),
        ]));

        summary.push(Line::from(""));
        summary.push(Line::from_spans(vec![
            Span::styled("Scenario:", muted),
            Span::raw(" "),
            Span::styled(
                SCENARIOS[self
                    .selected_scenario
                    .min(SCENARIOS.len().saturating_sub(1))]
                .title,
                Style::new().fg(theme::accent::ACCENT_1).bold(),
            ),
        ]));

        summary.push(Line::from_spans(vec![
            Span::styled("Controls:", muted),
            Span::raw(" "),
            Span::raw("1/2/3 "),
            Span::styled("strategy", muted),
            Span::raw("  "),
            Span::raw("[/] "),
            Span::styled("seed", muted),
            Span::raw("  "),
            Span::raw("Space "),
            Span::styled("pause", muted),
            Span::raw("  "),
            Span::raw("F "),
            Span::styled("fault", muted),
            Span::raw("  "),
            Span::raw("E "),
            Span::styled("export", muted),
            Span::raw("  "),
            Span::raw("Enter/R "),
            Span::styled("run", muted),
            Span::raw("  "),
            Span::raw("A "),
            Span::styled("all", muted),
            Span::raw("  "),
            Span::raw("C "),
            Span::styled("checksum", muted),
            Span::raw("  "),
            Span::raw("X "),
            Span::styled("reset", muted),
        ]));

        if let Some(export) = &self.last_export {
            let export_style = if export.ok {
                Style::new().fg(theme::accent::SUCCESS)
            } else {
                Style::new().fg(theme::accent::ERROR)
            };
            summary.push(Line::from_spans(vec![
                Span::styled("Last export:", muted),
                Span::raw(" "),
                Span::styled(&export.path, export_style),
                Span::raw(" "),
                Span::styled(&export.message, muted),
            ]));
        }

        let details_lines = self.build_details_lines();

        let summary_height = summary.len() as u16;
        if summary_height >= area.height {
            self.layout_details.set(Rect::default());
            Paragraph::new(Text::from_lines(summary))
                .wrap(WrapMode::None)
                .render(area, frame);
            return;
        }

        let parts = Flex::vertical()
            .constraints([Constraint::Fixed(summary_height), Constraint::Fill])
            .split(area);

        Paragraph::new(Text::from_lines(summary))
            .wrap(WrapMode::None)
            .render(parts[0], frame);

        self.layout_details.set(parts[1]);
        Paragraph::new(Text::from_lines(details_lines))
            .wrap(WrapMode::None)
            .scroll((self.details_scroll_y, 0))
            .render(parts[1], frame);
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let max_w = area.width.min(self.current_buffer.width());
        let max_h = area.height.min(self.current_buffer.height());
        let mut lines = Vec::new();

        for y in 0..max_h {
            let mut row = String::with_capacity(max_w as usize);
            for x in 0..max_w {
                let ch = self
                    .current_buffer
                    .get(x, y)
                    .and_then(|cell| cell.content.as_char())
                    .unwrap_or('.');
                row.push(ch);
            }
            lines.push(Line::from(row));
        }

        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::None)
            .render(area, frame);
    }

    fn render_checks(&self, frame: &mut Frame, area: Rect) {
        self.layout_scenario_rows.set(Rect::default());
        self.layout_run_rows.set(Rect::default());

        if area.is_empty() {
            return;
        }

        let muted = theme::muted();
        let accent = Style::new().fg(theme::screen_accent::PERFORMANCE).bold();
        let ok_style = Style::new().fg(theme::accent::SUCCESS);
        let bad_style = Style::new().fg(theme::accent::ERROR).bold();

        // Scenarios at the top, runs below.
        let scenario_height = (SCENARIOS.len() as u16 + 1).min(area.height);
        let parts = Flex::vertical()
            .constraints([Constraint::Fixed(scenario_height), Constraint::Fill])
            .split(area);

        // Scenarios.
        let mut scenario_lines = Vec::new();
        scenario_lines.push(Line::from_spans(vec![
            Span::styled("Scenarios", accent),
            Span::raw("  "),
            Span::styled("(click/Enter)", muted),
            Span::raw("  "),
            Span::styled("A", accent),
            Span::raw(" all"),
        ]));

        let scenario_rows = parts[0].height.saturating_sub(1);
        let scenario_rows_rect =
            Rect::new(parts[0].x, parts[0].y + 1, parts[0].width, scenario_rows);
        self.layout_scenario_rows.set(scenario_rows_rect);

        for (idx, scenario) in SCENARIOS.iter().enumerate() {
            if idx as u16 >= scenario_rows {
                break;
            }
            let is_selected = idx == self.selected_scenario;
            let row_style = if is_selected {
                Style::new().fg(theme::accent::INFO).bold()
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            };

            scenario_lines.push(Line::from_spans(vec![
                Span::styled(if is_selected { ">" } else { " " }, row_style),
                Span::raw(" "),
                Span::styled(scenario.title, row_style),
            ]));

            let row_rect = Rect::new(
                scenario_rows_rect.x,
                scenario_rows_rect.y + idx as u16,
                scenario_rows_rect.width,
                1,
            );
            frame.register_hit_region(
                row_rect,
                HitId::new(HIT_DETERMINISM_LAB_SCENARIO_BASE + idx as u32),
            );
        }

        Paragraph::new(Text::from_lines(scenario_lines))
            .wrap(WrapMode::None)
            .render(parts[0], frame);

        // Runs.
        if parts[1].is_empty() {
            return;
        }

        let mut run_lines = Vec::new();
        run_lines.push(Line::from_spans(vec![
            Span::styled("Runs", accent),
            Span::raw("  "),
            Span::styled("(click row to inspect)", muted),
        ]));

        let run_rows = parts[1].height.saturating_sub(1);
        let run_rows_rect = Rect::new(parts[1].x, parts[1].y + 1, parts[1].width, run_rows);
        self.layout_run_rows.set(run_rows_rect);

        let visible = run_rows as usize;
        let start = self.runs.len().saturating_sub(visible);

        for row in 0..run_rows as usize {
            let idx = start + row;
            if idx >= self.runs.len() {
                break;
            }
            let run = &self.runs[idx];
            let is_selected = self.selected_run == Some(idx);

            let status_style = if run.first_mismatch.is_some() {
                bad_style
            } else {
                ok_style
            };
            let row_style = if is_selected {
                Style::new().fg(theme::accent::ACCENT_1).bold()
            } else {
                Style::new().fg(theme::fg::PRIMARY)
            };

            run_lines.push(Line::from_spans(vec![
                Span::styled(if is_selected { ">" } else { " " }, row_style),
                Span::raw(" "),
                Span::styled(run.status_label(), status_style),
                Span::raw(" "),
                Span::styled(run.title, row_style),
            ]));

            let row_rect = Rect::new(
                run_rows_rect.x,
                run_rows_rect.y + row as u16,
                run_rows_rect.width,
                1,
            );
            frame.register_hit_region(
                row_rect,
                HitId::new(HIT_DETERMINISM_LAB_RUN_BASE + row as u32),
            );
        }

        Paragraph::new(Text::from_lines(run_lines))
            .wrap(WrapMode::None)
            .render(parts[1], frame);
    }
}

impl Screen for DeterminismLab {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        let mut cmd = Cmd::none();

        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('1') => self.active_strategy = StrategyKind::Full,
                KeyCode::Char('2') => self.active_strategy = StrategyKind::DirtyRows,
                KeyCode::Char('3') => self.active_strategy = StrategyKind::FullRedraw,
                KeyCode::Char(' ') => self.toggle_pause(),
                KeyCode::Char('[') => self.bump_seed(-1),
                KeyCode::Char(']') => self.bump_seed(1),
                KeyCode::Char('f') | KeyCode::Char('F') => self.toggle_fault(),
                KeyCode::Char('e') | KeyCode::Char('E') => self.export_report(),
                KeyCode::Char('x') | KeyCode::Char('X') => self.reset_scene(),
                KeyCode::Char('a') | KeyCode::Char('A') => self.run_all_scenarios(),
                KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => {
                    self.run_scenario(self.selected_scenario)
                }
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    let (checksum, label) = if let Some(run) = self.selected_run_record() {
                        (run.results.get(self.active_strategy).checksum, run.title)
                    } else {
                        (self.results.get(self.active_strategy).checksum, "live")
                    };
                    cmd = Cmd::log(format!(
                        "[determinism] checksum ({}) {} = 0x{:016x}",
                        self.active_strategy.label(),
                        label,
                        checksum
                    ));
                }
                _ => {}
            },
            Event::Mouse(mouse) => self.handle_mouse(mouse, &mut cmd),
            Event::Resize { .. } => self.clamp_details_scroll(),
            _ => {}
        }

        cmd
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(2), Constraint::Fill])
            .split(area);

        self.render_header(frame, rows[0]);

        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(rows[1]);

        let left = Block::new()
            .title("Equivalence")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .style(theme::panel_border_style(
                false,
                theme::screen_accent::PERFORMANCE,
            ));
        let left_inner = left.inner(cols[0]);
        left.render(cols[0], frame);
        self.render_results(frame, left_inner);

        let right_rows = Flex::vertical()
            .constraints([Constraint::Percentage(55.0), Constraint::Fill])
            .split(cols[1]);

        let preview = Block::new()
            .title("Scene Preview")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .style(theme::panel_border_style(
                false,
                theme::screen_accent::PERFORMANCE,
            ));
        let preview_inner = preview.inner(right_rows[0]);
        preview.render(right_rows[0], frame);
        self.render_preview(frame, preview_inner);

        let checks = Block::new()
            .title("Checks")
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .style(theme::panel_border_style(
                false,
                theme::screen_accent::PERFORMANCE,
            ));
        let checks_inner = checks.inner(right_rows[1]);
        checks.render(right_rows[1], frame);
        self.render_checks(frame, checks_inner);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "1/2/3",
                action: "Select Full / DirtyRows / FullRedraw strategy",
            },
            HelpEntry {
                key: "[ / ]",
                action: "Decrease / increase seed",
            },
            HelpEntry {
                key: "Space",
                action: "Pause / resume updates",
            },
            HelpEntry {
                key: "F",
                action: "Toggle fault injection",
            },
            HelpEntry {
                key: "E",
                action: "Export JSONL verification report",
            },
            HelpEntry {
                key: "Enter / R",
                action: "Run selected scenario (appends a run record)",
            },
            HelpEntry {
                key: "A",
                action: "Run all scenarios",
            },
            HelpEntry {
                key: "C",
                action: "Log active checksum for selected run (or live state)",
            },
            HelpEntry {
                key: "X",
                action: "Reset live scene + timeline",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click scenario to run; click run row to inspect; wheel scrolls details",
            },
        ]
    }

    fn tick(&mut self, _tick_count: u64) {
        self.advance_frame();
    }

    fn title(&self) -> &'static str {
        "Determinism Lab"
    }

    fn tab_label(&self) -> &'static str {
        "Determinism"
    }
}

fn lcg_next(state: u64) -> u64 {
    state.wrapping_mul(6364136223846793005).wrapping_add(1)
}

fn apply_diff(base: &Buffer, target: &Buffer, diff: &BufferDiff) -> Buffer {
    let mut result = base.clone();
    for (x, y) in diff.iter() {
        let cell = *target.get_unchecked(x, y);
        result.set_raw(x, y, cell);
    }
    result
}

fn compare_buffers(expected: &Buffer, actual: &Buffer) -> Option<MismatchInfo> {
    let width = expected.width().min(actual.width());
    let height = expected.height().min(actual.height());
    let mut count = 0usize;
    let mut first = None;

    for y in 0..height {
        for x in 0..width {
            let a = expected.get_unchecked(x, y);
            let b = actual.get_unchecked(x, y);
            if !a.bits_eq(b) {
                count += 1;
                if first.is_none() {
                    first = Some((x, y));
                }
            }
        }
    }

    first.map(|(x, y)| MismatchInfo { x, y, count })
}

fn checksum_buffer(buf: &Buffer) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            if let Some(cell) = buf.get(x, y) {
                hash = fnv1a64_u32(hash, cell.content.raw());
                hash = fnv1a64_u32(hash, cell.fg.0);
                hash = fnv1a64_u32(hash, cell.bg.0);
                hash = fnv1a64_u32(hash, pack_attrs(cell.attrs));
            }
        }
    }
    hash
}

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv1a64_u32(hash: u64, v: u32) -> u64 {
    fnv1a64_bytes(hash, &v.to_le_bytes())
}

fn fnv1a64_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn pack_attrs(attrs: ftui_render::cell::CellAttrs) -> u32 {
    let flags = attrs.flags().bits() as u32;
    let link = attrs.link_id() & 0x00FF_FFFF;
    (flags << 24) | link
}

fn push_history(history: &mut VecDeque<u64>, checksum: u64) {
    if history.len() >= HISTORY_LEN {
        history.pop_front();
    }
    history.push_back(checksum);
}

fn format_history(history: &VecDeque<u64>) -> String {
    if history.is_empty() {
        return "n/a".to_string();
    }
    let mut parts = Vec::with_capacity(history.len());
    for checksum in history.iter() {
        parts.push(format!("{:06x}", checksum & 0x00FF_FFFF));
    }
    parts.join(" ")
}
