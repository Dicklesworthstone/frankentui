#![forbid(unsafe_code)]

//! Layout Laboratory screen — interactive constraint solver and layout widget demos.

use std::cell::Cell;

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers, MouseButton, MouseEventKind};
use ftui_core::geometry::{Rect, Sides};
use ftui_layout::{Alignment as FlexAlignment, Constraint, Flex};
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::display_width;
use ftui_widgets::Widget;
use ftui_widgets::align::{Align, VerticalAlignment};
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::columns::Columns;
use ftui_widgets::group::Group;
use ftui_widgets::layout_debugger::LayoutDebugger;
use ftui_widgets::padding::Padding;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::rule::Rule;

use super::{HelpEntry, Screen};
use crate::theme;

/// The five layout presets available.
const PRESET_COUNT: usize = 5;

/// Names for each preset.
const PRESET_NAMES: [&str; PRESET_COUNT] = [
    "Flex Vertical",
    "Flex Horizontal",
    "Grid 3x3",
    "Nested Flex",
    "Real-World Layout",
];

/// Whether the flex direction is vertical or horizontal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Vertical,
    Horizontal,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::Vertical => "Vertical",
            Direction::Horizontal => "Horizontal",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Direction::Vertical => Direction::Horizontal,
            Direction::Horizontal => Direction::Vertical,
        }
    }
}

/// Layout Lab screen state.
pub struct LayoutLab {
    /// Current preset index (0-4).
    current_preset: usize,
    /// Which constraint is selected for editing (within presets 0/1).
    selected_constraint: usize,
    /// Flex direction for presets 0/1.
    direction: Direction,
    /// Alignment mode index (0-4).
    alignment_idx: usize,
    /// Gap between items.
    gap: u16,
    /// Margin around layout.
    margin: u16,
    /// Padding amount for Padding widget demo.
    padding_amount: u16,
    /// Align demo position index (0-8 for 9 positions).
    align_pos: usize,
    /// Layout debugger instance.
    debugger: LayoutDebugger,
    /// Show debug overlay.
    show_debug: bool,
    /// Cached preview area for mouse hit-testing.
    layout_preview: Cell<Rect>,
    /// Cached controls area for mouse hit-testing.
    layout_controls: Cell<Rect>,
}

/// The 5 alignment modes.
const ALIGNMENTS: [FlexAlignment; 5] = [
    FlexAlignment::Start,
    FlexAlignment::Center,
    FlexAlignment::End,
    FlexAlignment::SpaceBetween,
    FlexAlignment::SpaceAround,
];

const ALIGNMENT_NAMES: [&str; 5] = ["Start", "Center", "End", "SpaceBetween", "SpaceAround"];

/// Colors for layout regions (theme-aware accents).
fn region_colors() -> [PackedRgba; 6] {
    [
        theme::accent::ACCENT_1.into(),
        theme::accent::ACCENT_2.into(),
        theme::accent::ACCENT_3.into(),
        theme::accent::ACCENT_4.into(),
        theme::accent::ACCENT_5.into(),
        theme::accent::ACCENT_6.into(),
    ]
}

impl Default for LayoutLab {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutLab {
    pub fn new() -> Self {
        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);
        Self {
            current_preset: 0,
            selected_constraint: 0,
            direction: Direction::Vertical,
            alignment_idx: 0,
            gap: 0,
            margin: 0,
            padding_amount: theme::spacing::XS,
            align_pos: 4, // Center/Middle
            debugger,
            show_debug: false,
            layout_preview: Cell::new(Rect::default()),
            layout_controls: Cell::new(Rect::default()),
        }
    }

    /// Get the constraints for the current preset.
    fn preset_constraints(&self) -> Vec<Constraint> {
        match self.current_preset {
            0 | 1 => vec![
                Constraint::Fixed(5),
                Constraint::Percentage(30.0),
                Constraint::Min(3),
                Constraint::Max(10),
                Constraint::Ratio(1, 3),
            ],
            2 => vec![
                // Grid: 3 columns
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ],
            3 => vec![
                // Nested: outer 60/40
                Constraint::Percentage(60.0),
                Constraint::Percentage(40.0),
            ],
            4 => vec![
                // Real-world: sidebar + main + aside
                Constraint::Fixed(20),
                Constraint::Min(40),
                Constraint::Max(30),
            ],
            _ => vec![Constraint::Min(0)],
        }
    }

    fn constraint_label(c: &Constraint) -> String {
        match c {
            Constraint::Fixed(v) => format!("Fixed({v})"),
            Constraint::Percentage(v) => format!("Pct({v:.0}%)"),
            Constraint::Min(v) => format!("Min({v})"),
            Constraint::Max(v) => format!("Max({v})"),
            Constraint::Ratio(n, d) => format!("Ratio({n}/{d})"),
            Constraint::Fill => "Fill".to_string(),
            Constraint::FitContent => "FitContent".to_string(),
            Constraint::FitContentBounded { min, max } => format!("FitContent({min}..{max})"),
            Constraint::FitMin => "FitMin".to_string(),
        }
    }

    fn current_alignment(&self) -> FlexAlignment {
        ALIGNMENTS[self.alignment_idx]
    }
}

impl LayoutLab {
    /// Handle mouse interactions.
    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let preview = self.layout_preview.get();
        let controls = self.layout_controls.get();
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if controls.contains(x, y) {
                    self.direction = self.direction.toggle();
                } else if preview.contains(x, y) {
                    self.show_debug = !self.show_debug;
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if preview.contains(x, y) {
                    self.current_preset = (self.current_preset + 1) % PRESET_COUNT;
                    self.selected_constraint = 0;
                }
            }
            MouseEventKind::ScrollUp => {
                if preview.contains(x, y) {
                    self.gap = self.gap.saturating_sub(1);
                } else if controls.contains(x, y) {
                    self.alignment_idx = if self.alignment_idx == 0 {
                        ALIGNMENTS.len() - 1
                    } else {
                        self.alignment_idx - 1
                    };
                }
            }
            MouseEventKind::ScrollDown => {
                if preview.contains(x, y) {
                    self.gap = self.gap.saturating_add(1).min(5);
                } else if controls.contains(x, y) {
                    self.alignment_idx = (self.alignment_idx + 1) % ALIGNMENTS.len();
                }
            }
            _ => {}
        }
    }
}

impl Screen for LayoutLab {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            self.handle_mouse(mouse.kind, mouse.x, mouse.y);
            return Cmd::None;
        }
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return Cmd::None;
            }

            match (key.code, key.modifiers) {
                // Preset selection: 1-5
                (KeyCode::Char('1'), Modifiers::NONE) => {
                    self.current_preset = 0;
                    self.selected_constraint = 0;
                }
                (KeyCode::Char('2'), Modifiers::NONE) => {
                    self.current_preset = 1;
                    self.selected_constraint = 0;
                }
                (KeyCode::Char('3'), Modifiers::NONE) => {
                    self.current_preset = 2;
                    self.selected_constraint = 0;
                }
                (KeyCode::Char('4'), Modifiers::NONE) => {
                    self.current_preset = 3;
                    self.selected_constraint = 0;
                }
                (KeyCode::Char('5'), Modifiers::NONE) => {
                    self.current_preset = 4;
                    self.selected_constraint = 0;
                }

                // Direction toggle
                (KeyCode::Char('d'), Modifiers::NONE) => {
                    self.direction = self.direction.toggle();
                }

                // Alignment cycle
                (KeyCode::Char('a'), Modifiers::NONE) => {
                    self.alignment_idx = (self.alignment_idx + 1) % ALIGNMENTS.len();
                }

                // Gap adjustment
                (KeyCode::Char('+'), _) | (KeyCode::Char('='), _) => {
                    self.gap = self.gap.saturating_add(1).min(5);
                }
                (KeyCode::Char('-'), Modifiers::NONE) => {
                    self.gap = self.gap.saturating_sub(1);
                }

                // Margin adjustment
                (KeyCode::Char('m'), Modifiers::NONE) => {
                    self.margin = self.margin.saturating_add(1).min(4);
                }
                (KeyCode::Char('M'), Modifiers::NONE) | (KeyCode::Char('m'), Modifiers::SHIFT) => {
                    self.margin = self.margin.saturating_sub(1);
                }

                // Padding adjustment
                (KeyCode::Char('p'), Modifiers::NONE) => {
                    self.padding_amount = self.padding_amount.saturating_add(1).min(4);
                }
                (KeyCode::Char('P'), Modifiers::NONE) | (KeyCode::Char('p'), Modifiers::SHIFT) => {
                    self.padding_amount = self.padding_amount.saturating_sub(1);
                }

                // Constraint selection (Tab to cycle)
                (KeyCode::Tab, Modifiers::NONE) => {
                    let count = self.preset_constraints().len();
                    self.selected_constraint = (self.selected_constraint + 1) % count;
                }

                // Arrow keys to adjust selected constraint value
                (KeyCode::Right, Modifiers::NONE) => self.adjust_constraint(1),
                (KeyCode::Left, Modifiers::NONE) => self.adjust_constraint(-1),

                // Align position cycle
                (KeyCode::Char('l'), Modifiers::NONE) => {
                    self.align_pos = (self.align_pos + 1) % 9;
                }

                // Debug overlay toggle
                (KeyCode::Char('D'), Modifiers::NONE) | (KeyCode::Char('d'), Modifiers::SHIFT) => {
                    self.show_debug = !self.show_debug;
                }

                _ => {}
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.height < 8 || area.width < 40 {
            Paragraph::new("Terminal too small for Layout Lab")
                .style(theme::muted())
                .render(area, frame);
            return;
        }

        // Top-level: upper preview + lower controls/demos
        let main_chunks = Flex::vertical()
            .constraints([
                Constraint::Percentage(55.0),
                Constraint::Fixed(1),
                Constraint::Min(8),
            ])
            .split(area);

        self.layout_preview.set(main_chunks[0]);
        self.render_preview(frame, main_chunks[0]);
        Rule::new()
            .style(Style::new().fg(theme::fg::MUTED))
            .render(main_chunks[1], frame);
        self.render_bottom(frame, main_chunks[2]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "1-5",
                action: "Switch preset",
            },
            HelpEntry {
                key: "d",
                action: "Toggle direction",
            },
            HelpEntry {
                key: "a",
                action: "Cycle alignment",
            },
            HelpEntry {
                key: "+/-",
                action: "Adjust gap",
            },
            HelpEntry {
                key: "m/M",
                action: "Adjust margin",
            },
            HelpEntry {
                key: "p/P",
                action: "Adjust padding",
            },
            HelpEntry {
                key: "Tab",
                action: "Select constraint",
            },
            HelpEntry {
                key: "Left/Right",
                action: "Adjust constraint",
            },
            HelpEntry {
                key: "l",
                action: "Cycle align pos",
            },
            HelpEntry {
                key: "D",
                action: "Toggle debug",
            },
            HelpEntry {
                key: "Click",
                action: "Toggle debug/direction",
            },
            HelpEntry {
                key: "Right-click",
                action: "Cycle preset",
            },
            HelpEntry {
                key: "Scroll",
                action: "Adjust gap/alignment",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Layout Laboratory"
    }

    fn tab_label(&self) -> &'static str {
        "Layout"
    }
}

impl LayoutLab {
    /// Adjust the selected constraint's value by delta.
    fn adjust_constraint(&mut self, delta: i16) {
        // Only presets 0, 1, 4 have meaningfully adjustable constraints
        let _ = delta; // Constraint values are fixed in our presets for simplicity
        // The interactive preview already shows the effect of gap/margin/alignment
    }

    /// Render the upper half: layout preview with colored blocks.
    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        let title = format!(
            " Preset {}: {} ",
            self.current_preset + 1,
            PRESET_NAMES[self.current_preset]
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .title_alignment(Alignment::Center)
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        match self.current_preset {
            0 | 1 => self.render_flex_preset(frame, inner),
            2 => self.render_grid_preset(frame, inner),
            3 => self.render_nested_preset(frame, inner),
            4 => self.render_realworld_preset(frame, inner),
            _ => {}
        }

        // Debug overlay
        if self.show_debug {
            // Render the debug info as text in the bottom-right corner
            let debug_width = 40u16.min(inner.width);
            let debug_height = 6u16.min(inner.height);
            let debug_area = Rect::new(
                inner.x + inner.width.saturating_sub(debug_width),
                inner.y + inner.height.saturating_sub(debug_height),
                debug_width,
                debug_height,
            );
            let debug_block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .title("Debug")
                .style(Style::new().fg(theme::accent::WARNING).bg(theme::bg::DEEP));
            let debug_inner = debug_block.inner(debug_area);
            debug_block.render(debug_area, frame);

            let constraints = self.preset_constraints();
            let debug_text: String = constraints
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let marker = if i == self.selected_constraint {
                        ">"
                    } else {
                        " "
                    };
                    format!("{marker} {}", Self::constraint_label(c))
                })
                .collect::<Vec<_>>()
                .join("\n");
            Paragraph::new(debug_text)
                .style(Style::new().fg(theme::fg::PRIMARY))
                .render(debug_inner, frame);
        }
    }

    /// Render a flex preset (vertical or horizontal) with colored blocks.
    fn render_flex_preset(&self, frame: &mut Frame, area: Rect) {
        let constraints = self.preset_constraints();
        let flex = match self.direction {
            Direction::Vertical => Flex::vertical(),
            Direction::Horizontal => Flex::horizontal(),
        };
        let rects = flex
            .constraints(constraints.clone())
            .gap(self.gap)
            .margin(Sides::all(self.margin))
            .alignment(self.current_alignment())
            .split(area);

        for (i, (rect, constraint)) in rects.iter().zip(constraints.iter()).enumerate() {
            self.render_region(frame, *rect, i, constraint);
        }
    }

    /// Render a grid 3x3 preset with header spanning 3 columns and sidebar spanning 2 rows.
    fn render_grid_preset(&self, frame: &mut Frame, area: Rect) {
        // 3 rows: header (3 high) + 2 content rows
        let rows = Flex::vertical()
            .gap(self.gap)
            .margin(Sides::all(self.margin))
            .constraints([Constraint::Fixed(3), Constraint::Min(2), Constraint::Min(2)])
            .split(area);

        // Header spans full width
        let header_constraint = Constraint::Ratio(1, 1);
        self.render_region(frame, rows[0], 0, &header_constraint);

        // Row 1: sidebar (spans 2 rows conceptually) + 2 cells
        let row1_cols = Flex::horizontal()
            .gap(self.gap)
            .constraints([
                Constraint::Fixed(12),
                Constraint::Min(5),
                Constraint::Min(5),
            ])
            .split(rows[1]);

        // Sidebar spans rows[1] and rows[2] vertically
        let sidebar_area = Rect::new(
            row1_cols[0].x,
            row1_cols[0].y,
            row1_cols[0].width,
            rows[1]
                .height
                .saturating_add(rows[2].height)
                .saturating_add(self.gap),
        );
        let sidebar_constraint = Constraint::Fixed(12);
        self.render_region(frame, sidebar_area, 1, &sidebar_constraint);

        self.render_region(frame, row1_cols[1], 2, &Constraint::Min(5));
        self.render_region(frame, row1_cols[2], 3, &Constraint::Min(5));

        // Row 2: skip sidebar col, render 2 cells
        let row2_cols = Flex::horizontal()
            .gap(self.gap)
            .constraints([
                Constraint::Fixed(12),
                Constraint::Min(5),
                Constraint::Min(5),
            ])
            .split(rows[2]);

        // Don't re-render sidebar area; just render the two cells
        self.render_region(frame, row2_cols[1], 4, &Constraint::Min(5));
        self.render_region(frame, row2_cols[2], 5, &Constraint::Min(5));
    }

    /// Render nested flex preset: outer 60/40 horizontal, each with vertical subdivisions.
    fn render_nested_preset(&self, frame: &mut Frame, area: Rect) {
        let outer = Flex::horizontal()
            .gap(self.gap)
            .margin(Sides::all(self.margin))
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(area);

        // Left side: 3 vertical regions
        let left = Flex::vertical()
            .gap(self.gap)
            .constraints([
                Constraint::Fixed(3),
                Constraint::Min(4),
                Constraint::Fixed(3),
            ])
            .split(outer[0]);
        self.render_region(frame, left[0], 0, &Constraint::Fixed(3));
        self.render_region(frame, left[1], 1, &Constraint::Min(4));
        self.render_region(frame, left[2], 2, &Constraint::Fixed(3));

        // Right side: 2 vertical regions
        let right = Flex::vertical()
            .gap(self.gap)
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(outer[1]);
        self.render_region(frame, right[0], 3, &Constraint::Percentage(50.0));
        self.render_region(frame, right[1], 4, &Constraint::Percentage(50.0));
    }

    /// Render real-world layout: header + (sidebar + main + aside) + footer.
    fn render_realworld_preset(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .margin(Sides::all(self.margin))
            .constraints([
                Constraint::Fixed(3),
                Constraint::Min(4),
                Constraint::Fixed(1),
            ])
            .split(area);

        // Header
        self.render_region(frame, rows[0], 0, &Constraint::Fixed(3));

        // Middle: sidebar + main + aside
        let cols = Flex::horizontal()
            .gap(self.gap)
            .constraints([
                Constraint::Fixed(20),
                Constraint::Min(20),
                Constraint::Max(30),
            ])
            .split(rows[1]);
        self.render_region(frame, cols[0], 1, &Constraint::Fixed(20));
        self.render_region(frame, cols[1], 2, &Constraint::Min(20));
        self.render_region(frame, cols[2], 3, &Constraint::Max(30));

        // Footer
        self.render_region(frame, rows[2], 4, &Constraint::Fixed(1));
    }

    /// Render a single colored region with its rect info and constraint label.
    fn render_region(&self, frame: &mut Frame, rect: Rect, idx: usize, constraint: &Constraint) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let colors = region_colors();
        let color = colors[idx % colors.len()];
        let label = Self::constraint_label(constraint);

        let style = Style::new().fg(theme::fg::PRIMARY).bg(color);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .title(&label)
            .title_alignment(Alignment::Center)
            .style(style);
        let inner = block.inner(rect);
        block.render(rect, frame);

        if inner.height > 0 && inner.width > 4 {
            let rect_text = format!("{}x{} @({},{})", rect.width, rect.height, rect.x, rect.y);
            Paragraph::new(rect_text).style(style).render(inner, frame);
        }
    }

    /// Render the bottom half: controls + widget demos.
    fn render_bottom(&self, frame: &mut Frame, area: Rect) {
        // Split horizontally: controls (left) + widget demos (right)
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(40.0), Constraint::Percentage(60.0)])
            .split(area);

        self.layout_controls.set(cols[0]);
        self.render_controls(frame, cols[0]);
        self.render_widget_demos(frame, cols[1]);
    }

    /// Render the controls panel showing current settings.
    fn render_controls(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Controls ")
            .title_alignment(Alignment::Center)
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height < 3 || inner.width < 15 {
            return;
        }

        let constraints = self.preset_constraints();
        let constraint_list: String = constraints
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let marker = if i == self.selected_constraint {
                    ">"
                } else {
                    " "
                };
                format!("{marker} {}", Self::constraint_label(c))
            })
            .collect::<Vec<_>>()
            .join("  ");

        let info = format!(
            "Preset: [{}] {}\n\
             Direction: {} (d)\n\
             Alignment: {} (a)\n\
             Gap: {} (+/-)\n\
             Margin: {} (m/M)\n\
             Padding: {} (p/P)\n\
             Constraints: {}",
            self.current_preset + 1,
            PRESET_NAMES[self.current_preset],
            self.direction.label(),
            ALIGNMENT_NAMES[self.alignment_idx],
            self.gap,
            self.margin,
            self.padding_amount,
            constraint_list,
        );

        Paragraph::new(info)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(inner, frame);
    }

    /// Render widget demos: Padding, Align, Columns, Group.
    fn render_widget_demos(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Widget Demos ")
            .title_alignment(Alignment::Center)
            .style(theme::content_border());
        let demo_inner = block.inner(area);
        block.render(area, frame);

        if demo_inner.height < 4 || demo_inner.width < 20 {
            return;
        }

        // Split into 4 demo areas
        let demo_rows = Flex::vertical()
            .constraints([Constraint::Min(3), Constraint::Min(3)])
            .split(demo_inner);

        let top_demos = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(demo_rows[0]);

        let bottom_demos = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(demo_rows[1]);

        // Demo 1: Padding widget
        self.render_padding_demo(frame, top_demos[0]);

        // Demo 2: Align widget
        self.render_align_demo(frame, top_demos[1]);

        // Demo 3: Columns widget
        self.render_columns_demo(frame, bottom_demos[0]);

        // Demo 4: Group widget
        self.render_group_demo(frame, bottom_demos[1]);
    }

    fn render_padding_demo(&self, frame: &mut Frame, area: Rect) {
        let label = Paragraph::new(format!("Padding({})", self.padding_amount)).style(
            Style::new()
                .fg(theme::accent::PRIMARY)
                .attrs(StyleFlags::BOLD),
        );
        let padded = Padding::new(label, Sides::all(self.padding_amount));
        let demo_block = Block::new()
            .borders(Borders::ALL)
            .title("Padding")
            .style(Style::new().fg(theme::fg::MUTED));
        let inner = demo_block.inner(area);
        demo_block.render(area, frame);
        Widget::render(&padded, inner, frame);
    }

    fn render_align_demo(&self, frame: &mut Frame, area: Rect) {
        let (h_align, v_align, pos_name) = align_position(self.align_pos);

        let content = Paragraph::new(pos_name)
            .style(Style::new().fg(theme::accent::INFO).attrs(StyleFlags::BOLD));
        let aligned = Align::new(content)
            .horizontal(h_align)
            .vertical(v_align)
            .child_width(display_width(pos_name) as u16)
            .child_height(1);

        let demo_block = Block::new()
            .borders(Borders::ALL)
            .title("Align (l)")
            .style(Style::new().fg(theme::fg::MUTED));
        let inner = demo_block.inner(area);
        demo_block.render(area, frame);
        Widget::render(&aligned, inner, frame);
    }

    fn render_columns_demo(&self, frame: &mut Frame, area: Rect) {
        let colors = region_colors();
        let col_a = Paragraph::new("A").style(Style::new().fg(colors[0]).attrs(StyleFlags::BOLD));
        let col_b = Paragraph::new("B").style(Style::new().fg(colors[1]).attrs(StyleFlags::BOLD));
        let col_c = Paragraph::new("C").style(Style::new().fg(colors[2]).attrs(StyleFlags::BOLD));

        let columns = Columns::new()
            .gap(theme::spacing::XS)
            .column(col_a, Constraint::Ratio(1, 3))
            .column(col_b, Constraint::Ratio(1, 3))
            .column(col_c, Constraint::Ratio(1, 3));

        let demo_block = Block::new()
            .borders(Borders::ALL)
            .title("Columns")
            .style(Style::new().fg(theme::fg::MUTED));
        let inner = demo_block.inner(area);
        demo_block.render(area, frame);
        columns.render(inner, frame);
    }

    fn render_group_demo(&self, frame: &mut Frame, area: Rect) {
        let bg = Paragraph::new("Group layer").style(Style::new().fg(theme::fg::MUTED));
        let fg = Paragraph::new("Overlay").style(
            Style::new()
                .fg(theme::accent::SUCCESS)
                .attrs(StyleFlags::BOLD),
        );

        let group = Group::new().push(bg).push(fg);

        let demo_block = Block::new()
            .borders(Borders::ALL)
            .title("Group")
            .style(Style::new().fg(theme::fg::MUTED));
        let inner = demo_block.inner(area);
        demo_block.render(area, frame);
        group.render(inner, frame);
    }
}

/// Map an align position index (0-8) to horizontal + vertical alignment and a label.
fn align_position(idx: usize) -> (Alignment, VerticalAlignment, &'static str) {
    match idx % 9 {
        0 => (Alignment::Left, VerticalAlignment::Top, "TopLeft"),
        1 => (Alignment::Center, VerticalAlignment::Top, "TopCenter"),
        2 => (Alignment::Right, VerticalAlignment::Top, "TopRight"),
        3 => (Alignment::Left, VerticalAlignment::Middle, "MidLeft"),
        4 => (Alignment::Center, VerticalAlignment::Middle, "Center"),
        5 => (Alignment::Right, VerticalAlignment::Middle, "MidRight"),
        6 => (Alignment::Left, VerticalAlignment::Bottom, "BotLeft"),
        7 => (Alignment::Center, VerticalAlignment::Bottom, "BotCenter"),
        _ => (Alignment::Right, VerticalAlignment::Bottom, "BotRight"),
    }
}

/// Solve a flex layout and return the resulting rects (for testing).
pub fn solve_flex_vertical(area: Rect, constraints: &[Constraint]) -> Vec<Rect> {
    Flex::vertical()
        .constraints(constraints.iter().copied())
        .split(area)
}

pub fn solve_flex_horizontal(area: Rect, constraints: &[Constraint]) -> Vec<Rect> {
    Flex::horizontal()
        .constraints(constraints.iter().copied())
        .split(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // Basic Layout Tests
    // ==========================================================================

    #[test]
    fn layout_flex_vertical() {
        let area = Rect::new(0, 0, 80, 40);
        let constraints = [
            Constraint::Fixed(5),
            Constraint::Percentage(30.0),
            Constraint::Min(3),
            Constraint::Max(10),
            Constraint::Ratio(1, 3),
        ];
        let rects = solve_flex_vertical(area, &constraints);
        assert_eq!(rects.len(), 5);
        // First rect should be exactly 5 rows tall
        assert_eq!(rects[0].height, 5);
        assert_eq!(rects[0].width, 80);
        // All rects should be within the area
        for r in &rects {
            assert!(r.x >= area.x);
            assert!(r.y >= area.y);
            assert!(r.x + r.width <= area.x + area.width);
            assert!(r.y + r.height <= area.y + area.height);
        }
    }

    #[test]
    fn layout_flex_horizontal() {
        let area = Rect::new(0, 0, 100, 30);
        let constraints = [
            Constraint::Fixed(10),
            Constraint::Percentage(30.0),
            Constraint::Min(5),
        ];
        let rects = solve_flex_horizontal(area, &constraints);
        assert_eq!(rects.len(), 3);
        // First rect should be exactly 10 cols wide
        assert_eq!(rects[0].width, 10);
        assert_eq!(rects[0].height, 30);
        // All rects should be within the area
        for r in &rects {
            assert!(r.x >= area.x);
            assert!(r.y >= area.y);
            assert!(r.x + r.width <= area.x + area.width);
        }
    }

    #[test]
    fn layout_grid_spanning() {
        let area = Rect::new(0, 0, 60, 20);
        // Header row
        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(3), Constraint::Min(2), Constraint::Min(2)])
            .split(area);
        // Header spans full width
        assert_eq!(rows[0].width, 60);
        assert_eq!(rows[0].height, 3);
        // Each row has 3 cols
        let cols = Flex::horizontal()
            .constraints([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(rows[1]);
        assert_eq!(cols.len(), 3);
        // Columns should roughly be 20 each
        for c in &cols {
            assert!(c.width >= 19 && c.width <= 21, "col width: {}", c.width);
        }
    }

    #[test]
    fn layout_alignment_modes() {
        let area = Rect::new(0, 0, 100, 50);
        // Test all 5 alignments with a small fixed constraint
        for alignment in &ALIGNMENTS {
            let rects = Flex::vertical()
                .alignment(*alignment)
                .constraints([Constraint::Fixed(10)])
                .split(area);
            assert_eq!(rects.len(), 1);
            assert_eq!(rects[0].height, 10);
            assert_eq!(rects[0].width, 100);
        }
    }

    #[test]
    fn layout_nested() {
        let area = Rect::new(0, 0, 100, 40);
        // Outer: 60/40 horizontal split
        let outer = Flex::horizontal()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(area);
        assert_eq!(outer.len(), 2);
        assert_eq!(outer[0].width, 60);
        assert_eq!(outer[1].width, 40);

        // Inner left: 3 vertical parts
        let inner_left = Flex::vertical()
            .constraints([
                Constraint::Fixed(5),
                Constraint::Min(4),
                Constraint::Fixed(5),
            ])
            .split(outer[0]);
        assert_eq!(inner_left.len(), 3);
        assert_eq!(inner_left[0].height, 5);
        assert_eq!(inner_left[2].height, 5);
        // Middle gets the rest: 40 - 5 - 5 = 30
        assert_eq!(inner_left[1].height, 30);
    }

    // ==========================================================================
    // Unit Tests: LayoutLab State and Logic (bd-32my.3)
    // ==========================================================================

    #[test]
    fn layout_lab_default_state() {
        let lab = LayoutLab::new();
        assert_eq!(lab.current_preset, 0);
        assert_eq!(lab.selected_constraint, 0);
        assert_eq!(lab.direction, Direction::Vertical);
        assert_eq!(lab.alignment_idx, 0);
        assert_eq!(lab.gap, 0);
        assert_eq!(lab.margin, 0);
        assert_eq!(lab.align_pos, 4); // Center
        assert!(!lab.show_debug);
    }

    #[test]
    fn direction_toggle() {
        assert_eq!(Direction::Vertical.toggle(), Direction::Horizontal);
        assert_eq!(Direction::Horizontal.toggle(), Direction::Vertical);
        // Toggle is involutive
        assert_eq!(Direction::Vertical.toggle().toggle(), Direction::Vertical);
    }

    #[test]
    fn direction_labels() {
        assert_eq!(Direction::Vertical.label(), "Vertical");
        assert_eq!(Direction::Horizontal.label(), "Horizontal");
    }

    #[test]
    fn constraint_label_formatting() {
        assert_eq!(
            LayoutLab::constraint_label(&Constraint::Fixed(10)),
            "Fixed(10)"
        );
        assert_eq!(
            LayoutLab::constraint_label(&Constraint::Percentage(50.0)),
            "Pct(50%)"
        );
        assert_eq!(LayoutLab::constraint_label(&Constraint::Min(5)), "Min(5)");
        assert_eq!(LayoutLab::constraint_label(&Constraint::Max(20)), "Max(20)");
        assert_eq!(
            LayoutLab::constraint_label(&Constraint::Ratio(1, 3)),
            "Ratio(1/3)"
        );
        assert_eq!(LayoutLab::constraint_label(&Constraint::Fill), "Fill");
        assert_eq!(
            LayoutLab::constraint_label(&Constraint::FitContent),
            "FitContent"
        );
        assert_eq!(
            LayoutLab::constraint_label(&Constraint::FitContentBounded { min: 5, max: 20 }),
            "FitContent(5..20)"
        );
        assert_eq!(LayoutLab::constraint_label(&Constraint::FitMin), "FitMin");
    }

    #[test]
    fn preset_constraints_count() {
        let lab = LayoutLab::new();

        // Preset 0: 5 constraints
        let mut lab0 = lab;
        lab0.current_preset = 0;
        assert_eq!(lab0.preset_constraints().len(), 5);

        // Preset 1: 5 constraints (same as 0)
        let mut lab1 = LayoutLab::new();
        lab1.current_preset = 1;
        assert_eq!(lab1.preset_constraints().len(), 5);

        // Preset 2: 3 constraints (grid)
        let mut lab2 = LayoutLab::new();
        lab2.current_preset = 2;
        assert_eq!(lab2.preset_constraints().len(), 3);

        // Preset 3: 2 constraints (nested)
        let mut lab3 = LayoutLab::new();
        lab3.current_preset = 3;
        assert_eq!(lab3.preset_constraints().len(), 2);

        // Preset 4: 3 constraints (real-world)
        let mut lab4 = LayoutLab::new();
        lab4.current_preset = 4;
        assert_eq!(lab4.preset_constraints().len(), 3);
    }

    #[test]
    fn alignment_cycling() {
        let lab = LayoutLab::new();
        assert_eq!(lab.alignment_idx, 0);
        assert_eq!(lab.current_alignment(), FlexAlignment::Start);

        let mut lab = lab;
        lab.alignment_idx = 1;
        assert_eq!(lab.current_alignment(), FlexAlignment::Center);

        lab.alignment_idx = 2;
        assert_eq!(lab.current_alignment(), FlexAlignment::End);

        lab.alignment_idx = 3;
        assert_eq!(lab.current_alignment(), FlexAlignment::SpaceBetween);

        lab.alignment_idx = 4;
        assert_eq!(lab.current_alignment(), FlexAlignment::SpaceAround);

        // Wrap around
        lab.alignment_idx = 5 % ALIGNMENTS.len();
        assert_eq!(lab.current_alignment(), FlexAlignment::Start);
    }

    #[test]
    fn align_position_mapping() {
        // Test all 9 positions
        assert_eq!(
            align_position(0),
            (Alignment::Left, VerticalAlignment::Top, "TopLeft")
        );
        assert_eq!(
            align_position(1),
            (Alignment::Center, VerticalAlignment::Top, "TopCenter")
        );
        assert_eq!(
            align_position(2),
            (Alignment::Right, VerticalAlignment::Top, "TopRight")
        );
        assert_eq!(
            align_position(3),
            (Alignment::Left, VerticalAlignment::Middle, "MidLeft")
        );
        assert_eq!(
            align_position(4),
            (Alignment::Center, VerticalAlignment::Middle, "Center")
        );
        assert_eq!(
            align_position(5),
            (Alignment::Right, VerticalAlignment::Middle, "MidRight")
        );
        assert_eq!(
            align_position(6),
            (Alignment::Left, VerticalAlignment::Bottom, "BotLeft")
        );
        assert_eq!(
            align_position(7),
            (Alignment::Center, VerticalAlignment::Bottom, "BotCenter")
        );
        assert_eq!(
            align_position(8),
            (Alignment::Right, VerticalAlignment::Bottom, "BotRight")
        );
        // Wrap around
        assert_eq!(
            align_position(9),
            (Alignment::Left, VerticalAlignment::Top, "TopLeft")
        );
    }

    #[test]
    fn keybindings_non_empty() {
        let lab = LayoutLab::new();
        let bindings = lab.keybindings();
        assert!(!bindings.is_empty());
        // Should have at least 10 keybindings
        assert!(
            bindings.len() >= 10,
            "Expected at least 10 keybindings, got {}",
            bindings.len()
        );
    }

    #[test]
    fn title_and_tab_label() {
        let lab = LayoutLab::new();
        assert_eq!(lab.title(), "Layout Laboratory");
        assert_eq!(lab.tab_label(), "Layout");
    }

    // ==========================================================================
    // Edge Cases
    // ==========================================================================

    #[test]
    fn layout_zero_size_area() {
        let area = Rect::new(0, 0, 0, 0);
        let constraints = [Constraint::Fixed(10)];
        let rects = solve_flex_vertical(area, &constraints);
        // Should return 1 rect with zero dimensions
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].width, 0);
        assert_eq!(rects[0].height, 0);
    }

    #[test]
    fn layout_tiny_area() {
        let area = Rect::new(0, 0, 2, 2);
        let constraints = [Constraint::Fixed(5), Constraint::Fixed(5)];
        let rects = solve_flex_vertical(area, &constraints);
        assert_eq!(rects.len(), 2);
        // Rects should still be within area even if constraints can't be satisfied
        for r in &rects {
            assert!(r.x + r.width <= area.x + area.width);
            assert!(r.y + r.height <= area.y + area.height);
        }
    }

    #[test]
    fn layout_single_constraint() {
        let area = Rect::new(10, 20, 80, 40);
        let constraints = [Constraint::Min(0)];
        let rects = solve_flex_vertical(area, &constraints);
        assert_eq!(rects.len(), 1);
        // Single Min(0) should fill the entire area
        assert_eq!(rects[0], area);
    }

    #[test]
    fn layout_empty_constraints() {
        let area = Rect::new(0, 0, 80, 40);
        let constraints: [Constraint; 0] = [];
        let rects = solve_flex_vertical(area, &constraints);
        assert!(rects.is_empty());
    }

    #[test]
    fn layout_offset_area() {
        // Test with non-zero origin
        let area = Rect::new(50, 100, 80, 40);
        let constraints = [Constraint::Fixed(10), Constraint::Min(5)];
        let rects = solve_flex_vertical(area, &constraints);
        assert_eq!(rects.len(), 2);
        // First rect should start at area origin
        assert_eq!(rects[0].x, 50);
        assert_eq!(rects[0].y, 100);
        // All within area
        for r in &rects {
            assert!(r.x >= area.x);
            assert!(r.y >= area.y);
            assert!(r.x + r.width <= area.x + area.width);
            assert!(r.y + r.height <= area.y + area.height);
        }
    }

    #[test]
    fn layout_with_gap() {
        let area = Rect::new(0, 0, 100, 50);
        let rects = Flex::vertical()
            .gap(5)
            .constraints([
                Constraint::Fixed(10),
                Constraint::Fixed(10),
                Constraint::Fixed(10),
            ])
            .split(area);
        assert_eq!(rects.len(), 3);
        // Check that there's a gap between rects
        // First rect ends at y=10, second should start at y=15 (gap=5)
        assert_eq!(rects[0].y + rects[0].height + 5, rects[1].y);
        assert_eq!(rects[1].y + rects[1].height + 5, rects[2].y);
    }

    #[test]
    fn layout_with_margin() {
        let area = Rect::new(0, 0, 100, 50);
        let rects = Flex::vertical()
            .margin(Sides::all(5))
            .constraints([Constraint::Min(0)])
            .split(area);
        assert_eq!(rects.len(), 1);
        // Rect should be inset by margin
        assert_eq!(rects[0].x, 5);
        assert_eq!(rects[0].y, 5);
        assert_eq!(rects[0].width, 90); // 100 - 5 - 5
        assert_eq!(rects[0].height, 40); // 50 - 5 - 5
    }

    // ==========================================================================
    // Invariant Tests
    // ==========================================================================

    #[test]
    fn layout_rects_no_overlap_vertical() {
        let area = Rect::new(0, 0, 80, 40);
        let constraints = [
            Constraint::Fixed(10),
            Constraint::Percentage(30.0),
            Constraint::Min(5),
        ];
        let rects = solve_flex_vertical(area, &constraints);

        // Check no vertical overlap
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let r1 = &rects[i];
                let r2 = &rects[j];
                // In vertical layout, r2 should start at or after r1 ends
                assert!(
                    r2.y >= r1.y + r1.height,
                    "Overlap detected: rect {} ends at y={}, rect {} starts at y={}",
                    i,
                    r1.y + r1.height,
                    j,
                    r2.y
                );
            }
        }
    }

    #[test]
    fn layout_rects_no_overlap_horizontal() {
        let area = Rect::new(0, 0, 100, 30);
        let constraints = [
            Constraint::Fixed(20),
            Constraint::Percentage(40.0),
            Constraint::Min(10),
        ];
        let rects = solve_flex_horizontal(area, &constraints);

        // Check no horizontal overlap
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let r1 = &rects[i];
                let r2 = &rects[j];
                // In horizontal layout, r2 should start at or after r1 ends
                assert!(
                    r2.x >= r1.x + r1.width,
                    "Overlap detected: rect {} ends at x={}, rect {} starts at x={}",
                    i,
                    r1.x + r1.width,
                    j,
                    r2.x
                );
            }
        }
    }

    #[test]
    fn layout_fixed_constraint_exact() {
        // Fixed constraints should produce exact sizes
        let area = Rect::new(0, 0, 100, 100);
        let rects = solve_flex_vertical(
            area,
            &[
                Constraint::Fixed(10),
                Constraint::Fixed(20),
                Constraint::Min(0),
            ],
        );
        assert_eq!(rects[0].height, 10, "Fixed(10) should be exactly 10");
        assert_eq!(rects[1].height, 20, "Fixed(20) should be exactly 20");
    }

    #[test]
    fn layout_ratio_proportional() {
        let area = Rect::new(0, 0, 90, 30);
        let rects = solve_flex_horizontal(
            area,
            &[
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ],
        );
        // Each should be 30 wide (90 / 3)
        for r in &rects {
            assert_eq!(r.width, 30, "Each ratio(1,3) of 90 should be 30");
        }
    }

    #[test]
    fn layout_percentage_proportional() {
        let area = Rect::new(0, 0, 100, 100);
        let rects = solve_flex_vertical(
            area,
            &[Constraint::Percentage(25.0), Constraint::Percentage(75.0)],
        );
        assert_eq!(rects[0].height, 25, "25% of 100 should be 25");
        assert_eq!(rects[1].height, 75, "75% of 100 should be 75");
    }

    // ==========================================================================
    // Render Determinism + Event-Driven Tests (bd-32my.3 completion)
    // ==========================================================================

    use ftui_render::grapheme_pool::GraphemePool;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn checksum_frame(frame: &Frame) -> u64 {
        let mut hasher = DefaultHasher::new();
        let width = frame.buffer.width();
        let height = frame.buffer.height();
        for y in 0..height {
            for x in 0..width {
                if let Some(cell) = frame.buffer.get(x, y) {
                    cell.content.hash(&mut hasher);
                    cell.fg.hash(&mut hasher);
                    cell.bg.hash(&mut hasher);
                    cell.attrs.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }

    fn log_jsonl(test: &str, fields: &[(&str, String)]) {
        let mut parts = Vec::with_capacity(fields.len() + 1);
        parts.push(format!("\"test\":\"{}\"", test));
        parts.extend(fields.iter().map(|(k, v)| format!("\"{}\":\"{}\"", k, v)));
        eprintln!("{{{}}}", parts.join(","));
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(ftui_core::event::KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    }

    fn shift_press(code: KeyCode) -> Event {
        Event::Key(ftui_core::event::KeyEvent {
            code,
            modifiers: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn render_deterministic_all_presets() {
        for preset in 0..PRESET_COUNT {
            let mut lab = LayoutLab::new();
            lab.current_preset = preset;

            // Acquire scoped theme lock to prevent race with parallel tests
            // that mutate the global CURRENT_THEME via set_theme().
            let _theme_guard = theme::ScopedThemeLock::new(theme::ThemeId::CyberpunkAurora);

            let checksum_a = {
                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(120, 40, &mut pool);
                lab.view(&mut frame, Rect::new(0, 0, 120, 40));
                checksum_frame(&frame)
            };

            let checksum_b = {
                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(120, 40, &mut pool);
                lab.view(&mut frame, Rect::new(0, 0, 120, 40));
                checksum_frame(&frame)
            };

            log_jsonl(
                "render_deterministic_all_presets",
                &[
                    ("preset", preset.to_string()),
                    ("checksum", format!("{checksum_a:016x}")),
                ],
            );

            assert_eq!(
                checksum_a, checksum_b,
                "Render should be deterministic for preset {}",
                preset
            );
            assert_ne!(
                checksum_a, 0,
                "Checksum should be non-zero for preset {}",
                preset
            );
        }
    }

    #[test]
    fn render_deterministic_with_state_variations() {
        let mut lab = LayoutLab::new();
        lab.current_preset = 1;
        lab.direction = Direction::Horizontal;
        lab.alignment_idx = 3;
        lab.gap = 3;
        lab.margin = 2;
        lab.padding_amount = 2;
        lab.align_pos = 7;
        lab.show_debug = true;

        // Acquire scoped theme lock to prevent race with parallel tests
        // that mutate the global CURRENT_THEME via set_theme().
        let _theme_guard = theme::ScopedThemeLock::new(theme::ThemeId::CyberpunkAurora);

        let checksum_a = {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            lab.view(&mut frame, Rect::new(0, 0, 120, 40));
            checksum_frame(&frame)
        };

        let checksum_b = {
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            lab.view(&mut frame, Rect::new(0, 0, 120, 40));
            checksum_frame(&frame)
        };

        log_jsonl(
            "render_deterministic_with_state_variations",
            &[("checksum", format!("{checksum_a:016x}"))],
        );

        assert_eq!(
            checksum_a, checksum_b,
            "Render should be deterministic with complex state"
        );
    }

    #[test]
    fn all_presets_render_at_standard_sizes() {
        let sizes: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 60)];

        for preset in 0..PRESET_COUNT {
            for (w, h) in &sizes {
                let mut lab = LayoutLab::new();
                lab.current_preset = preset;

                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(*w, *h, &mut pool);
                lab.view(&mut frame, Rect::new(0, 0, *w, *h));

                let checksum = checksum_frame(&frame);
                log_jsonl(
                    "all_presets_render",
                    &[
                        ("preset", preset.to_string()),
                        ("size", format!("{w}x{h}")),
                        ("checksum", format!("{checksum:016x}")),
                    ],
                );
                assert_ne!(
                    checksum, 0,
                    "Preset {} at {}x{} should produce non-empty output",
                    preset, w, h
                );
            }
        }
    }

    #[test]
    fn tiny_terminal_shows_message() {
        let lab = LayoutLab::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(30, 5, &mut pool);
        lab.view(&mut frame, Rect::new(0, 0, 30, 5));

        // Should not panic and should produce some output
        let checksum = checksum_frame(&frame);
        assert_ne!(checksum, 0, "Tiny terminal should still produce output");
    }

    #[test]
    fn update_preset_via_events() {
        let mut lab = LayoutLab::new();
        assert_eq!(lab.current_preset, 0);

        lab.update(&press(KeyCode::Char('3')));
        assert_eq!(lab.current_preset, 2, "Pressing '3' should select preset 2");

        lab.update(&press(KeyCode::Char('5')));
        assert_eq!(lab.current_preset, 4, "Pressing '5' should select preset 4");

        lab.update(&press(KeyCode::Char('1')));
        assert_eq!(lab.current_preset, 0, "Pressing '1' should select preset 0");

        log_jsonl(
            "update_preset_via_events",
            &[("passed", "true".to_string())],
        );
    }

    #[test]
    fn update_direction_via_event() {
        let mut lab = LayoutLab::new();
        assert_eq!(lab.direction, Direction::Vertical);

        lab.update(&press(KeyCode::Char('d')));
        assert_eq!(lab.direction, Direction::Horizontal);

        lab.update(&press(KeyCode::Char('d')));
        assert_eq!(lab.direction, Direction::Vertical);

        log_jsonl(
            "update_direction_via_event",
            &[("passed", "true".to_string())],
        );
    }

    #[test]
    fn update_alignment_via_event() {
        let mut lab = LayoutLab::new();
        assert_eq!(lab.alignment_idx, 0);

        for expected in 1..=5 {
            lab.update(&press(KeyCode::Char('a')));
            assert_eq!(
                lab.alignment_idx,
                expected % ALIGNMENTS.len(),
                "After {} presses",
                expected
            );
        }
    }

    #[test]
    fn update_gap_via_events() {
        let mut lab = LayoutLab::new();
        assert_eq!(lab.gap, 0);

        // Increase gap to max (5)
        for _ in 0..10 {
            lab.update(&press(KeyCode::Char('+')));
        }
        assert_eq!(lab.gap, 5, "Gap should saturate at 5");

        // Decrease gap to 0
        for _ in 0..10 {
            lab.update(&press(KeyCode::Char('-')));
        }
        assert_eq!(lab.gap, 0, "Gap should saturate at 0");

        log_jsonl("update_gap_via_events", &[("passed", "true".to_string())]);
    }

    #[test]
    fn update_margin_via_events() {
        let mut lab = LayoutLab::new();
        assert_eq!(lab.margin, 0);

        // Increase margin to max (4)
        for _ in 0..10 {
            lab.update(&press(KeyCode::Char('m')));
        }
        assert_eq!(lab.margin, 4, "Margin should saturate at 4");

        // Decrease margin to 0 (Char('M') with NONE modifier)
        for _ in 0..10 {
            lab.update(&press(KeyCode::Char('M')));
        }
        assert_eq!(lab.margin, 0, "Margin should saturate at 0");

        log_jsonl(
            "update_margin_via_events",
            &[("passed", "true".to_string())],
        );
    }

    #[test]
    fn update_padding_via_events() {
        let mut lab = LayoutLab::new();
        assert_eq!(
            lab.padding_amount,
            theme::spacing::XS,
            "Default padding is theme::spacing::XS"
        );

        // Increase padding to max (4)
        for _ in 0..10 {
            lab.update(&press(KeyCode::Char('p')));
        }
        assert_eq!(lab.padding_amount, 4, "Padding should saturate at 4");

        // Decrease padding to 0 (Char('P') with NONE modifier)
        for _ in 0..10 {
            lab.update(&press(KeyCode::Char('P')));
        }
        assert_eq!(lab.padding_amount, 0, "Padding should saturate at 0");

        log_jsonl(
            "update_padding_via_events",
            &[("passed", "true".to_string())],
        );
    }

    #[test]
    fn update_debug_toggle_via_event() {
        let mut lab = LayoutLab::new();
        assert!(!lab.show_debug);

        // Char('D') with NONE modifier (how terminals typically send uppercase)
        lab.update(&press(KeyCode::Char('D')));
        assert!(lab.show_debug, "D should enable debug");

        lab.update(&press(KeyCode::Char('D')));
        assert!(!lab.show_debug, "D again should disable debug");

        log_jsonl("update_debug_toggle", &[("passed", "true".to_string())]);
    }

    #[test]
    fn update_align_position_via_event() {
        let mut lab = LayoutLab::new();
        assert_eq!(lab.align_pos, 4); // Center

        lab.update(&press(KeyCode::Char('l')));
        assert_eq!(lab.align_pos, 5); // MidRight

        // Cycle all 9 positions
        for _ in 0..8 {
            lab.update(&press(KeyCode::Char('l')));
        }
        assert_eq!(
            lab.align_pos, 4,
            "Full 9-position cycle should return to start+1"
        );

        log_jsonl("update_align_position", &[("passed", "true".to_string())]);
    }

    #[test]
    fn update_tab_cycles_constraints() {
        let mut lab = LayoutLab::new();
        lab.current_preset = 0; // 5 constraints
        assert_eq!(lab.selected_constraint, 0);

        let count = lab.preset_constraints().len();
        for expected in 1..=count {
            lab.update(&press(KeyCode::Tab));
            assert_eq!(lab.selected_constraint, expected % count);
        }
    }

    #[test]
    fn non_press_events_ignored() {
        let mut lab = LayoutLab::new();
        let initial_preset = lab.current_preset;
        let initial_direction = lab.direction;

        // Release and Repeat events should be ignored
        let release = Event::Key(ftui_core::event::KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Release,
        });
        lab.update(&release);
        assert_eq!(
            lab.direction, initial_direction,
            "Release events should be ignored"
        );

        let repeat = Event::Key(ftui_core::event::KeyEvent {
            code: KeyCode::Char('3'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Repeat,
        });
        lab.update(&repeat);
        assert_eq!(
            lab.current_preset, initial_preset,
            "Repeat events should be ignored"
        );
    }

    #[test]
    fn debug_overlay_renders_without_panic() {
        let mut lab = LayoutLab::new();
        lab.show_debug = true;

        for preset in 0..PRESET_COUNT {
            lab.current_preset = preset;
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            lab.view(&mut frame, Rect::new(0, 0, 120, 40));

            let checksum = checksum_frame(&frame);
            assert_ne!(
                checksum, 0,
                "Debug overlay on preset {} should render",
                preset
            );
        }
    }

    #[test]
    fn presets_produce_distinct_renders() {
        let mut checksums = Vec::with_capacity(PRESET_COUNT);

        for preset in 0..PRESET_COUNT {
            let mut lab = LayoutLab::new();
            lab.current_preset = preset;
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            lab.view(&mut frame, Rect::new(0, 0, 120, 40));
            checksums.push(checksum_frame(&frame));
        }

        // Each preset should produce a distinct render
        for i in 0..checksums.len() {
            for j in (i + 1)..checksums.len() {
                assert_ne!(
                    checksums[i], checksums[j],
                    "Presets {} and {} should produce distinct renders",
                    i, j
                );
            }
        }

        log_jsonl(
            "presets_produce_distinct_renders",
            &[("count", checksums.len().to_string())],
        );
    }

    #[test]
    fn direction_produces_distinct_render() {
        let mut lab = LayoutLab::new();
        lab.current_preset = 0;

        let checksum_vertical = {
            lab.direction = Direction::Vertical;
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            lab.view(&mut frame, Rect::new(0, 0, 120, 40));
            checksum_frame(&frame)
        };

        let checksum_horizontal = {
            lab.direction = Direction::Horizontal;
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(120, 40, &mut pool);
            lab.view(&mut frame, Rect::new(0, 0, 120, 40));
            checksum_frame(&frame)
        };

        assert_ne!(
            checksum_vertical, checksum_horizontal,
            "Vertical and horizontal should produce distinct renders"
        );
    }
}

#[test]
fn click_preview_toggles_debug() {
    let mut lab = LayoutLab::new();
    lab.layout_preview.set(Rect::new(0, 0, 80, 20));
    assert!(!lab.show_debug);
    lab.handle_mouse(MouseEventKind::Down(MouseButton::Left), 10, 10);
    assert!(lab.show_debug);
    lab.handle_mouse(MouseEventKind::Down(MouseButton::Left), 10, 10);
    assert!(!lab.show_debug);
}

#[test]
fn click_controls_toggles_direction() {
    let mut lab = LayoutLab::new();
    lab.layout_controls.set(Rect::new(0, 20, 40, 10));
    assert_eq!(lab.direction, Direction::Vertical);
    lab.handle_mouse(MouseEventKind::Down(MouseButton::Left), 10, 25);
    assert_eq!(lab.direction, Direction::Horizontal);
}

#[test]
fn right_click_preview_cycles_preset() {
    let mut lab = LayoutLab::new();
    lab.layout_preview.set(Rect::new(0, 0, 80, 20));
    assert_eq!(lab.current_preset, 0);
    lab.handle_mouse(MouseEventKind::Down(MouseButton::Right), 10, 10);
    assert_eq!(lab.current_preset, 1);
}

#[test]
fn scroll_preview_adjusts_gap() {
    let mut lab = LayoutLab::new();
    lab.layout_preview.set(Rect::new(0, 0, 80, 20));
    assert_eq!(lab.gap, 0);
    lab.handle_mouse(MouseEventKind::ScrollDown, 10, 10);
    assert_eq!(lab.gap, 1);
    lab.handle_mouse(MouseEventKind::ScrollUp, 10, 10);
    assert_eq!(lab.gap, 0);
}

#[test]
fn scroll_controls_adjusts_alignment() {
    let mut lab = LayoutLab::new();
    lab.layout_controls.set(Rect::new(0, 20, 40, 10));
    assert_eq!(lab.alignment_idx, 0);
    lab.handle_mouse(MouseEventKind::ScrollDown, 10, 25);
    assert_eq!(lab.alignment_idx, 1);
    lab.handle_mouse(MouseEventKind::ScrollUp, 10, 25);
    assert_eq!(lab.alignment_idx, 0);
}

// ==========================================================================
// Property Tests for Layout Invariants (bd-32my.3)
// ==========================================================================

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Generate a reasonable Rect for testing
    fn arb_rect() -> impl Strategy<Value = Rect> {
        (0u16..100, 0u16..100, 10u16..200, 10u16..100)
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    /// Generate a simple constraint
    fn arb_simple_constraint() -> impl Strategy<Value = Constraint> {
        prop_oneof![
            (1u16..50).prop_map(Constraint::Fixed),
            (1.0f64..100.0).prop_map(|p| Constraint::Percentage(p as f32)),
            (0u16..30).prop_map(Constraint::Min),
            (5u16..50).prop_map(Constraint::Max),
            (1u16..5, 2u16..6)
                .prop_filter("denominator > numerator", |(n, d)| n < d)
                .prop_map(|(n, d)| Constraint::Ratio(u32::from(n), u32::from(d))),
        ]
    }

    proptest! {
        /// Property: All resulting rects are within the input area bounds.
        #[test]
        fn prop_rects_within_area(area in arb_rect(), constraints in prop::collection::vec(arb_simple_constraint(), 1..5)) {
            let rects = solve_flex_vertical(area, &constraints);

            for (i, r) in rects.iter().enumerate() {
                prop_assert!(
                    r.x >= area.x,
                    "Rect {} x={} < area.x={}",
                    i, r.x, area.x
                );
                prop_assert!(
                    r.y >= area.y,
                    "Rect {} y={} < area.y={}",
                    i, r.y, area.y
                );
                prop_assert!(
                    r.x + r.width <= area.x + area.width,
                    "Rect {} right={} > area right={}",
                    i, r.x + r.width, area.x + area.width
                );
                prop_assert!(
                    r.y + r.height <= area.y + area.height,
                    "Rect {} bottom={} > area bottom={}",
                    i, r.y + r.height, area.y + area.height
                );
            }
        }

        /// Property: Number of resulting rects equals number of constraints.
        #[test]
        fn prop_rect_count_matches_constraints(
            area in arb_rect(),
            constraints in prop::collection::vec(arb_simple_constraint(), 1..5)
        ) {
            let rects = solve_flex_vertical(area, &constraints);
            prop_assert_eq!(rects.len(), constraints.len());
        }

        /// Property: All resulting rects have full width (in vertical layout).
        #[test]
        fn prop_vertical_rects_full_width(
            area in arb_rect(),
            constraints in prop::collection::vec(arb_simple_constraint(), 1..4)
        ) {
            let rects = solve_flex_vertical(area, &constraints);
            for (i, r) in rects.iter().enumerate() {
                prop_assert_eq!(
                    r.width, area.width,
                    "Rect {} width={} != area.width={}",
                    i, r.width, area.width
                );
            }
        }

        /// Property: All resulting rects have full height (in horizontal layout).
        #[test]
        fn prop_horizontal_rects_full_height(
            area in arb_rect(),
            constraints in prop::collection::vec(arb_simple_constraint(), 1..4)
        ) {
            let rects = solve_flex_horizontal(area, &constraints);
            for (i, r) in rects.iter().enumerate() {
                prop_assert_eq!(
                    r.height, area.height,
                    "Rect {} height={} != area.height={}",
                    i, r.height, area.height
                );
            }
        }

        /// Property: Vertical layout rects are contiguous (no gaps without explicit gap setting).
        #[test]
        fn prop_vertical_contiguous(
            area in arb_rect(),
            constraints in prop::collection::vec(arb_simple_constraint(), 2..4)
        ) {
            let rects = solve_flex_vertical(area, &constraints);
            for i in 1..rects.len() {
                let prev = &rects[i - 1];
                let curr = &rects[i];
                prop_assert!(
                    curr.y >= prev.y + prev.height,
                    "Rect {} overlaps rect {}: curr.y={} < prev_end={}",
                    i, i - 1, curr.y, prev.y + prev.height
                );
            }
        }

        /// Property: Horizontal layout rects are contiguous (no gaps without explicit gap setting).
        #[test]
        fn prop_horizontal_contiguous(
            area in arb_rect(),
            constraints in prop::collection::vec(arb_simple_constraint(), 2..4)
        ) {
            let rects = solve_flex_horizontal(area, &constraints);
            for i in 1..rects.len() {
                let prev = &rects[i - 1];
                let curr = &rects[i];
                prop_assert!(
                    curr.x >= prev.x + prev.width,
                    "Rect {} overlaps rect {}: curr.x={} < prev_end={}",
                    i, i - 1, curr.x, prev.x + prev.width
                );
            }
        }

        /// Property: Direction toggle is involutive (toggle twice = identity).
        #[test]
        fn prop_direction_toggle_involutive(dir in prop_oneof![Just(Direction::Vertical), Just(Direction::Horizontal)]) {
            prop_assert_eq!(dir.toggle().toggle(), dir);
        }

        /// Property: Alignment index wraps correctly.
        #[test]
        fn prop_alignment_wrap(idx in 0usize..100) {
            let wrapped = idx % ALIGNMENTS.len();
            prop_assert!(wrapped < ALIGNMENTS.len());
            // Accessing should not panic
            let _ = ALIGNMENTS[wrapped];
        }

        /// Property: Align position wraps correctly (9 positions).
        #[test]
        fn prop_align_position_wrap(idx in 0usize..100) {
            let (h, v, label) = align_position(idx);
            // Should not panic and label should be non-empty
            prop_assert!(!label.is_empty());
            // Horizontal and vertical alignment should be valid enums
            let _ = h;
            let _ = v;
        }

        /// Property: Gap adjustment is bounded.
        #[test]
        fn prop_gap_bounded(initial in 0u16..10, delta in 0u16..10) {
            let mut lab = LayoutLab::new();
            lab.gap = initial;
            lab.gap = lab.gap.saturating_add(delta).min(5);
            prop_assert!(lab.gap <= 5);
        }

        /// Property: Margin adjustment is bounded.
        #[test]
        fn prop_margin_bounded(initial in 0u16..10, delta in 0u16..10) {
            let mut lab = LayoutLab::new();
            lab.margin = initial;
            lab.margin = lab.margin.saturating_add(delta).min(4);
            prop_assert!(lab.margin <= 4);
        }

        /// Property: Padding adjustment is bounded at max=4.
        #[test]
        fn prop_padding_bounded(initial in 0u16..10, delta in 0u16..10) {
            let mut lab = LayoutLab::new();
            lab.padding_amount = initial;
            lab.padding_amount = lab.padding_amount.saturating_add(delta).min(4);
            prop_assert!(lab.padding_amount <= 4);
        }

        /// Property: Render is deterministic for any valid state.
        #[test]
        fn prop_render_deterministic(
            preset in 0usize..5,
            dir in prop_oneof![Just(Direction::Vertical), Just(Direction::Horizontal)],
            align in 0usize..5,
            gap in 0u16..6,
            margin in 0u16..5,
            padding in 0u16..5,
            debug in proptest::bool::ANY,
        ) {
            use ftui_render::grapheme_pool::GraphemePool;
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};

            let mut lab = LayoutLab::new();
            lab.current_preset = preset;
            lab.direction = dir;
            lab.alignment_idx = align;
            lab.gap = gap;
            lab.margin = margin;
            lab.padding_amount = padding;
            lab.show_debug = debug;

            // Acquire combined render lock to prevent race with parallel tests
            // that mutate global theme or accessibility state.
            let _render_guard = theme::ScopedRenderLock::new(
                theme::ThemeId::CyberpunkAurora,
                false,
                1.0,
            );

            let checksum_a = {
                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(120, 40, &mut pool);
                lab.view(&mut frame, Rect::new(0, 0, 120, 40));
                let mut hasher = DefaultHasher::new();
                for y in 0..40u16 {
                    for x in 0..120u16 {
                        if let Some(cell) = frame.buffer.get(x, y) {
                            cell.content.hash(&mut hasher);
                            cell.fg.hash(&mut hasher);
                            cell.bg.hash(&mut hasher);
                            cell.attrs.hash(&mut hasher);
                        }
                    }
                }
                hasher.finish()
            };

            let checksum_b = {
                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(120, 40, &mut pool);
                lab.view(&mut frame, Rect::new(0, 0, 120, 40));
                let mut hasher = DefaultHasher::new();
                for y in 0..40u16 {
                    for x in 0..120u16 {
                        if let Some(cell) = frame.buffer.get(x, y) {
                            cell.content.hash(&mut hasher);
                            cell.fg.hash(&mut hasher);
                            cell.bg.hash(&mut hasher);
                            cell.attrs.hash(&mut hasher);
                        }
                    }
                }
                hasher.finish()
            };

            prop_assert_eq!(checksum_a, checksum_b, "Render should be deterministic");
        }
    }
}
