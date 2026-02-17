#![forbid(unsafe_code)]

//! Widget Gallery screen — showcases every available widget type.

use std::cell::Cell;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::WrapMode;
use ftui_widgets::Badge;
use ftui_widgets::StatefulWidget;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::columns::{Column, Columns};
use ftui_widgets::command_palette::{ActionItem, CommandPalette};
use ftui_widgets::constraint_overlay::ConstraintOverlay;
use ftui_widgets::emoji::Emoji;
use ftui_widgets::file_picker::{FilePicker, FilePickerState};
use ftui_widgets::group::Group;
use ftui_widgets::help::{Help as HelpWidget, HelpMode};
use ftui_widgets::history_panel::{HistoryPanel, HistoryPanelMode};
use ftui_widgets::input::TextInput;
use ftui_widgets::json_view::JsonView;
use ftui_widgets::layout::Layout;
use ftui_widgets::layout_debugger::{LayoutConstraints, LayoutDebugger, LayoutRecord};
use ftui_widgets::list::{List, ListItem};
use ftui_widgets::log_viewer::{LogViewer, LogViewerState, LogWrapMode};
use ftui_widgets::modal::{Dialog, DialogState};
use ftui_widgets::notification_queue::{
    NotificationPriority, NotificationQueue, NotificationStack, QueueConfig,
};
use ftui_widgets::paginator::{Paginator, PaginatorMode};
use ftui_widgets::panel::Panel;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::{MiniBar, MiniBarColors, ProgressBar};
use ftui_widgets::rule::Rule;
use ftui_widgets::scrollbar::{
    SCROLLBAR_PART_TRACK, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ftui_widgets::sparkline::Sparkline;
use ftui_widgets::spinner::SpinnerState;
use ftui_widgets::status_line::{StatusItem, StatusLine};
use ftui_widgets::stopwatch::{Stopwatch, StopwatchFormat, StopwatchState};
use ftui_widgets::table::{Row, Table};
use ftui_widgets::textarea::{TextArea, TextAreaState};
use ftui_widgets::timer::{Timer, TimerFormat, TimerState};
use ftui_widgets::toast::{Toast, ToastIcon, ToastPosition, ToastStyle};
use ftui_widgets::tree::{Tree, TreeGuides, TreeNode};
use ftui_widgets::validation_error::{ValidationErrorDisplay, ValidationErrorState};
use ftui_widgets::virtualized::{RenderItem, VirtualizedList, VirtualizedListState};
use std::cell::RefCell;
use std::time::Duration;

use super::{HelpEntry, Screen};
use crate::theme;
use crate::theme::{BadgeSpec, PriorityBadge, StatusBadge};

/// Number of gallery sections.
const SECTION_COUNT: usize = 8;

/// Section names.
const SECTION_NAMES: [&str; SECTION_COUNT] = [
    "A: Inputs",
    "B: Display",
    "C: Status",
    "D: Data Viz",
    "E: Navigation",
    "F: Layout",
    "G: Utility",
    "H: Advanced",
];

const VIRTUALIZED_SCROLLBAR_HIT_ID: HitId = HitId::new(0x1777);

#[derive(Debug, Clone)]
struct GalleryVirtualItem {
    label: &'static str,
    detail: &'static str,
}

impl RenderItem for GalleryVirtualItem {
    fn render(&self, area: Rect, frame: &mut Frame, selected: bool) {
        if area.is_empty() {
            return;
        }
        let prefix = theme::selection_indicator(selected);
        let text = format!("{prefix}{} — {}", self.label, self.detail);
        let style = theme::list_item_style(selected, true);
        Paragraph::new(text).style(style).render(area, frame);
    }
}

/// Widget Gallery screen state.
pub struct WidgetGallery {
    current_section: usize,
    tick_count: u64,
    spinner_state: SpinnerState,
    file_picker_state: RefCell<Option<FilePickerState>>,
    log_viewer: RefCell<LogViewer>,
    log_viewer_state: RefCell<LogViewerState>,
    virtualized_items: Vec<GalleryVirtualItem>,
    virtualized_state: RefCell<VirtualizedListState>,
    layout_tabs: Cell<Rect>,
    layout_virtualized: Cell<Rect>,
}

impl Default for WidgetGallery {
    fn default() -> Self {
        Self::new()
    }
}

impl WidgetGallery {
    pub fn new() -> Self {
        let file_picker_state = FilePickerState::from_path(".")
            .map(|mut state| {
                // Keep the widget-gallery snapshots deterministic even when the crate
                // directory contains local artifacts (e.g. proptest regressions).
                state.entries.retain(|entry| {
                    !matches!(entry.name.as_str(), "proptest-regressions" | "a.out")
                });
                state
            })
            .ok();

        let mut log_viewer = LogViewer::new(128).wrap_mode(LogWrapMode::WordWrap);
        let sample_logs = [
            "INFO  boot: FrankenTUI demo started",
            "DEBUG layout: resolved 6 constraints",
            "WARN  io: slow disk detected, using cache",
            "INFO  render: diff cells=512 runs=18 bytes=9.6KB",
            "INFO  net: connected to telemetry endpoint",
            "ERROR auth: token expired, refresh scheduled",
        ];
        for line in sample_logs {
            log_viewer.push(line);
        }

        let virtualized_items = vec![
            GalleryVirtualItem {
                label: "Item 0001",
                detail: "CPU 72%",
            },
            GalleryVirtualItem {
                label: "Item 0002",
                detail: "Mem 48%",
            },
            GalleryVirtualItem {
                label: "Item 0003",
                detail: "IO 35%",
            },
            GalleryVirtualItem {
                label: "Item 0004",
                detail: "GPU 18%",
            },
            GalleryVirtualItem {
                label: "Item 0005",
                detail: "Net 2.1MB/s",
            },
            GalleryVirtualItem {
                label: "Item 0006",
                detail: "FPS 120",
            },
        ];

        let mut virtualized_state = VirtualizedListState::new();
        virtualized_state.selected = Some(2);

        Self {
            current_section: 0,
            tick_count: 0,
            spinner_state: SpinnerState::default(),
            file_picker_state: RefCell::new(file_picker_state),
            log_viewer: RefCell::new(log_viewer),
            log_viewer_state: RefCell::new(LogViewerState::default()),
            virtualized_items,
            virtualized_state: RefCell::new(virtualized_state),
            layout_tabs: Cell::new(Rect::default()),
            layout_virtualized: Cell::new(Rect::default()),
        }
    }
}

impl Screen for WidgetGallery {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let tabs = self.layout_tabs.get();
                if !tabs.is_empty() && tabs.contains(mouse.x, mouse.y) && tabs.width > 0 {
                    let rel = mouse.x.saturating_sub(tabs.x) as usize;
                    let idx = (rel * SECTION_COUNT) / tabs.width as usize;
                    if idx < SECTION_COUNT {
                        self.current_section = idx;
                    }
                }
            }

            // Wire the virtualized list mouse semantics (including scrollbar drag/click)
            // in the Advanced section.
            if self.current_section == 7 {
                let viewport = self.layout_virtualized.get();
                if !viewport.is_empty() && viewport.contains(mouse.x, mouse.y) {
                    let total_items = self.virtualized_items.len();
                    let fixed_height = 1u16;
                    let viewport_height = viewport.height;
                    let items_per_viewport = viewport_height.div_ceil(fixed_height.max(1)) as usize;
                    let needs_scrollbar = total_items > items_per_viewport;

                    let hit = if needs_scrollbar
                        && viewport.width > 0
                        && mouse.x == viewport.right().saturating_sub(1)
                    {
                        let track_len = u64::from(viewport_height);
                        let track_pos = u64::from(mouse.y.saturating_sub(viewport.y));
                        let data = (SCROLLBAR_PART_TRACK << 56)
                            | ((track_len & 0x0FFF_FFFF) << 28)
                            | (track_pos & 0x0FFF_FFFF);
                        Some((VIRTUALIZED_SCROLLBAR_HIT_ID, HitRegion::Scrollbar, data))
                    } else {
                        None
                    };

                    let mut state = self.virtualized_state.borrow_mut();
                    let _ = state.handle_mouse(
                        mouse,
                        hit,
                        VIRTUALIZED_SCROLLBAR_HIT_ID,
                        total_items,
                        viewport_height,
                        fixed_height,
                    );
                }
            }
            return Cmd::None;
        }

        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            // Section-specific widget navigation
            if self.current_section == 7 {
                // Advanced section: VirtualizedList navigation
                match code {
                    KeyCode::Up => {
                        let mut state = self.virtualized_state.borrow_mut();
                        let curr = state.selected.unwrap_or(0);
                        state.selected = Some(curr.saturating_sub(1));
                        return Cmd::None;
                    }
                    KeyCode::Down => {
                        let mut state = self.virtualized_state.borrow_mut();
                        let curr = state.selected.unwrap_or(0);
                        if curr + 1 < self.virtualized_items.len() {
                            state.selected = Some(curr + 1);
                        }
                        return Cmd::None;
                    }
                    _ => {}
                }
            }

            match code {
                KeyCode::Char('j') | KeyCode::Right | KeyCode::Down => {
                    self.current_section = (self.current_section + 1) % SECTION_COUNT;
                }
                KeyCode::Char('k') | KeyCode::Left | KeyCode::Up => {
                    self.current_section = if self.current_section == 0 {
                        SECTION_COUNT - 1
                    } else {
                        self.current_section - 1
                    };
                }
                _ => {}
            }
        }
        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.spinner_state.tick();
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.height < 5 || area.width < 20 {
            let msg = Paragraph::new("Terminal too small").style(theme::muted());
            msg.render(area, frame);
            return;
        }

        // Vertical: section tabs (1) + content + paginator (1)
        let v_chunks = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(4),
                Constraint::Fixed(1),
            ])
            .split(area);

        self.layout_tabs.set(v_chunks[0]);
        self.render_section_tabs(frame, v_chunks[0]);
        self.render_section_content(frame, v_chunks[1]);
        self.render_paginator(frame, v_chunks[2]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "j/↓/→",
                action: "Next section",
            },
            HelpEntry {
                key: "k/↑/←",
                action: "Previous section",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click tab to switch",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Widget Gallery"
    }

    fn tab_label(&self) -> &'static str {
        "Widgets"
    }
}

impl WidgetGallery {
    fn render_section_tabs(&self, frame: &mut Frame, area: Rect) {
        let mut tab_text = String::new();
        for (i, name) in SECTION_NAMES.iter().enumerate() {
            if i > 0 {
                tab_text.push_str(" │ ");
            }
            if i == self.current_section {
                tab_text.push_str(&format!("▸ {name}"));
            } else {
                tab_text.push_str(&format!("  {name}"));
            }
        }
        let style = if self.current_section < SECTION_COUNT {
            Style::new()
                .fg(theme::screen_accent::WIDGET_GALLERY)
                .attrs(StyleFlags::BOLD)
        } else {
            theme::muted()
        };
        Paragraph::new(tab_text).style(style).render(area, frame);
    }

    fn render_section_content(&self, frame: &mut Frame, area: Rect) {
        self.layout_virtualized.set(Rect::default());
        match self.current_section {
            0 => self.render_inputs(frame, area),
            1 => self.render_display_widgets(frame, area),
            2 => self.render_status_widgets(frame, area),
            3 => self.render_data_viz(frame, area),
            4 => self.render_navigation_widgets(frame, area),
            5 => self.render_layout_widgets(frame, area),
            6 => self.render_utility_widgets(frame, area),
            7 => self.render_advanced_widgets(frame, area),
            _ => {}
        }
    }

    fn render_paginator(&self, frame: &mut Frame, area: Rect) {
        let pag = Paginator::with_pages((self.current_section as u64) + 1, SECTION_COUNT as u64)
            .mode(PaginatorMode::Dots)
            .style(Style::new().fg(theme::fg::MUTED));
        pag.render(area, frame);
    }

    // -----------------------------------------------------------------------
    // Section B: Display
    // -----------------------------------------------------------------------
    fn render_display_widgets(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([Constraint::Percentage(45.0), Constraint::Percentage(55.0)])
            .split(area);

        let top_cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(rows[0]);
        self.render_borders(frame, top_cols[0]);
        self.render_text_styles(frame, top_cols[1]);

        let bottom_cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(34.0),
                Constraint::Percentage(33.0),
                Constraint::Percentage(33.0),
            ])
            .split(rows[1]);
        self.render_paragraph_wraps(frame, bottom_cols[0]);
        self.render_lists_and_table(frame, bottom_cols[1]);
        self.render_code_and_quote(frame, bottom_cols[2]);
    }

    // -----------------------------------------------------------------------
    // Section A: Borders
    // -----------------------------------------------------------------------
    fn render_borders(&self, frame: &mut Frame, area: Rect) {
        let border_types = [
            ("ASCII", BorderType::Ascii),
            ("Square", BorderType::Square),
            ("Rounded", BorderType::Rounded),
            ("Double", BorderType::Double),
            ("Heavy", BorderType::Heavy),
            ("Custom", BorderType::Rounded),
        ];

        let alignments = [
            Alignment::Left,
            Alignment::Center,
            Alignment::Right,
            Alignment::Left,
            Alignment::Center,
            Alignment::Right,
        ];

        let colors = [
            theme::accent::PRIMARY,
            theme::accent::SECONDARY,
            theme::accent::SUCCESS,
            theme::accent::WARNING,
            theme::accent::ERROR,
            theme::accent::INFO,
        ];

        // 2 rows of 3
        let rows = Flex::vertical()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);

        for (row_idx, row_area) in rows.iter().enumerate().take(2) {
            let cols = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(33.3),
                    Constraint::Percentage(33.3),
                    Constraint::Percentage(33.4),
                ])
                .split(*row_area);

            for (col_idx, col_area) in cols.iter().enumerate().take(3) {
                let i = row_idx * 3 + col_idx;
                let (name, bt) = border_types[i];
                let block = Block::new()
                    .borders(Borders::ALL)
                    .border_type(bt)
                    .title(name)
                    .title_alignment(alignments[i])
                    .style(Style::new().fg(colors[i]));
                let inner = block.inner(*col_area);
                block.render(*col_area, frame);

                let desc = format!("Border: {name}\nAlign: {:?}", alignments[i]);
                Paragraph::new(desc)
                    .style(theme::body())
                    .render(inner, frame);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Section B: Text Styles
    // -----------------------------------------------------------------------
    fn render_text_styles(&self, frame: &mut Frame, area: Rect) {
        let styles: Vec<(&str, Style)> = vec![
            ("Bold", theme::bold()),
            ("Dim", theme::dim()),
            ("Italic", theme::italic()),
            ("Underline", theme::underline()),
            ("DblUnder", theme::double_underline()),
            ("CurlyUL", theme::curly_underline()),
            ("Blink", theme::blink_style()),
            ("Reverse", theme::reverse()),
            ("Hidden", theme::hidden()),
            ("Strike", theme::strikethrough()),
            (
                "Bold+Italic",
                Style::new()
                    .fg(theme::accent::PRIMARY)
                    .attrs(StyleFlags::BOLD | StyleFlags::ITALIC),
            ),
            (
                "Bold+Under",
                Style::new()
                    .fg(theme::accent::SECONDARY)
                    .attrs(StyleFlags::BOLD | StyleFlags::UNDERLINE),
            ),
            (
                "Dim+Italic",
                Style::new()
                    .fg(theme::accent::SUCCESS)
                    .attrs(StyleFlags::DIM | StyleFlags::ITALIC),
            ),
            (
                "All Flags",
                Style::new().fg(theme::accent::WARNING).attrs(
                    StyleFlags::BOLD
                        | StyleFlags::ITALIC
                        | StyleFlags::UNDERLINE
                        | StyleFlags::STRIKETHROUGH,
                ),
            ),
        ];

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Text Style Flags")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Grid: fill rows, ~3 columns
        let col_width = 16u16;
        let cols_per_row = (inner.width / col_width).max(1) as usize;

        for (i, (label, style)) in styles.iter().enumerate() {
            let row = (i / cols_per_row) as u16;
            let col = (i % cols_per_row) as u16;
            let x = inner.x + col * col_width;
            let y = inner.y + row;
            if y >= inner.y + inner.height {
                break;
            }
            let cell_area = Rect {
                x,
                y,
                width: col_width.min(inner.x + inner.width - x),
                height: 1,
            };
            Paragraph::new(*label)
                .style(*style)
                .render(cell_area, frame);
        }
    }

    fn render_paragraph_wraps(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Paragraph Wrap")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(34.0),
                Constraint::Percentage(33.0),
                Constraint::Percentage(33.0),
            ])
            .split(inner);
        let sample = "The quick brown fox jumps over the lazy dog.";
        let modes = [
            ("Word", WrapMode::Word),
            ("Char", WrapMode::Char),
            ("None", WrapMode::None),
        ];

        for (row, (label, mode)) in rows.iter().zip(modes.iter()) {
            let text = format!("{label}: {sample}");
            Paragraph::new(text)
                .wrap(*mode)
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(*row, frame);
        }
    }

    fn render_lists_and_table(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([Constraint::Percentage(45.0), Constraint::Percentage(55.0)])
            .split(area);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Lists")
            .style(theme::content_border());
        let inner = block.inner(rows[0]);
        block.render(rows[0], frame);

        let items = vec![
            ListItem::new("• Bullet item"),
            ListItem::new("• Second item"),
            ListItem::new("  ◦ Nested item"),
            ListItem::new("1. Numbered item"),
            ListItem::new("2. Another item"),
        ];
        Widget::render(
            &List::new(items).style(Style::new().fg(theme::fg::SECONDARY)),
            inner,
            frame,
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Table")
            .style(theme::content_border());
        let inner = block.inner(rows[1]);
        block.render(rows[1], frame);

        let header = Row::new(["Name", "Type", "Size"]).style(theme::title());
        let table_rows = vec![
            Row::new(["main.rs", "Rust", "4.2 KB"]),
            Row::new(["README.md", "MD", "3.4 KB"]),
            Row::new(["config.toml", "TOML", "1.1 KB"]),
        ];
        let widths = [
            Constraint::Min(8),
            Constraint::Fixed(5),
            Constraint::Fixed(7),
        ];
        Widget::render(
            &Table::new(table_rows, widths)
                .header(header)
                .style(Style::new().fg(theme::fg::SECONDARY))
                .theme(theme::table_theme_demo())
                .theme_phase(theme::table_theme_phase(self.tick_count)),
            inner,
            frame,
        );
    }

    fn render_code_and_quote(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([Constraint::Percentage(55.0), Constraint::Percentage(45.0)])
            .split(area);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Code Block")
            .style(theme::content_border());
        let inner = block.inner(rows[0]);
        block.render(rows[0], frame);
        Paragraph::new("fn main() {\n    println!(\"hello\");\n}")
            .style(Style::new().fg(theme::accent::INFO))
            .render(inner, frame);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Blockquote + Rule")
            .style(theme::content_border());
        let inner = block.inner(rows[1]);
        block.render(rows[1], frame);
        Paragraph::new("> Deterministic output beats clever output.")
            .style(Style::new().fg(theme::fg::MUTED))
            .render(inner, frame);

        if inner.height > 1 {
            let rule_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
            Rule::new()
                .style(Style::new().fg(theme::fg::MUTED))
                .render(rule_area, frame);
        }
    }

    // -----------------------------------------------------------------------
    // Status Palette + Badges
    // -----------------------------------------------------------------------
    fn render_colors(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(3),
                Constraint::Fixed(1),
                Constraint::Min(2),
                Constraint::Fixed(1),
                Constraint::Fixed(3),
            ])
            .split(area);

        // TrueColor gradient strip
        self.render_color_gradient(frame, rows[0]);

        // Separator: named colors
        Rule::new()
            .title("Named Colors")
            .title_alignment(Alignment::Center)
            .style(theme::muted())
            .render(rows[1], frame);

        // Named accent colors
        self.render_named_colors(frame, rows[2]);

        // Separator: semantic badges
        Rule::new()
            .title("Semantic Badges")
            .title_alignment(Alignment::Center)
            .style(theme::muted())
            .render(rows[3], frame);

        self.render_badges(frame, rows[4]);
    }

    fn render_color_gradient(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Theme Accent Gradient")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let w = inner.width as usize;
        for i in 0..w {
            let t = i as f64 / w.max(1) as f64;
            let color = theme::accent_gradient(t);
            // Write each cell with its color using the frame buffer directly
            let x = inner.x + i as u16;
            if x < inner.x + inner.width {
                let cell_area = Rect {
                    x,
                    y: inner.y,
                    width: 1,
                    height: 1,
                };
                Paragraph::new("█")
                    .style(Style::new().fg(color))
                    .render(cell_area, frame);
            }
        }
    }

    fn render_named_colors(&self, frame: &mut Frame, area: Rect) {
        let named = [
            ("Primary", theme::accent::PRIMARY),
            ("Secondary", theme::accent::SECONDARY),
            ("Success", theme::accent::SUCCESS),
            ("Warning", theme::accent::WARNING),
            ("Error", theme::accent::ERROR),
            ("Info", theme::accent::INFO),
            ("Link", theme::accent::LINK),
            ("FG Primary", theme::fg::PRIMARY),
            ("FG Secondary", theme::fg::SECONDARY),
            ("FG Muted", theme::fg::MUTED),
        ];

        let col_width = 16u16;
        let cols_per_row = (area.width / col_width).max(1) as usize;

        for (i, (label, color)) in named.iter().enumerate() {
            let row = (i / cols_per_row) as u16;
            let col = (i % cols_per_row) as u16;
            let x = area.x + col * col_width;
            let y = area.y + row;
            if y >= area.y + area.height {
                break;
            }
            let cell_area = Rect {
                x,
                y,
                width: col_width.min(area.x + area.width - x),
                height: 1,
            };
            let text = format!("██ {label}");
            Paragraph::new(text)
                .style(Style::new().fg(*color))
                .render(cell_area, frame);
        }
    }

    fn render_badges(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let status_row = Rect::new(area.x, area.y, area.width, 1);
        let priority_row = Rect::new(area.x, area.y.saturating_add(1), area.width, 1);

        let status_badges = [
            theme::status_badge(StatusBadge::Open),
            theme::status_badge(StatusBadge::InProgress),
            theme::status_badge(StatusBadge::Blocked),
            theme::status_badge(StatusBadge::Closed),
        ];

        let priority_badges = [
            theme::priority_badge(PriorityBadge::P0),
            theme::priority_badge(PriorityBadge::P1),
            theme::priority_badge(PriorityBadge::P2),
            theme::priority_badge(PriorityBadge::P3),
            theme::priority_badge(PriorityBadge::P4),
        ];

        self.render_badge_row(frame, status_row, &status_badges);
        self.render_badge_row(frame, priority_row, &priority_badges);
    }

    fn render_badge_row(&self, frame: &mut Frame, area: Rect, badges: &[BadgeSpec]) {
        if area.is_empty() {
            return;
        }

        let mut x = area.x;
        let max_x = area.right();
        let y = area.y;

        for badge_spec in badges {
            let badge = Badge::new(badge_spec.label).with_style(badge_spec.style);
            let w = badge.width().min(area.width);
            if x >= max_x || x.saturating_add(w) > max_x {
                break;
            }
            badge.render(Rect::new(x, y, w, 1), frame);
            x = x.saturating_add(w).saturating_add(1);
        }
    }

    // -----------------------------------------------------------------------
    // Section C: Status
    // -----------------------------------------------------------------------
    fn render_status_widgets(&self, frame: &mut Frame, area: Rect) {
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(55.0), Constraint::Percentage(45.0)])
            .split(area);

        self.render_colors(frame, cols[0]);
        self.render_status_activity(frame, cols[1]);
    }

    fn render_status_activity(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(7),
                Constraint::Fixed(3),
                Constraint::Min(3),
            ])
            .split(area);

        self.render_progress_bars(frame, rows[0]);
        self.render_spinners(frame, rows[1]);
        self.render_toast_demo(frame, rows[2]);
    }

    fn render_spinners(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Spinners")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 {
            return;
        }

        let top_row = Rect::new(inner.x, inner.y, inner.width, 1);
        let dots_idx = self.spinner_state.current_frame % ftui_widgets::spinner::DOTS.len();
        let dots_frame = ftui_widgets::spinner::DOTS[dots_idx];
        Paragraph::new(format!("{dots_frame} Loading (DOTS)"))
            .style(Style::new().fg(theme::accent::PRIMARY))
            .render(top_row, frame);

        if inner.height >= 2 {
            let bot_row = Rect::new(inner.x, inner.y + 1, inner.width, 1);
            let line_idx = self.spinner_state.current_frame % ftui_widgets::spinner::LINE.len();
            let line_frame = ftui_widgets::spinner::LINE[line_idx];
            Paragraph::new(format!("{line_frame} Processing (LINE)"))
                .style(Style::new().fg(theme::accent::SECONDARY))
                .render(bot_row, frame);
        }
    }

    fn render_toast_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Toast")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        let toast = Toast::new("Settings saved successfully")
            .title("Success")
            .icon(ToastIcon::Success)
            .persistent();
        toast.render(inner, frame);
    }

    // -----------------------------------------------------------------------
    // Section A: Inputs
    // -----------------------------------------------------------------------
    fn render_inputs(&self, frame: &mut Frame, area: Rect) {
        let cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(38.0),
                Constraint::Percentage(32.0),
                Constraint::Percentage(30.0),
            ])
            .split(area);

        let left_rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(20.0),
                Constraint::Percentage(45.0),
                Constraint::Percentage(35.0),
            ])
            .split(cols[0]);
        self.render_text_inputs(frame, left_rows[0]);
        self.render_text_area(frame, left_rows[1]);
        let left_bottom = Flex::horizontal()
            .constraints([Constraint::Percentage(55.0), Constraint::Percentage(45.0)])
            .split(left_rows[2]);
        self.render_input_controls(frame, left_bottom[0]);
        self.render_validation_timer_demo(frame, left_bottom[1]);

        let mid_rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(36.0),
                Constraint::Percentage(34.0),
                Constraint::Percentage(30.0),
            ])
            .split(cols[1]);
        self.render_command_palette_demo(frame, mid_rows[0]);
        self.render_file_picker_demo(frame, mid_rows[1]);
        self.render_notification_stack_demo(frame, mid_rows[2]);

        let right_rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(40.0),
                Constraint::Percentage(25.0),
                Constraint::Percentage(35.0),
            ])
            .split(cols[2]);
        self.render_log_viewer_demo(frame, right_rows[0]);
        self.render_progress_bars(frame, right_rows[1]);
        let right_bottom = Flex::vertical()
            .constraints([Constraint::Fixed(3), Constraint::Min(1)])
            .split(right_rows[2]);
        self.render_spinners(frame, right_bottom[0]);
        self.render_toast_demo(frame, right_bottom[1]);
    }

    fn render_text_inputs(&self, frame: &mut Frame, area: Rect) {
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);

        // Plain text input
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("TextInput")
            .style(theme::content_border());
        let inner = block.inner(cols[0]);
        block.render(cols[0], frame);
        TextInput::new()
            .with_value("demo@example.com")
            .with_placeholder("name@example.com")
            .with_style(Style::new().fg(theme::fg::PRIMARY))
            .with_cursor_style(Style::new().fg(theme::accent::PRIMARY))
            .with_focused(true)
            .render(inner, frame);

        // Masked input
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Masked Input")
            .style(theme::content_border());
        let inner = block.inner(cols[1]);
        block.render(cols[1], frame);
        TextInput::new()
            .with_value("s3cr3t")
            .with_mask('•')
            .with_style(Style::new().fg(theme::fg::PRIMARY))
            .with_placeholder("••••••••")
            .render(inner, frame);
    }

    fn render_text_area(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("TextArea")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        let text =
            "FrankenTUI text area\n- Multi-line editing\n- Soft wrap enabled\n- Line numbers";
        let text_area = TextArea::new()
            .with_text(text)
            .with_focus(true)
            .with_line_numbers(true)
            .with_soft_wrap(true)
            .with_style(Style::new().fg(theme::fg::PRIMARY))
            .with_cursor_line_style(Style::new().bg(theme::alpha::SURFACE));
        let mut state = TextAreaState::default();
        StatefulWidget::render(&text_area, inner, frame, &mut state);
    }

    fn render_input_controls(&self, frame: &mut Frame, area: Rect) {
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Checkboxes + Radios")
            .style(theme::content_border());
        let inner = block.inner(cols[0]);
        block.render(cols[0], frame);
        Paragraph::new(
            "[ ] Enable autosave\n\
             [x] Sync on save\n\
             ( ) Light theme\n\
             (*) Dark theme",
        )
        .style(Style::new().fg(theme::fg::SECONDARY))
        .render(inner, frame);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Select + Number + Slider")
            .style(theme::content_border());
        let inner = block.inner(cols[1]);
        block.render(cols[1], frame);
        Paragraph::new(
            "Select: [High ▾]\n\
             Number: [-]  42  [+]\n\
             Slider: 30%  [■■■■■─────]",
        )
        .style(Style::new().fg(theme::fg::SECONDARY))
        .render(inner, frame);
    }

    fn render_tree(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Tree (Unicode Guides)")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        let root = TreeNode::new("project/")
            .child(
                TreeNode::new("src/")
                    .child(TreeNode::new("main.rs"))
                    .child(TreeNode::new("lib.rs"))
                    .child(
                        TreeNode::new("screens/")
                            .child(TreeNode::new("dashboard.rs"))
                            .child(TreeNode::new("gallery.rs")),
                    ),
            )
            .child(TreeNode::new("tests/").child(TreeNode::new("integration.rs")))
            .child(TreeNode::new("Cargo.toml"));
        let tree = Tree::new(root)
            .with_guides(TreeGuides::Unicode)
            .with_label_style(Style::new().fg(theme::fg::PRIMARY));
        tree.render(inner, frame);
    }

    // -----------------------------------------------------------------------
    // Section D: Data Viz
    // -----------------------------------------------------------------------
    fn render_data_viz(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(3),
                Constraint::Fixed(4),
                Constraint::Min(4),
            ])
            .split(area);

        self.render_sparklines(frame, rows[0]);
        self.render_mini_bars(frame, rows[1]);
        self.render_json_view(frame, rows[2]);
    }

    fn render_sparklines(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Sparklines")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Fixed(1)])
            .split(inner);
        let data_up = [1.0, 2.0, 4.0, 3.0, 6.0, 5.0, 7.0, 8.0];
        let data_down = [8.0, 7.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];

        Sparkline::new(&data_up)
            .style(Style::new().fg(theme::accent::PRIMARY))
            .render(rows[0], frame);
        if rows.len() > 1 {
            Sparkline::new(&data_down)
                .style(Style::new().fg(theme::accent::WARNING))
                .render(rows[1], frame);
        }
    }

    fn render_mini_bars(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Mini Bars")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .split(inner);
        let metrics = [("CPU", 0.72), ("Mem", 0.48), ("IO", 0.35)];
        let colors = MiniBarColors::new(
            theme::accent::SUCCESS.into(),
            theme::accent::WARNING.into(),
            theme::accent::INFO.into(),
            theme::accent::ERROR.into(),
        );

        for (row, (label, ratio)) in rows.iter().zip(metrics.iter()) {
            if row.is_empty() {
                continue;
            }

            let label_width = 5_u16.min(row.width);
            let label_area = Rect::new(row.x, row.y, label_width, 1);
            let bar_area = Rect::new(
                row.x.saturating_add(label_width),
                row.y,
                row.width.saturating_sub(label_width),
                1,
            );

            Paragraph::new(label.to_string())
                .style(Style::new().fg(theme::fg::MUTED))
                .render(label_area, frame);

            if !bar_area.is_empty() {
                let bar = MiniBar::new(*ratio, bar_area.width)
                    .show_percent(true)
                    .colors(colors)
                    .style(Style::new().fg(theme::fg::PRIMARY));
                bar.render(bar_area, frame);
            }
        }
    }

    fn render_progress_bars(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("ProgressBar")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 {
            return;
        }

        let ratios = [0.0, 0.25, 0.50, 0.75, 1.0];
        let colors = [
            theme::accent::ERROR,
            theme::accent::WARNING,
            theme::accent::INFO,
            theme::accent::PRIMARY,
            theme::accent::SUCCESS,
        ];

        let bar_rows = Flex::vertical()
            .constraints(
                ratios
                    .iter()
                    .map(|_| Constraint::Fixed(1))
                    .collect::<Vec<_>>(),
            )
            .split(inner);

        for (i, (&ratio, &color)) in ratios.iter().zip(colors.iter()).enumerate() {
            if i >= bar_rows.len() {
                break;
            }
            let pct = (ratio * 100.0) as u32;
            let label = format!("{pct}%");
            ProgressBar::new()
                .ratio(ratio)
                .label(&label)
                .style(Style::new().fg(theme::fg::MUTED))
                .gauge_style(Style::new().fg(color).bg(theme::alpha::SURFACE))
                .render(bar_rows[i], frame);
        }
    }

    fn render_json_view(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("JsonView")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        let sample_json = r#"{"name": "FrankenTUI", "version": "0.1.0", "widgets": 28, "features": ["charts", "forms", "canvas"], "nested": {"key": "value"}}"#;
        JsonView::new(sample_json)
            .with_indent(2)
            .with_key_style(
                Style::new()
                    .fg(theme::accent::PRIMARY)
                    .attrs(StyleFlags::BOLD),
            )
            .with_string_style(Style::new().fg(theme::accent::SUCCESS))
            .with_number_style(Style::new().fg(theme::accent::WARNING))
            .with_literal_style(Style::new().fg(theme::accent::ERROR))
            .with_punct_style(Style::new().fg(theme::fg::MUTED))
            .render(inner, frame);
    }

    // -----------------------------------------------------------------------
    // Section E: Navigation
    // -----------------------------------------------------------------------
    fn render_navigation_widgets(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(3),
                Constraint::Min(4),
                Constraint::Fixed(3),
            ])
            .split(area);

        self.render_tabs_and_breadcrumbs(frame, rows[0]);
        self.render_tree(frame, rows[1]);
        self.render_paginator_modes(frame, rows[2]);
    }

    fn render_tabs_and_breadcrumbs(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Tabs + Breadcrumbs")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.height == 0 {
            return;
        }

        let tab_row = Rect::new(inner.x, inner.y, inner.width, 1);
        Paragraph::new(" [Widgets]  Logs  Settings ")
            .style(Style::new().fg(theme::accent::PRIMARY))
            .render(tab_row, frame);

        if inner.height >= 2 {
            let crumb_row = Rect::new(inner.x, inner.y + 1, inner.width, 1);
            Paragraph::new("Home / Gallery / Widgets")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(crumb_row, frame);
        }
    }

    // -----------------------------------------------------------------------
    // Section F: Layout Widgets
    // -----------------------------------------------------------------------
    fn render_layout_widgets(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(30.0),
                Constraint::Percentage(25.0),
                Constraint::Percentage(25.0),
                Constraint::Percentage(20.0),
            ])
            .split(area);

        // Columns demo
        self.render_columns_demo(frame, rows[0]);
        // Flex demo
        self.render_flex_demo(frame, rows[1]);
        // Grid layout demo
        self.render_grid_demo(frame, rows[2]);
        // Panel/padding demo
        self.render_panel_demo(frame, rows[3]);
    }

    fn render_columns_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Columns Widget")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        let col1 =
            Paragraph::new("Column 1\nFixed(15)").style(Style::new().fg(theme::accent::PRIMARY));
        let col2 =
            Paragraph::new("Column 2\nMin(10)").style(Style::new().fg(theme::accent::SECONDARY));
        let col3 = Paragraph::new("Column 3\nPercentage(40%)")
            .style(Style::new().fg(theme::accent::SUCCESS));

        let columns = Columns::new()
            .push(Column::new(col1, Constraint::Fixed(15)))
            .push(Column::new(col2, Constraint::Min(10)))
            .push(Column::new(col3, Constraint::Percentage(40.0)))
            .gap(theme::spacing::XS);
        columns.render(inner, frame);
    }

    fn render_flex_demo(&self, frame: &mut Frame, area: Rect) {
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);

        // Horizontal flex
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Flex Horizontal")
            .style(theme::content_border());
        let inner = block.inner(cols[0]);
        block.render(cols[0], frame);

        let h_chunks = Flex::horizontal()
            .constraints([
                Constraint::Fixed(8),
                Constraint::Min(4),
                Constraint::Fixed(8),
            ])
            .split(inner);
        for (i, &color) in [
            theme::accent::PRIMARY,
            theme::accent::SECONDARY,
            theme::accent::SUCCESS,
        ]
        .iter()
        .enumerate()
        {
            if i < h_chunks.len() {
                Paragraph::new(format!("H{}", i + 1))
                    .style(Style::new().fg(color).attrs(StyleFlags::BOLD))
                    .render(h_chunks[i], frame);
            }
        }

        // Vertical flex
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Flex Vertical")
            .style(theme::content_border());
        let inner = block.inner(cols[1]);
        block.render(cols[1], frame);

        let v_chunks = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(1),
                Constraint::Fixed(1),
            ])
            .split(inner);
        for (i, &color) in [
            theme::accent::WARNING,
            theme::accent::ERROR,
            theme::accent::INFO,
        ]
        .iter()
        .enumerate()
        {
            if i < v_chunks.len() {
                Paragraph::new(format!("V{}", i + 1))
                    .style(Style::new().fg(color).attrs(StyleFlags::BOLD))
                    .render(v_chunks[i], frame);
            }
        }
    }

    fn render_grid_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Grid + Constraints")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        let layout = Layout::new()
            .rows([
                Constraint::Fixed(1),
                Constraint::Min(1),
                Constraint::Fixed(1),
            ])
            .columns([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .child(
                Panel::new(Paragraph::new("Header").style(theme::title()))
                    .border_type(BorderType::Rounded),
                0,
                0,
                1,
                2,
            )
            .child(
                Panel::new(Paragraph::new("Left")).border_type(BorderType::Rounded),
                1,
                0,
                1,
                1,
            )
            .child(
                Panel::new(Paragraph::new("Right")).border_type(BorderType::Rounded),
                1,
                1,
                1,
                1,
            )
            .child(
                Panel::new(Paragraph::new("Footer")).border_type(BorderType::Rounded),
                2,
                0,
                1,
                2,
            );
        if inner.width < 6 || inner.height < 4 {
            layout.render(inner, frame);
            return;
        }

        let mut debugger = LayoutDebugger::new();
        debugger.set_enabled(true);

        let header_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let footer_y = inner.y.saturating_add(inner.height.saturating_sub(1));
        let footer_area = Rect::new(inner.x, footer_y, inner.width, 1);
        let mid_y = inner.y.saturating_add(1);
        let mid_height = inner.height.saturating_sub(2);
        let left_width = inner.width / 2;
        let right_width = inner.width.saturating_sub(left_width);
        let left_area = Rect::new(inner.x, mid_y, left_width, mid_height);
        let right_area = Rect::new(
            inner.x.saturating_add(left_width),
            mid_y,
            right_width,
            mid_height,
        );

        let record = LayoutRecord::new("Grid", inner, inner, LayoutConstraints::new(0, 0, 0, 0))
            .with_child(LayoutRecord::new(
                "Header",
                header_area,
                header_area,
                LayoutConstraints::new(8, 0, 1, 1),
            ))
            .with_child(LayoutRecord::new(
                "Left",
                left_area,
                left_area,
                LayoutConstraints::new(left_width.saturating_add(2), 0, 0, 0),
            ))
            .with_child(LayoutRecord::new(
                "Right",
                right_area,
                right_area,
                LayoutConstraints::new(0, right_width.saturating_sub(2), 0, 0),
            ))
            .with_child(LayoutRecord::new(
                "Footer",
                footer_area,
                footer_area,
                LayoutConstraints::new(12, 0, 1, 1),
            ));
        debugger.record(record);

        Group::new()
            .push(layout)
            .push(ConstraintOverlay::new(&debugger))
            .render(inner, frame);
    }

    fn render_panel_demo(&self, frame: &mut Frame, area: Rect) {
        let pad_x = theme::spacing::SM;
        let pad_y = theme::spacing::XS;

        let content = Paragraph::new(
            "Panels wrap content with borders,\n\
             titles, and inner padding.\n\
             Useful for cards and tool panes.",
        )
        .style(Style::new().fg(theme::accent::INFO));

        let panel = Panel::new(content)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Panel Card")
            .subtitle("padding demo")
            .padding((pad_y, pad_x))
            .border_style(Style::new().fg(theme::accent::SECONDARY))
            .style(Style::new().bg(theme::alpha::SURFACE));

        panel.render(area, frame);
    }

    // -----------------------------------------------------------------------
    // Section G: Utility Widgets
    // -----------------------------------------------------------------------
    fn render_utility_widgets(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(2),
                Constraint::Fixed(3),
                Constraint::Min(2),
            ])
            .split(area);

        // Rule
        Rule::new()
            .title("Horizontal Rule")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::accent::SECONDARY))
            .render(rows[0], frame);

        self.render_status_line(frame, rows[1]);
        self.render_scrollbar_demo(frame, rows[2]);
        self.render_stopwatch_demo(frame, rows[3]);
    }

    fn render_status_line(&self, frame: &mut Frame, area: Rect) {
        let status = StatusLine::new()
            .left(StatusItem::text("[NORMAL]"))
            .left(StatusItem::key_hint("^S", "Save"))
            .center(StatusItem::text("widget_gallery.rs"))
            .right(StatusItem::progress(42, 100))
            .right(StatusItem::text("Ln 12, Col 4"))
            .style(
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::SURFACE),
            )
            .separator("  ");
        status.render(area, frame);
    }

    fn render_stopwatch_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Stopwatch")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        let mut state = StopwatchState::new();
        state.start();
        state.tick(Duration::from_secs(93));

        let widget = Stopwatch::new()
            .format(StopwatchFormat::Digital)
            .label("Uptime ")
            .running_style(Style::new().fg(theme::accent::SUCCESS))
            .stopped_style(Style::new().fg(theme::fg::MUTED));
        StatefulWidget::render(&widget, inner, frame, &mut state);
    }

    fn render_scrollbar_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Scrollbar (V+H)")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        // Vertical scrollbar on right edge
        if inner.width > 2 && inner.height > 0 {
            let v_area = Rect {
                x: inner.x + inner.width - 1,
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            let mut v_state = ScrollbarState::new(100, 33, inner.height as usize);
            let v_sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::new().fg(theme::accent::PRIMARY));
            StatefulWidget::render(&v_sb, v_area, frame, &mut v_state);

            // Horizontal scrollbar on bottom
            if inner.height >= 2 {
                let h_area = Rect {
                    x: inner.x,
                    y: inner.y + inner.height - 1,
                    width: inner.width.saturating_sub(1),
                    height: 1,
                };
                let mut h_state =
                    ScrollbarState::new(200, 75, inner.width.saturating_sub(1) as usize);
                let h_sb = Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
                    .thumb_style(Style::new().fg(theme::accent::SECONDARY));
                StatefulWidget::render(&h_sb, h_area, frame, &mut h_state);
            }
        }
    }

    fn render_paginator_modes(&self, frame: &mut Frame, area: Rect) {
        let cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(33.3),
                Constraint::Percentage(33.3),
                Constraint::Percentage(33.4),
            ])
            .split(area);

        // Page mode
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Paginator: Page")
            .style(theme::content_border());
        let inner = block.inner(cols[0]);
        block.render(cols[0], frame);
        Paginator::with_pages(2, 5)
            .mode(PaginatorMode::Page)
            .style(Style::new().fg(theme::accent::PRIMARY))
            .render(inner, frame);

        // Compact mode
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Paginator: Compact")
            .style(theme::content_border());
        let inner = block.inner(cols[1]);
        block.render(cols[1], frame);
        Paginator::with_pages(3, 5)
            .mode(PaginatorMode::Compact)
            .style(Style::new().fg(theme::accent::SECONDARY))
            .render(inner, frame);

        // Dots mode
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Paginator: Dots")
            .style(theme::content_border());
        let inner = block.inner(cols[2]);
        block.render(cols[2], frame);
        Paginator::with_pages(4, 5)
            .mode(PaginatorMode::Dots)
            .style(Style::new().fg(theme::accent::SUCCESS))
            .render(inner, frame);
    }

    // -----------------------------------------------------------------------
    // Section H: Advanced Widgets
    // -----------------------------------------------------------------------
    fn render_advanced_widgets(&self, frame: &mut Frame, area: Rect) {
        let rows = Flex::vertical()
            .constraints([
                Constraint::Percentage(38.0),
                Constraint::Percentage(34.0),
                Constraint::Percentage(28.0),
            ])
            .split(area);

        let top_cols = Flex::horizontal()
            .constraints([Constraint::Percentage(52.0), Constraint::Percentage(48.0)])
            .split(rows[0]);
        self.render_command_palette_demo(frame, top_cols[0]);
        self.render_file_picker_demo(frame, top_cols[1]);

        let mid_cols = Flex::horizontal()
            .constraints([Constraint::Percentage(58.0), Constraint::Percentage(42.0)])
            .split(rows[1]);
        self.render_log_viewer_demo(frame, mid_cols[0]);
        let mid_right_rows = Flex::vertical()
            .constraints([Constraint::Percentage(55.0), Constraint::Percentage(45.0)])
            .split(mid_cols[1]);
        self.render_virtualized_demo(frame, mid_right_rows[0]);
        self.render_history_panel_demo(frame, mid_right_rows[1]);

        let bottom_cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(34.0),
                Constraint::Percentage(33.0),
                Constraint::Percentage(33.0),
            ])
            .split(rows[2]);
        self.render_modal_demo(frame, bottom_cols[0]);
        self.render_notification_stack_demo(frame, bottom_cols[1]);
        self.render_validation_timer_demo(frame, bottom_cols[2]);
    }

    fn render_command_palette_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Command Palette")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut palette = CommandPalette::new().with_max_visible(6);
        palette.register_action(
            ActionItem::new("cmd:open", "Open File")
                .with_description("Open a file from disk")
                .with_tags(&["file", "open"])
                .with_category("File"),
        );
        palette.register_action(
            ActionItem::new("cmd:save", "Save File")
                .with_description("Save current buffer")
                .with_tags(&["file", "save"])
                .with_category("File"),
        );
        palette.register_action(
            ActionItem::new("cmd:theme", "Cycle Theme")
                .with_description("Switch color theme")
                .with_tags(&["theme", "colors"])
                .with_category("View"),
        );
        palette.register_action(
            ActionItem::new("cmd:perf", "Toggle Performance HUD")
                .with_description("Show performance overlay")
                .with_tags(&["hud", "perf"])
                .with_category("View"),
        );
        palette.register_action(
            ActionItem::new("cmd:quit", "Quit")
                .with_description("Exit the application")
                .with_tags(&["exit"])
                .with_category("App"),
        );
        palette.open();
        palette.render(inner, frame);
    }

    fn render_file_picker_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("File Picker")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let picker = FilePicker::new()
            .dir_style(Style::new().fg(theme::accent::PRIMARY))
            .file_style(Style::new().fg(theme::fg::PRIMARY))
            .cursor_style(Style::new().bg(theme::alpha::HIGHLIGHT))
            .header_style(Style::new().fg(theme::fg::MUTED));

        let mut guard = self.file_picker_state.borrow_mut();
        if let Some(state) = guard.as_mut() {
            state.entries.truncate(8);
            StatefulWidget::render(&picker, inner, frame, state);
        } else {
            Paragraph::new("File picker unavailable")
                .style(theme::muted())
                .render(inner, frame);
        }
    }

    fn render_log_viewer_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Log Viewer")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let viewer = self.log_viewer.borrow();
        let mut state = self.log_viewer_state.borrow_mut();
        StatefulWidget::render(&*viewer, inner, frame, &mut *state);
    }

    fn render_virtualized_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Virtualized List")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);
        self.layout_virtualized.set(inner);

        if inner.is_empty() {
            return;
        }

        let list = VirtualizedList::new(&self.virtualized_items)
            .fixed_height(1)
            .hit_id(VIRTUALIZED_SCROLLBAR_HIT_ID)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .highlight_style(
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(theme::alpha::HIGHLIGHT)
                    .attrs(StyleFlags::BOLD),
            );
        let mut state = self.virtualized_state.borrow_mut();
        StatefulWidget::render(&list, inner, frame, &mut *state);
    }

    fn render_history_panel_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("History Panel")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let panel = HistoryPanel::new()
            .with_title("History")
            .with_mode(HistoryPanelMode::Compact)
            .with_undo_items(&[
                "Insert heading",
                "Toggle bold",
                "Paste snippet",
                "Apply theme",
            ])
            .with_redo_items(&["Restore section", "Undo delete"]);
        panel.render(inner, frame);
    }

    fn render_modal_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Modal Dialog")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let dialog = Dialog::confirm("Delete file?", "This action cannot be undone.");
        let mut state = DialogState::new();
        StatefulWidget::render(&dialog, inner, frame, &mut state);
    }

    fn render_notification_stack_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Notifications")
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut queue = NotificationQueue::new(
            QueueConfig::new()
                .max_visible(2)
                .max_queued(4)
                .position(ToastPosition::TopLeft),
        );
        queue.push(
            Toast::new("Build succeeded")
                .icon(ToastIcon::Success)
                .style_variant(ToastStyle::Success)
                .persistent(),
            NotificationPriority::Normal,
        );
        queue.push(
            Toast::new("New update available")
                .icon(ToastIcon::Info)
                .style_variant(ToastStyle::Info)
                .persistent(),
            NotificationPriority::Low,
        );

        let stack = NotificationStack::new(&queue).margin(0);
        stack.render(inner, frame);
    }

    fn render_validation_timer_demo(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Validation + Timer")
            .style(theme::content_border());
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

        let error = ValidationErrorDisplay::new("Invalid email address")
            .with_icon("!")
            .with_style(Style::new().fg(theme::accent::ERROR));
        let mut error_state = ValidationErrorState::default();
        error_state.set_visible(true);
        StatefulWidget::render(&error, rows[0], frame, &mut error_state);

        if rows.len() > 1 {
            let mut timer_state = TimerState::new(Duration::from_secs(90));
            timer_state.start();
            timer_state.tick(Duration::from_secs(31));
            let timer = Timer::new().format(TimerFormat::Digital).label("ETA ");
            StatefulWidget::render(&timer, rows[1], frame, &mut timer_state);
        }

        if rows.len() > 2 {
            let help_cols = Flex::horizontal()
                .constraints([Constraint::Fixed(3), Constraint::Min(1)])
                .split(rows[2]);
            Emoji::new("🧭")
                .with_fallback("[?]")
                .with_style(Style::new().fg(theme::accent::PRIMARY))
                .render(help_cols[0], frame);
            let help = HelpWidget::new()
                .with_mode(HelpMode::Short)
                .entry("j/k", "Navigate")
                .entry("enter", "Select")
                .entry("?", "Help");
            Widget::render(&help, help_cols[1], frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::CellContent;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn gallery_initial_state() {
        let gallery = WidgetGallery::new();
        assert_eq!(gallery.current_section, 0);
        assert_eq!(gallery.tick_count, 0);
    }

    #[test]
    fn gallery_renders_all_sections() {
        let mut gallery = WidgetGallery::new();
        let mut pool = GraphemePool::new();

        for section in 0..SECTION_COUNT {
            gallery.current_section = section;
            let mut frame = Frame::new(120, 40, &mut pool);
            gallery.view(&mut frame, Rect::new(0, 0, 120, 40));

            let mut has_content = false;
            for y in 0..40 {
                for x in 0..120 {
                    if let Some(cell) = frame.buffer.get(x, y)
                        && cell.content != CellContent::EMPTY
                    {
                        has_content = true;
                        break;
                    }
                }
                if has_content {
                    break;
                }
            }

            assert!(has_content, "section {section} rendered empty frame");
        }
    }

    #[test]
    fn gallery_handles_small_terminals() {
        let gallery = WidgetGallery::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);
        gallery.view(&mut frame, Rect::new(0, 0, 20, 5));
    }

    #[test]
    fn gallery_section_navigation() {
        let mut gallery = WidgetGallery::new();
        assert_eq!(gallery.current_section, 0);

        // Navigate forward with j
        let ev = Event::Key(KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        gallery.update(&ev);
        assert_eq!(gallery.current_section, 1);

        // Navigate forward again
        gallery.update(&ev);
        assert_eq!(gallery.current_section, 2);

        // Navigate backward with k
        let ev_back = Event::Key(KeyEvent {
            code: KeyCode::Char('k'),
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        gallery.update(&ev_back);
        assert_eq!(gallery.current_section, 1);
    }

    #[test]
    fn gallery_section_navigation_arrows() {
        let mut gallery = WidgetGallery::new();
        assert_eq!(gallery.current_section, 0);

        let ev_down = Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        gallery.update(&ev_down);
        assert_eq!(gallery.current_section, 1);

        let ev_up = Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        gallery.update(&ev_up);
        assert_eq!(gallery.current_section, 0);
    }

    #[test]
    fn gallery_section_wrap_around() {
        let mut gallery = WidgetGallery::new();
        gallery.current_section = 0;

        // Navigate backward from first section wraps to last section.
        let ev_back = Event::Key(KeyEvent {
            code: KeyCode::Char('k'),
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        gallery.update(&ev_back);
        assert_eq!(gallery.current_section, SECTION_COUNT - 1);

        // Navigate forward from last wraps to 0
        let ev_fwd = Event::Key(KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        });
        gallery.update(&ev_fwd);
        assert_eq!(gallery.current_section, 0);
    }

    #[test]
    fn gallery_all_borders() {
        // Verify all 6 border types are distinct
        let types = [
            BorderType::Ascii,
            BorderType::Square,
            BorderType::Rounded,
            BorderType::Double,
            BorderType::Heavy,
        ];
        // All should be distinct enum variants
        for i in 0..types.len() {
            for j in (i + 1)..types.len() {
                assert_ne!(types[i], types[j]);
            }
        }
    }

    #[test]
    fn gallery_all_styles() {
        // Verify style flag combos produce distinct styles
        let flags = [
            StyleFlags::BOLD,
            StyleFlags::DIM,
            StyleFlags::ITALIC,
            StyleFlags::UNDERLINE,
            StyleFlags::DOUBLE_UNDERLINE,
            StyleFlags::CURLY_UNDERLINE,
            StyleFlags::BLINK,
            StyleFlags::REVERSE,
            StyleFlags::HIDDEN,
            StyleFlags::STRIKETHROUGH,
        ];
        // All single flags should be distinct
        for i in 0..flags.len() {
            for j in (i + 1)..flags.len() {
                assert_ne!(flags[i], flags[j]);
            }
        }
    }

    #[test]
    fn gallery_tick_updates_spinner() {
        let mut gallery = WidgetGallery::new();
        assert_eq!(gallery.spinner_state.current_frame, 0);
        gallery.tick(1);
        assert_eq!(gallery.spinner_state.current_frame, 1);
        gallery.tick(2);
        assert_eq!(gallery.spinner_state.current_frame, 2);
    }

    #[test]
    fn gallery_virtualized_section_wires_mouse_scrollbar_input() {
        let mut gallery = WidgetGallery::new();
        gallery.current_section = 7;
        gallery.layout_virtualized.set(Rect::new(0, 0, 10, 3));

        let down_track = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            9,
            2,
        ));
        gallery.update(&down_track);

        assert_eq!(gallery.virtualized_state.borrow().scroll_offset, 3);
    }
}
