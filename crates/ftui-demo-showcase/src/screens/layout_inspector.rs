#![forbid(unsafe_code)]

//! Layout Inspector screen — visualize constraints, computed rects, and solver steps.

use std::cell::Cell;

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::{Line, Span, Text};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::constraint_overlay::{ConstraintOverlay, ConstraintOverlayStyle};
use ftui_widgets::layout_debugger::{LayoutConstraints, LayoutDebugger, LayoutRecord};
use ftui_widgets::paragraph::Paragraph;

use super::layout_lab::LayoutLab;
use super::{HelpEntry, Screen};
use crate::theme;

const SCENARIO_COUNT: usize = 3;
const STEP_COUNT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Constraints,
    Allocation,
    Final,
}

impl StepKind {
    fn from_index(idx: usize) -> Self {
        match idx % STEP_COUNT {
            0 => Self::Constraints,
            1 => Self::Allocation,
            _ => Self::Final,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Constraints => "Constraints",
            Self::Allocation => "Allocation",
            Self::Final => "Final",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Self::Constraints => "Inspect min/max bounds and requested sizes.",
            Self::Allocation => "See solver allocation vs requested outlines.",
            Self::Final => "Verify final rects + overflow/underflow flags.",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ScenarioSpec {
    name: &'static str,
    description: &'static str,
}

const SCENARIOS: [ScenarioSpec; SCENARIO_COUNT] = [
    ScenarioSpec {
        name: "Flex Trio",
        description: "Vertical flex: Fixed + Min + Max",
    },
    ScenarioSpec {
        name: "Tight Grid",
        description: "2x2 grid with intentional constraint pressure",
    },
    ScenarioSpec {
        name: "FitContent Clamp",
        description: "FitContent bounded by min/max",
    },
];

pub struct LayoutInspector {
    scenario_idx: usize,
    step_idx: usize,
    show_overlay: bool,
    show_tree: bool,
    layout_info: Cell<Rect>,
    layout_viz: Cell<Rect>,
    layout_tree: Cell<Rect>,
    pane_workspace: LayoutLab,
    pane_workspace_visible: Cell<bool>,
}

impl Default for LayoutInspector {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutInspector {
    pub fn new() -> Self {
        Self {
            scenario_idx: 0,
            step_idx: 0,
            show_overlay: true,
            show_tree: true,
            layout_info: Cell::new(Rect::default()),
            layout_viz: Cell::new(Rect::default()),
            layout_tree: Cell::new(Rect::default()),
            pane_workspace: LayoutLab::new(),
            pane_workspace_visible: Cell::new(false),
        }
    }

    fn current_step(&self) -> StepKind {
        StepKind::from_index(self.step_idx)
    }

    fn overlay_style(&self) -> ConstraintOverlayStyle {
        let mut style = ConstraintOverlayStyle::default();
        match self.current_step() {
            StepKind::Constraints => {
                style.show_borders = false;
                style.show_size_diff = true;
                style.show_labels = true;
            }
            StepKind::Allocation => {
                style.show_borders = true;
                style.show_size_diff = true;
                style.show_labels = true;
            }
            StepKind::Final => {
                style.show_borders = true;
                style.show_size_diff = false;
                style.show_labels = true;
            }
        }
        style
    }

    fn step_hint_line(&self) -> Line {
        let step = self.current_step();
        Line::from_spans([
            Span::styled("Step:", Style::new().fg(theme::fg::SECONDARY)),
            Span::raw(" "),
            Span::styled(step.label(), Style::new().fg(theme::accent::INFO)),
            Span::raw(" - "),
            Span::styled(step.blurb(), Style::new().fg(theme::fg::MUTED)),
        ])
    }

    fn info_lines(&self, _record: &LayoutRecord) -> Vec<Line> {
        let scenario = SCENARIOS[self.scenario_idx];
        let mut lines = Vec::new();
        lines.push(Line::from_spans([
            Span::styled("Scenario:", Style::new().fg(theme::fg::SECONDARY)),
            Span::raw(" "),
            Span::styled(scenario.name, Style::new().fg(theme::fg::PRIMARY)),
        ]));
        lines.push(Line::from_spans([
            Span::styled("Details:", Style::new().fg(theme::fg::SECONDARY)),
            Span::raw(" "),
            Span::styled(scenario.description, Style::new().fg(theme::fg::MUTED)),
        ]));
        lines.push(self.step_hint_line());
        lines.push(Line::raw(""));
        lines.push(Line::from_spans([
            Span::styled("Overlay:", Style::new().fg(theme::fg::SECONDARY)),
            Span::raw(" "),
            Span::styled(
                if self.show_overlay { "on" } else { "off" },
                Style::new().fg(theme::accent::INFO),
            ),
            Span::raw("   "),
            Span::styled("Tree:", Style::new().fg(theme::fg::SECONDARY)),
            Span::raw(" "),
            Span::styled(
                if self.show_tree { "on" } else { "off" },
                Style::new().fg(theme::accent::INFO),
            ),
        ]));
        lines.push(Line::raw(""));
        lines.push(Line::from_spans([
            Span::styled("Keys:", Style::new().fg(theme::fg::SECONDARY)),
            Span::raw(" n/p "),
            Span::styled("scenario", Style::new().fg(theme::fg::MUTED)),
            Span::raw("  [/ ] "),
            Span::styled("step", Style::new().fg(theme::fg::MUTED)),
            Span::raw("  o "),
            Span::styled("overlay", Style::new().fg(theme::fg::MUTED)),
            Span::raw("  t "),
            Span::styled("tree", Style::new().fg(theme::fg::MUTED)),
            Span::raw("  r "),
            Span::styled("reset", Style::new().fg(theme::fg::MUTED)),
        ]));
        lines.push(Line::raw(""));
        lines.push(Line::from_spans([
            Span::styled("Records", Style::new().fg(theme::accent::INFO)),
            Span::raw(": constraints vs rects"),
        ]));
        lines
    }

    fn record_table(&self, record: &LayoutRecord) -> Vec<Line> {
        let mut lines = Vec::new();
        self.push_record_lines(record, 0, &mut lines);
        lines
    }

    fn push_record_lines(&self, record: &LayoutRecord, depth: usize, lines: &mut Vec<Line>) {
        let indent = "  ".repeat(depth);
        let constraints = record.constraints;
        let received = record.area_received;
        let overflow = (constraints.max_width != 0 && received.width > constraints.max_width)
            || (constraints.max_height != 0 && received.height > constraints.max_height);
        let underflow =
            received.width < constraints.min_width || received.height < constraints.min_height;
        let status_style = if overflow {
            Style::new().fg(theme::accent::ERROR)
        } else if underflow {
            Style::new().fg(theme::accent::WARNING)
        } else {
            Style::new().fg(theme::accent::SUCCESS)
        };
        let status_label = if overflow {
            "OVER"
        } else if underflow {
            "UNDER"
        } else {
            "OK"
        };
        let line = Line::from_spans([
            Span::raw(indent),
            Span::styled(
                record.widget_name.as_str(),
                Style::new().fg(theme::fg::PRIMARY),
            ),
            Span::raw("  req "),
            Span::styled(
                format!(
                    "{}x{}",
                    record.area_requested.width, record.area_requested.height
                ),
                Style::new().fg(theme::fg::SECONDARY),
            ),
            Span::raw("  got "),
            Span::styled(
                format!("{}x{}", received.width, received.height),
                Style::new().fg(theme::fg::SECONDARY),
            ),
            Span::raw("  min "),
            Span::styled(
                format!("{}x{}", constraints.min_width, constraints.min_height),
                Style::new().fg(theme::fg::MUTED),
            ),
            Span::raw("  max "),
            Span::styled(
                format!("{}x{}", constraints.max_width, constraints.max_height),
                Style::new().fg(theme::fg::MUTED),
            ),
            Span::raw("  "),
            Span::styled(status_label, status_style),
        ]);
        lines.push(line);
        for child in &record.children {
            self.push_record_lines(child, depth + 1, lines);
        }
    }

    fn build_scenario(&self, area: Rect) -> ScenarioRender {
        match self.scenario_idx % SCENARIO_COUNT {
            0 => self.build_flex_trio(area),
            1 => self.build_tight_grid(area),
            _ => self.build_fit_content(area),
        }
    }

    fn build_flex_trio(&self, area: Rect) -> ScenarioRender {
        let constraints = [Constraint::Fixed(3), Constraint::Min(4), Constraint::Max(6)];
        let rows = Flex::vertical().constraints(constraints).gap(1).split(area);

        let mut blocks = Vec::new();
        let accents = [
            theme::accent::ACCENT_1,
            theme::accent::ACCENT_4,
            theme::accent::ACCENT_6,
        ];
        for (idx, rect) in rows.iter().enumerate() {
            blocks.push(BlockSpec {
                rect: *rect,
                title: match idx {
                    0 => "Fixed(3)",
                    1 => "Min(4)",
                    _ => "Max(6)",
                },
                accent: accents[idx],
            });
        }

        let root = LayoutRecord::new("FlexRoot", area, area, LayoutConstraints::new(0, 0, 0, 0))
            .with_child(self.make_record("Fixed", rows[0], 3, 3, true))
            .with_child(self.make_record("Min", rows[1], 4, 0, true))
            .with_child(self.make_record("Max", rows[2], 0, 6, true));

        ScenarioRender { root, blocks }
    }

    fn build_tight_grid(&self, area: Rect) -> ScenarioRender {
        let rows = Flex::vertical()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .gap(1)
            .split(area);
        let top = rows[0];
        let bottom = rows[1];
        let top_cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .gap(1)
            .split(top);
        let bottom_cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .gap(1)
            .split(bottom);

        let cells = [top_cols[0], top_cols[1], bottom_cols[0], bottom_cols[1]];
        let labels = ["A", "B", "C", "D"];
        let accents = [
            theme::accent::ACCENT_2,
            theme::accent::ACCENT_3,
            theme::accent::ACCENT_5,
            theme::accent::ACCENT_7,
        ];
        let mut blocks = Vec::new();
        for (i, rect) in cells.iter().enumerate() {
            blocks.push(BlockSpec {
                rect: *rect,
                title: labels[i],
                accent: accents[i],
            });
        }

        let mut root =
            LayoutRecord::new("GridRoot", area, area, LayoutConstraints::new(0, 0, 0, 0));
        for (i, rect) in cells.iter().enumerate() {
            let min = rect.width.saturating_add(2);
            let max = rect.width.saturating_sub(2);
            let max_width = if i % 2 == 0 { max } else { 0 };
            let min_width = if i % 2 == 1 { min } else { 0 };
            let min_height = if i < 2 {
                rect.height.saturating_add(1)
            } else {
                0
            };
            let max_height = if i >= 2 {
                rect.height.saturating_sub(1)
            } else {
                0
            };
            let record = LayoutRecord::new(
                format!("Cell {label}", label = labels[i]),
                self.request_rect(*rect, rect.width.saturating_add(2), false),
                *rect,
                LayoutConstraints::new(min_width, max_width, min_height, max_height),
            );
            root = root.with_child(record);
        }

        ScenarioRender { root, blocks }
    }

    fn build_fit_content(&self, area: Rect) -> ScenarioRender {
        let cols = Flex::horizontal()
            .constraints([
                Constraint::FitContentBounded { min: 12, max: 18 },
                Constraint::Min(10),
            ])
            .gap(2)
            .split(area);

        let blocks = vec![
            BlockSpec {
                rect: cols[0],
                title: "FitContent(12..18)",
                accent: theme::accent::ACCENT_9,
            },
            BlockSpec {
                rect: cols[1],
                title: "Min(10)",
                accent: theme::accent::ACCENT_11,
            },
        ];

        let root = LayoutRecord::new("FitRoot", area, area, LayoutConstraints::new(0, 0, 0, 0))
            .with_child(self.make_record("FitContent", cols[0], 12, 18, false))
            .with_child(self.make_record("Min", cols[1], 10, 0, false));

        ScenarioRender { root, blocks }
    }

    fn make_record(
        &self,
        name: &str,
        received: Rect,
        min_main: u16,
        max_main: u16,
        vertical: bool,
    ) -> LayoutRecord {
        let requested_main = if max_main > 0 {
            max_main
        } else {
            min_main.max(1)
        };
        let (min_width, max_width, min_height, max_height) = if vertical {
            (0, 0, min_main, max_main)
        } else {
            (min_main, max_main, 0, 0)
        };
        let requested = self.request_rect(received, requested_main, vertical);
        LayoutRecord::new(
            name,
            requested,
            received,
            LayoutConstraints::new(min_width, max_width, min_height, max_height),
        )
    }

    fn request_rect(&self, received: Rect, requested_main: u16, vertical: bool) -> Rect {
        if vertical {
            Rect::new(received.x, received.y, received.width, requested_main)
        } else {
            Rect::new(received.x, received.y, requested_main, received.height)
        }
    }

    fn render_blocks(&self, frame: &mut Frame, blocks: &[BlockSpec]) {
        for block in blocks {
            if block.rect.is_empty() {
                continue;
            }
            let style = Style::new().fg(block.accent).bg(theme::alpha::SURFACE);
            let frame_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(block.title)
                .title_alignment(Alignment::Center)
                .style(style);
            frame_block.render(block.rect, frame);
        }
    }
}

impl Screen for LayoutInspector {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return Cmd::None;
            }
            self.pane_workspace.cancel_embedded_pane_workspace_drag();
            match (key.code, key.modifiers) {
                (KeyCode::Char('n'), Modifiers::NONE) => {
                    self.scenario_idx = (self.scenario_idx + 1) % SCENARIO_COUNT;
                }
                (KeyCode::Char('p'), Modifiers::NONE) => {
                    self.scenario_idx = (self.scenario_idx + SCENARIO_COUNT - 1) % SCENARIO_COUNT;
                }
                (KeyCode::Char(']'), Modifiers::NONE) | (KeyCode::Right, Modifiers::NONE) => {
                    self.step_idx = (self.step_idx + 1) % STEP_COUNT;
                }
                (KeyCode::Char('['), Modifiers::NONE) | (KeyCode::Left, Modifiers::NONE) => {
                    self.step_idx = (self.step_idx + STEP_COUNT - 1) % STEP_COUNT;
                }
                (KeyCode::Char('o'), Modifiers::NONE) => {
                    self.show_overlay = !self.show_overlay;
                }
                (KeyCode::Char('t'), Modifiers::NONE) => {
                    self.show_tree = !self.show_tree;
                }
                (KeyCode::Char('r'), Modifiers::NONE) => {
                    self.step_idx = 0;
                }
                _ => {}
            }
        }
        if let Event::Mouse(mouse) = event {
            let (x, y) = (mouse.x, mouse.y);
            if !self.pane_workspace_visible.get() {
                self.pane_workspace.cancel_embedded_pane_workspace_drag();
                self.pane_workspace.clear_embedded_pane_workspace_bounds();
            } else if self.pane_workspace.pane_workspace_wants_mouse(x, y) {
                self.pane_workspace.update_embedded_pane_workspace_mouse(
                    mouse.kind,
                    x,
                    y,
                    mouse.modifiers,
                );
                return Cmd::None;
            }
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if self.layout_info.get().contains(x, y) {
                        self.scenario_idx = (self.scenario_idx + 1) % SCENARIO_COUNT;
                    } else if self.layout_tree.get().contains(x, y) {
                        self.show_tree = !self.show_tree;
                    } else if self.layout_viz.get().contains(x, y) {
                        self.step_idx = (self.step_idx + 1) % STEP_COUNT;
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    if self.layout_viz.get().contains(x, y) {
                        self.show_overlay = !self.show_overlay;
                    }
                }
                MouseEventKind::ScrollDown => {
                    self.scenario_idx = (self.scenario_idx + 1) % SCENARIO_COUNT;
                }
                MouseEventKind::ScrollUp => {
                    self.scenario_idx = (self.scenario_idx + SCENARIO_COUNT - 1) % SCENARIO_COUNT;
                }
                _ => {}
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(36.0), Constraint::Percentage(64.0)])
            .gap(theme::spacing::SM)
            .split(area);
        let info_area = cols[0];
        let viz_host = cols[1];
        let (viz_area, pane_area) = if viz_host.width >= 52 && viz_host.height >= 14 {
            let split = Flex::horizontal()
                .constraints([Constraint::Percentage(64.0), Constraint::Percentage(36.0)])
                .gap(theme::spacing::SM)
                .split(viz_host);
            (split[0], Some(split[1]))
        } else {
            (viz_host, None)
        };
        self.layout_info.set(info_area);
        self.layout_viz.set(viz_area);

        if viz_area.is_empty() {
            self.pane_workspace_visible.set(false);
            self.pane_workspace.clear_embedded_pane_workspace_bounds();
            return;
        }

        let viz_rows = if self.show_tree {
            Flex::vertical()
                .constraints([Constraint::Percentage(68.0), Constraint::Percentage(32.0)])
                .gap(theme::spacing::XS)
                .split(viz_area)
        } else {
            vec![viz_area]
        };

        let main_viz = viz_rows[0];
        let viz_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Layout")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY));
        let viz_inner = viz_block.inner(main_viz);

        let render = self.build_scenario(viz_inner);
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        debugger.record(render.root.clone());

        let info_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Inspector")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY));
        let info_inner = info_block.inner(info_area);
        info_block.render(info_area, frame);

        if !info_inner.is_empty() {
            let rows = Flex::vertical()
                .constraints([Constraint::Fixed(7), Constraint::Min(0)])
                .gap(theme::spacing::XS)
                .split(info_inner);
            let summary_lines = self.info_lines(&render.root);
            Paragraph::new(Text::from_lines(summary_lines))
                .style(Style::new().fg(theme::fg::PRIMARY))
                .render(rows[0], frame);
            let table_lines = self.record_table(&render.root);
            Paragraph::new(Text::from_lines(table_lines))
                .style(Style::new().fg(theme::fg::PRIMARY))
                .render(rows[1], frame);
        }

        viz_block.render(main_viz, frame);
        if !viz_inner.is_empty() {
            self.render_blocks(frame, &render.blocks);
            if self.show_overlay {
                ConstraintOverlay::new(&debugger)
                    .style(self.overlay_style())
                    .render(viz_inner, frame);
            }
        }

        if self.show_tree && viz_rows.len() > 1 {
            let tree_area = viz_rows[1];
            self.layout_tree.set(tree_area);
            let tree_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title("Tree")
                .title_alignment(Alignment::Center)
                .style(Style::new().fg(theme::fg::PRIMARY));
            let tree_inner = tree_block.inner(tree_area);
            tree_block.render(tree_area, frame);
            if !tree_inner.is_empty() {
                debugger.render_debug(tree_inner, &mut frame.buffer);
            }
        } else {
            self.layout_tree.set(Rect::default());
        }

        if let Some(pane_area) = pane_area {
            if !pane_area.is_empty() {
                self.pane_workspace_visible.set(true);
                self.pane_workspace
                    .render_embedded_pane_workspace(frame, pane_area);
            } else {
                self.pane_workspace_visible.set(false);
                self.pane_workspace.clear_embedded_pane_workspace_bounds();
            }
        } else {
            self.pane_workspace_visible.set(false);
            self.pane_workspace.clear_embedded_pane_workspace_bounds();
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "n/p",
                action: "Next/prev scenario",
            },
            HelpEntry {
                key: "[/]",
                action: "Previous/next step",
            },
            HelpEntry {
                key: "o",
                action: "Toggle overlay",
            },
            HelpEntry {
                key: "t",
                action: "Toggle tree panel",
            },
            HelpEntry {
                key: "r",
                action: "Reset step",
            },
            HelpEntry {
                key: "\u{2190}/\u{2192}",
                action: "Previous/next step",
            },
            HelpEntry {
                key: "Click info",
                action: "Next scenario",
            },
            HelpEntry {
                key: "Click viz",
                action: "Next step",
            },
            HelpEntry {
                key: "Scroll",
                action: "Cycle scenarios",
            },
            HelpEntry {
                key: "Pane rail",
                action: "Drag pane studio",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Layout Inspector"
    }

    fn tab_label(&self) -> &'static str {
        "Layout"
    }
}

#[derive(Clone)]
struct BlockSpec {
    rect: Rect,
    title: &'static str,
    accent: theme::ColorToken,
}

#[derive(Clone)]
struct ScenarioRender {
    root: LayoutRecord,
    blocks: Vec<BlockSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyEvent, MouseEvent};
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn layout_inspector_cycles_scenarios() {
        let mut screen = LayoutInspector::new();
        let next = Event::Key(KeyEvent {
            code: KeyCode::Char('n'),
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        screen.update(&next);
        assert_eq!(screen.scenario_idx, 1);

        let prev = Event::Key(KeyEvent {
            code: KeyCode::Char('p'),
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        screen.update(&prev);
        assert_eq!(screen.scenario_idx, 0);
    }

    #[test]
    fn layout_inspector_steps_wrap() {
        let mut screen = LayoutInspector::new();
        let next = Event::Key(KeyEvent {
            code: KeyCode::Char(']'),
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        for _ in 0..STEP_COUNT {
            screen.update(&next);
        }
        assert_eq!(screen.step_idx, 0);
    }

    #[test]
    fn layout_inspector_builds_expected_children() {
        let mut screen = LayoutInspector::new();
        let area = Rect::new(0, 0, 60, 20);

        let render = screen.build_scenario(area);
        assert_eq!(render.root.children.len(), 3);

        screen.scenario_idx = 1;
        let render = screen.build_scenario(area);
        assert_eq!(render.root.children.len(), 4);

        screen.scenario_idx = 2;
        let render = screen.build_scenario(area);
        assert_eq!(render.root.children.len(), 2);
    }

    fn render_screen(screen: &LayoutInspector) {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 80, 24));
    }

    fn render_wide_screen(screen: &LayoutInspector) {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 28, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 120, 28));
    }

    fn mouse_click(x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    fn mouse_right_click(x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    fn mouse_scroll_down(x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    fn mouse_scroll_up(x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    #[test]
    fn click_info_panel_cycles_scenario() {
        let mut screen = LayoutInspector::new();
        render_screen(&screen);
        let info = screen.layout_info.get();
        assert!(!info.is_empty(), "info layout rect should be populated");
        let cx = info.x + info.width / 2;
        let cy = info.y + info.height / 2;
        screen.update(&mouse_click(cx, cy));
        assert_eq!(screen.scenario_idx, 1);
    }

    #[test]
    fn click_viz_panel_cycles_step() {
        let mut screen = LayoutInspector::new();
        render_screen(&screen);
        let viz = screen.layout_viz.get();
        assert!(!viz.is_empty(), "viz layout rect should be populated");
        let cx = viz.x + viz.width / 2;
        let cy = viz.y + viz.height / 2;
        screen.update(&mouse_click(cx, cy));
        assert_eq!(screen.step_idx, 1);
    }

    #[test]
    fn right_click_viz_toggles_overlay() {
        let mut screen = LayoutInspector::new();
        render_screen(&screen);
        assert!(screen.show_overlay);
        let viz = screen.layout_viz.get();
        let cx = viz.x + viz.width / 2;
        let cy = viz.y + viz.height / 2;
        screen.update(&mouse_right_click(cx, cy));
        assert!(!screen.show_overlay);
        screen.update(&mouse_right_click(cx, cy));
        assert!(screen.show_overlay);
    }

    #[test]
    fn scroll_cycles_scenarios() {
        let mut screen = LayoutInspector::new();
        screen.update(&mouse_scroll_down(40, 12));
        assert_eq!(screen.scenario_idx, 1);
        screen.update(&mouse_scroll_up(40, 12));
        assert_eq!(screen.scenario_idx, 0);
        screen.update(&mouse_scroll_up(40, 12));
        assert_eq!(screen.scenario_idx, SCENARIO_COUNT - 1);
    }

    #[test]
    fn click_tree_panel_toggles_tree() {
        let mut screen = LayoutInspector::new();
        render_screen(&screen);
        assert!(screen.show_tree);
        let tree = screen.layout_tree.get();
        assert!(!tree.is_empty(), "tree layout rect should be populated");
        let cx = tree.x + tree.width / 2;
        let cy = tree.y + tree.height / 2;
        screen.update(&mouse_click(cx, cy));
        assert!(!screen.show_tree);
    }

    #[test]
    fn layout_inspector_embeds_pane_workspace_when_wide() {
        let screen = LayoutInspector::new();
        render_wide_screen(&screen);
        assert!(
            !screen
                .pane_workspace
                .embedded_pane_workspace_bounds()
                .is_empty(),
            "wide layout should render embedded pane workspace"
        );
    }

    #[test]
    fn pane_workspace_click_does_not_cycle_step() {
        let mut screen = LayoutInspector::new();
        render_wide_screen(&screen);
        let pane = screen.pane_workspace.embedded_pane_workspace_bounds();
        assert!(!pane.is_empty(), "pane workspace should be visible");
        let step_before = screen.step_idx;
        screen.update(&mouse_click(
            pane.x + pane.width / 2,
            pane.y + pane.height / 2,
        ));
        assert_eq!(
            screen.step_idx, step_before,
            "pane workspace click should not trigger inspector step cycling"
        );
    }

    #[test]
    fn keybindings_include_mouse_hints() {
        let screen = LayoutInspector::new();
        let bindings = screen.keybindings();
        assert!(
            bindings.len() >= 8,
            "should have at least 8 keybinding entries"
        );
        let keys: Vec<&str> = bindings.iter().map(|b| b.key).collect();
        assert!(keys.contains(&"Click info"));
        assert!(keys.contains(&"Click viz"));
        assert!(keys.contains(&"Scroll"));
    }

    #[test]
    fn render_no_panic_standard_area() {
        let screen = LayoutInspector::new();
        render_screen(&screen);
    }

    #[test]
    fn render_no_panic_empty_area() {
        let screen = LayoutInspector::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn toggle_overlay_keyboard() {
        let mut screen = LayoutInspector::new();
        assert!(screen.show_overlay);
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('o'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&event);
        assert!(!screen.show_overlay);
        screen.update(&event);
        assert!(screen.show_overlay);
    }

    #[test]
    fn toggle_tree_keyboard() {
        let mut screen = LayoutInspector::new();
        assert!(screen.show_tree);
        let event = Event::Key(KeyEvent {
            code: KeyCode::Char('t'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&event);
        assert!(!screen.show_tree);
    }

    #[test]
    fn reset_step_keyboard() {
        let mut screen = LayoutInspector::new();
        let step_event = Event::Key(KeyEvent {
            code: KeyCode::Char(']'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&step_event);
        assert_eq!(screen.step_idx, 1);
        let reset_event = Event::Key(KeyEvent {
            code: KeyCode::Char('r'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        });
        screen.update(&reset_event);
        assert_eq!(screen.step_idx, 0);
    }

    #[test]
    fn click_outside_panels_no_change() {
        let mut screen = LayoutInspector::new();
        render_screen(&screen);
        screen.update(&mouse_click(0, 0));
        assert_eq!(screen.scenario_idx, 0);
        assert_eq!(screen.step_idx, 0);
    }

    #[test]
    fn tree_hidden_clears_layout_rect() {
        let mut screen = LayoutInspector::new();
        render_screen(&screen);
        assert!(!screen.layout_tree.get().is_empty());
        screen.show_tree = false;
        render_screen(&screen);
        assert!(screen.layout_tree.get().is_empty());
    }
}
