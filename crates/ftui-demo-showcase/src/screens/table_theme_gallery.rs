#![forbid(unsafe_code)]

//! Table theme gallery screen — preview TableTheme presets side-by-side.

use std::cell::{Cell, RefCell};
use std::fs::OpenOptions;
use std::io::Write;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
use ftui_layout::{Constraint, Flex, LayoutSizeHint};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{ColorProfile, Style, TablePresetId, TableTheme};
use ftui_text::{Line, Span, Text, WrapMode};
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table, TableState};
use ftui_widgets::{StatefulWidget, Widget};
use serde_json::json;

use super::{HelpEntry, Screen};
use crate::{determinism, theme};

const PREVIEW_PHASE: f32 = 0.35;
const MIN_CARD_HEIGHT: u16 = 6;
const CARD_DESC_LINES: u16 = 2;
const PREVIEW_PANEL_HEIGHT: u16 = 7;
const LEGEND_HEIGHT: u16 = 3;
const HIGHLIGHT_ROW_INDEX: usize = 1;
const DEFAULT_EXPORT_PATH: &str = "table_theme_preset.json";
const EXPORT_PATH_ENV: &str = "FTUI_TABLE_THEME_EXPORT_PATH";
const IMPORT_PATH_ENV: &str = "FTUI_TABLE_THEME_IMPORT_PATH";
const CLIPBOARD_ENV: &str = "FTUI_TABLE_THEME_CLIPBOARD";

#[derive(Debug, Clone, Copy)]
enum PresetKind {
    Preset(TablePresetId),
    TerminalClassicAnsi,
    TerminalClassicAuto,
}

#[derive(Debug, Clone, Copy)]
struct PresetSpec {
    name: &'static str,
    desc: &'static str,
    kind: PresetKind,
}

impl PresetSpec {
    fn theme(self) -> TableTheme {
        match self.kind {
            PresetKind::Preset(id) => TableTheme::preset(id),
            PresetKind::TerminalClassicAnsi => {
                TableTheme::terminal_classic_for(ColorProfile::Ansi16)
            }
            PresetKind::TerminalClassicAuto => TableTheme::terminal_classic(),
        }
    }

    fn log_id(self) -> &'static str {
        match self.kind {
            PresetKind::Preset(TablePresetId::Aurora) => "aurora",
            PresetKind::Preset(TablePresetId::Graphite) => "graphite",
            PresetKind::Preset(TablePresetId::Neon) => "neon",
            PresetKind::Preset(TablePresetId::Slate) => "slate",
            PresetKind::Preset(TablePresetId::Solar) => "solar",
            PresetKind::Preset(TablePresetId::Orchard) => "orchard",
            PresetKind::Preset(TablePresetId::Paper) => "paper",
            PresetKind::Preset(TablePresetId::Midnight) => "midnight",
            PresetKind::Preset(TablePresetId::TerminalClassic) => "terminal_classic",
            PresetKind::TerminalClassicAnsi => "terminal_classic_ansi16",
            PresetKind::TerminalClassicAuto => "terminal_classic_auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewMode {
    Widget,
    Markdown,
}

impl PreviewMode {
    fn toggle(self) -> Self {
        match self {
            Self::Widget => Self::Markdown,
            Self::Markdown => Self::Widget,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Widget => "Widget",
            Self::Markdown => "Markdown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZebraStrength {
    Off,
    Subtle,
    Strong,
}

impl ZebraStrength {
    fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Subtle,
            Self::Subtle => Self::Strong,
            Self::Strong => Self::Off,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Subtle => "Subtle",
            Self::Strong => "Strong",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorderStyle {
    Preset,
    Subtle,
    High,
}

impl BorderStyle {
    fn cycle(self) -> Self {
        match self {
            Self::Preset => Self::Subtle,
            Self::Subtle => Self::High,
            Self::High => Self::Preset,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Preset => "Preset",
            Self::Subtle => "Subtle",
            Self::High => "High",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogState {
    size: (u16, u16),
    selected: usize,
    preview: PreviewMode,
    header_emphasis: bool,
    zebra: ZebraStrength,
    border: BorderStyle,
    highlight_row: bool,
}

const PRESETS: &[PresetSpec] = &[
    PresetSpec {
        name: "Aurora",
        desc: "cool glow",
        kind: PresetKind::Preset(TablePresetId::Aurora),
    },
    PresetSpec {
        name: "Graphite",
        desc: "neutral + dense",
        kind: PresetKind::Preset(TablePresetId::Graphite),
    },
    PresetSpec {
        name: "Neon",
        desc: "high energy",
        kind: PresetKind::Preset(TablePresetId::Neon),
    },
    PresetSpec {
        name: "Slate",
        desc: "balanced",
        kind: PresetKind::Preset(TablePresetId::Slate),
    },
    PresetSpec {
        name: "Solar",
        desc: "warm contrast",
        kind: PresetKind::Preset(TablePresetId::Solar),
    },
    PresetSpec {
        name: "Orchard",
        desc: "earthy",
        kind: PresetKind::Preset(TablePresetId::Orchard),
    },
    PresetSpec {
        name: "Paper",
        desc: "light ink",
        kind: PresetKind::Preset(TablePresetId::Paper),
    },
    PresetSpec {
        name: "Midnight",
        desc: "dark focus",
        kind: PresetKind::Preset(TablePresetId::Midnight),
    },
    PresetSpec {
        name: "Terminal Classic",
        desc: "ANSI-16 baseline",
        kind: PresetKind::TerminalClassicAnsi,
    },
    PresetSpec {
        name: "Terminal Classic+",
        desc: "modern terminals",
        kind: PresetKind::TerminalClassicAuto,
    },
];

pub struct TableThemeGallery {
    selected: usize,
    grid_columns: Cell<usize>,
    card_layout: RefCell<Vec<Rect>>,
    log_path: Option<String>,
    run_id: Option<String>,
    last_logged_state: Cell<Option<LogState>>,
    preview_mode: PreviewMode,
    header_emphasis: bool,
    zebra_strength: ZebraStrength,
    border_style: BorderStyle,
    highlight_row: bool,
}

impl Default for TableThemeGallery {
    fn default() -> Self {
        Self::new()
    }
}

impl TableThemeGallery {
    pub fn new() -> Self {
        Self::with_log_path(Self::resolve_log_path())
    }

    pub fn with_log_path(log_path: Option<String>) -> Self {
        Self {
            selected: 0,
            grid_columns: Cell::new(1),
            card_layout: RefCell::new(Vec::new()),
            log_path,
            run_id: determinism::demo_run_id(),
            last_logged_state: Cell::new(None),
            preview_mode: PreviewMode::Markdown,
            header_emphasis: false,
            zebra_strength: ZebraStrength::Subtle,
            border_style: BorderStyle::Preset,
            highlight_row: false,
        }
    }

    fn resolve_log_path() -> Option<String> {
        std::env::var("FTUI_TABLE_THEME_REPORT_PATH")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("FTUI_TABLE_THEME_GALLERY_REPORT")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
    }

    fn table_rows() -> Vec<Row> {
        [
            Row::new(["Latency", "16.2ms", "v"]),
            Row::new(["Errors", "0.3%", "v"]),
            Row::new(["Throughput", "9.8k/s", "^"]),
            Row::new(["Cache Hit", "94%", "^"]),
        ]
        .into_iter()
        .collect()
    }

    fn table_header() -> Row {
        Row::new(["Metric", "Value", "Trend"]).style(Style::new().bold())
    }

    fn markdown_table_text() -> &'static str {
        "| Metric | Value | Trend |\n| --- | --- | --- |\n| Latency | 16.2ms | v |\n| Errors | 0.3% | v |\n| Throughput | 9.8k/s | ^ |\n| Cache Hit | 94% | ^ |\n"
    }

    fn compute_grid(area: Rect, count: usize) -> (usize, usize) {
        if area.is_empty() || count == 0 {
            return (1, 1);
        }
        let mut cols = if area.width >= 120 {
            4
        } else if area.width >= 90 {
            3
        } else {
            2
        };
        cols = cols.min(count.max(1));
        let mut rows = count.div_ceil(cols);
        while rows as u16 * MIN_CARD_HEIGHT > area.height && cols > 1 {
            cols -= 1;
            rows = count.div_ceil(cols);
        }
        (cols.max(1), rows.max(1))
    }

    fn move_selection(&mut self, delta_row: i32, delta_col: i32) {
        let count = PRESETS.len();
        if count == 0 {
            return;
        }
        let cols = self.grid_columns.get().max(1);
        let rows = count.div_ceil(cols);
        let row = (self.selected / cols) as i32;
        let col = (self.selected % cols) as i32;
        let next_row = (row + delta_row).clamp(0, rows.saturating_sub(1) as i32);
        let next_col = (col + delta_col).clamp(0, cols.saturating_sub(1) as i32);
        let mut idx = (next_row as usize) * cols + (next_col as usize);
        if idx >= count {
            idx = count.saturating_sub(1);
        }
        self.selected = idx;
    }

    fn apply_overrides(&self, mut theme: TableTheme) -> TableTheme {
        if self.header_emphasis {
            theme.header = theme.header.bold().underline();
        }

        theme.row_alt = match self.zebra_strength {
            ZebraStrength::Off => theme.row,
            ZebraStrength::Subtle => theme.row_alt,
            ZebraStrength::Strong => Style::new()
                .bg(theme::alpha::HIGHLIGHT)
                .merge(&theme.row_alt),
        };

        match self.border_style {
            BorderStyle::Preset => {}
            BorderStyle::Subtle => {
                let border = Style::new().fg(theme::fg::MUTED).dim();
                theme.border = border.merge(&theme.border);
                theme.divider = border.merge(&theme.divider);
            }
            BorderStyle::High => {
                let border = Style::new().fg(theme::accent::PRIMARY).bold();
                theme.border = border.merge(&theme.border);
                theme.divider = border.merge(&theme.divider);
            }
        }

        theme
    }

    fn preview_controls_text(&self) -> Text {
        let header = if self.header_emphasis { "On" } else { "Off" };
        let highlight = if self.highlight_row { "On" } else { "Off" };
        let line_one = Line::from(format!(
            "View(V): {} | Header(H): {} | Zebra(Z): {}",
            self.preview_mode.label(),
            header,
            self.zebra_strength.label(),
        ));
        let line_two = Line::from(format!(
            "Border(B): {} | Highlight(L): {} | Arrows/Tab: Select | R: Reset",
            self.border_style.label(),
            highlight,
        ));
        Text::from_lines([line_one, line_two])
    }

    fn preview_theme(&self, preset: PresetSpec) -> TableTheme {
        self.apply_overrides(preset.theme())
    }

    fn render_preset_card(
        &self,
        frame: &mut Frame,
        area: Rect,
        preset: PresetSpec,
        selected: bool,
    ) {
        if area.is_empty() {
            return;
        }

        let border_style = theme::panel_border_style(selected, theme::screen_accent::DATA_VIZ);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(preset.name)
            .title_alignment(Alignment::Center)
            .style(border_style);

        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(CARD_DESC_LINES), Constraint::Min(1)])
            .split(inner);

        let desc = Line::from_spans([Span::styled(
            preset.desc,
            Style::new().fg(theme::fg::SECONDARY),
        )]);
        Paragraph::new(Text::from_lines([desc]))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(rows[0], frame);

        let widths = [
            Constraint::Percentage(50.0),
            Constraint::Percentage(30.0),
            Constraint::Percentage(20.0),
        ];
        let table = Table::new(Self::table_rows(), widths)
            .header(Self::table_header())
            .theme(self.preview_theme(preset))
            .theme_phase(PREVIEW_PHASE)
            .column_spacing(1);
        let mut state = TableState::default();
        if self.highlight_row {
            state.selected = Some(HIGHLIGHT_ROW_INDEX);
        }
        StatefulWidget::render(&table, rows[1], frame, &mut state);
    }

    fn render_markdown_preview(&self, frame: &mut Frame, area: Rect, theme: TableTheme) {
        if area.is_empty() {
            return;
        }
        let md_theme = MarkdownTheme {
            table_theme: theme,
            ..Default::default()
        };
        let rendered = MarkdownRenderer::new(md_theme).render(Self::markdown_table_text());
        Paragraph::new(rendered).render(area, frame);
    }

    fn render_widget_preview(&self, frame: &mut Frame, area: Rect, theme: TableTheme) {
        if area.is_empty() {
            return;
        }
        let widths = [
            Constraint::Percentage(50.0),
            Constraint::Percentage(30.0),
            Constraint::Percentage(20.0),
        ];
        let table = Table::new(Self::table_rows(), widths)
            .header(Self::table_header())
            .theme(theme)
            .theme_phase(PREVIEW_PHASE)
            .column_spacing(1);
        if self.highlight_row {
            let mut state = TableState::default();
            state.selected = Some(HIGHLIGHT_ROW_INDEX);
            StatefulWidget::render(&table, area, frame, &mut state);
        } else {
            Widget::render(&table, area, frame);
        }
    }

    fn render_preview_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let preset = PRESETS.get(self.selected).copied().unwrap_or(PRESETS[0]);
        let title = format!("Preview · {} · {}", preset.name, self.preview_mode.label());
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .title_alignment(Alignment::Center)
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }

        let legend_height = LEGEND_HEIGHT.min(inner.height.saturating_sub(1));
        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(legend_height), Constraint::Min(1)])
            .split(inner);

        if legend_height > 0 {
            Paragraph::new(self.preview_controls_text())
                .style(theme::muted())
                .wrap(WrapMode::Word)
                .render(rows[0], frame);
        }

        let preview_area = rows.get(1).copied().unwrap_or(rows[0]);
        let theme = self.preview_theme(preset);
        match self.preview_mode {
            PreviewMode::Markdown => self.render_markdown_preview(frame, preview_area, theme),
            PreviewMode::Widget => self.render_widget_preview(frame, preview_area, theme),
        }
    }

    fn card_table_area(&self, preset: PresetSpec, area: Rect) -> Rect {
        if area.is_empty() {
            return Rect::new(area.x, area.y, 0, 0);
        }
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(preset.name)
            .title_alignment(Alignment::Center);
        let inner = block.inner(area);
        if inner.is_empty() {
            return Rect::new(inner.x, inner.y, 0, 0);
        }
        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(CARD_DESC_LINES), Constraint::Min(1)])
            .split(inner);
        rows.get(1)
            .copied()
            .unwrap_or(Rect::new(inner.x, inner.y, 0, 0))
    }

    fn column_widths(&self, table_area: Rect) -> Vec<u16> {
        if table_area.is_empty() {
            return Vec::new();
        }
        let widths = [
            Constraint::Percentage(50.0),
            Constraint::Percentage(30.0),
            Constraint::Percentage(20.0),
        ];
        let rects = Flex::horizontal()
            .constraints(widths)
            .gap(1)
            .split_with_measurer(
                Rect::new(table_area.x, table_area.y, table_area.width, 1),
                |_, _| LayoutSizeHint::ZERO,
            );
        rects.iter().map(|rect| rect.width).collect()
    }

    fn log_gallery(&self, area: Rect, layout: &[Rect]) {
        let Some(path) = self.log_path.as_ref() else {
            return;
        };
        let state = LogState {
            size: (area.width, area.height),
            selected: self.selected,
            preview: self.preview_mode,
            header_emphasis: self.header_emphasis,
            zebra: self.zebra_strength,
            border: self.border_style,
            highlight_row: self.highlight_row,
        };
        if self.last_logged_state.get() == Some(state) {
            return;
        }
        self.last_logged_state.set(Some(state));
        let run_id = self.run_id.as_deref().unwrap_or("unknown");
        let selected_preset = PRESETS
            .get(self.selected)
            .copied()
            .map(|preset| preset.log_id())
            .unwrap_or("unknown");
        let selected_preset_name = PRESETS
            .get(self.selected)
            .map(|preset| preset.name)
            .unwrap_or("unknown");

        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };

        for (idx, preset) in PRESETS.iter().copied().enumerate() {
            let card_area = layout.get(idx).copied().unwrap_or(Rect::new(0, 0, 0, 0));
            let table_area = self.card_table_area(preset, card_area);
            let widths = self.column_widths(table_area);
            let diagnostics = self.preview_theme(preset).diagnostics();
            let payload = json!({
                "event": "table_theme_gallery",
                "timestamp": determinism::chrono_like_timestamp(),
                "run_id": run_id,
                "screen_width": area.width,
                "screen_height": area.height,
                "selected_index": state.selected,
                "selected_preset": selected_preset,
                "selected_preset_name": selected_preset_name,
                "preview_mode": state.preview.label(),
                "header_emphasis": state.header_emphasis,
                "zebra_strength": state.zebra.label(),
                "border_style": state.border.label(),
                "highlight_row": state.highlight_row,
                "preset_index": idx,
                "preset_name": preset.name,
                "preset_id": preset.log_id(),
                "theme_preset": diagnostics.preset_id.map(|id| format!("{id:?}")),
                "phase": PREVIEW_PHASE,
                "style_hash": diagnostics.style_hash,
                "effects_hash": diagnostics.effects_hash,
                "column_widths": widths,
            });
            let _ = writeln!(file, "{payload}");
        }
    }
}

impl Screen for TableThemeGallery {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        match event {
            Event::Mouse(mouse) => {
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                    let layout = self.card_layout.borrow();
                    for (idx, rect) in layout.iter().enumerate() {
                        if rect.contains(mouse.x, mouse.y) {
                            self.selected = idx.min(PRESETS.len().saturating_sub(1));
                            break;
                        }
                    }
                }
            }
            Event::Key(KeyEvent {
                code,
                kind: KeyEventKind::Press,
                ..
            }) => match code {
                KeyCode::Left => self.move_selection(0, -1),
                KeyCode::Right => self.move_selection(0, 1),
                KeyCode::Up => self.move_selection(-1, 0),
                KeyCode::Down => self.move_selection(1, 0),
                KeyCode::Tab => {
                    self.selected = (self.selected + 1) % PRESETS.len().max(1);
                }
                KeyCode::Home => self.selected = 0,
                KeyCode::End => self.selected = PRESETS.len().saturating_sub(1),
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    self.preview_mode = self.preview_mode.toggle();
                }
                KeyCode::Char('m') | KeyCode::Char('M') => {
                    self.preview_mode = PreviewMode::Markdown;
                }
                KeyCode::Char('w') | KeyCode::Char('W') => {
                    self.preview_mode = PreviewMode::Widget;
                }
                KeyCode::Char('h') | KeyCode::Char('H') => {
                    self.header_emphasis = !self.header_emphasis;
                }
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    self.zebra_strength = self.zebra_strength.cycle();
                }
                KeyCode::Char('b') | KeyCode::Char('B') => {
                    self.border_style = self.border_style.cycle();
                }
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    self.highlight_row = !self.highlight_row;
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.preview_mode = PreviewMode::Markdown;
                    self.header_emphasis = false;
                    self.zebra_strength = ZebraStrength::Subtle;
                    self.border_style = BorderStyle::Preset;
                    self.highlight_row = false;
                }
                _ => {}
            },
            _ => {}
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let outer = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Table Theme Gallery")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::DATA_VIZ));

        let inner = outer.inner(area);
        outer.render(area, frame);
        if inner.is_empty() {
            return;
        }

        let preview_height = PREVIEW_PANEL_HEIGHT.min(inner.height.saturating_sub(4));
        let rows = Flex::vertical()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Min(6), Constraint::Fixed(preview_height)])
            .split(inner);

        let grid_area = rows[0];
        let preview_area = rows[1];
        let (cols, rows_count) = Self::compute_grid(grid_area, PRESETS.len());
        self.grid_columns.set(cols);

        let row_constraints = vec![Constraint::Ratio(1, rows_count as u32); rows_count];
        let col_constraints = vec![Constraint::Ratio(1, cols as u32); cols];

        let grid_rows = Flex::vertical()
            .gap(theme::spacing::XS)
            .constraints(row_constraints)
            .split(grid_area);

        let mut layout = Vec::with_capacity(PRESETS.len());
        let mut preset_idx = 0usize;
        for row_area in grid_rows {
            let grid_cols = Flex::horizontal()
                .gap(theme::spacing::XS)
                .constraints(col_constraints.clone())
                .split(row_area);
            for col_area in grid_cols {
                if preset_idx >= PRESETS.len() {
                    break;
                }
                layout.push(col_area);
                self.render_preset_card(
                    frame,
                    col_area,
                    PRESETS[preset_idx],
                    preset_idx == self.selected,
                );
                preset_idx += 1;
            }
        }
        self.log_gallery(area, &layout);
        self.card_layout.replace(layout);

        self.render_preview_panel(frame, preview_area);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Arrows",
                action: "Move selection",
            },
            HelpEntry {
                key: "Tab",
                action: "Next preset",
            },
            HelpEntry {
                key: "Home/End",
                action: "First/last preset",
            },
            HelpEntry {
                key: "V",
                action: "Toggle preview mode",
            },
            HelpEntry {
                key: "H",
                action: "Header emphasis",
            },
            HelpEntry {
                key: "Z",
                action: "Cycle zebra strength",
            },
            HelpEntry {
                key: "B",
                action: "Cycle border style",
            },
            HelpEntry {
                key: "L",
                action: "Toggle highlight row",
            },
            HelpEntry {
                key: "R",
                action: "Reset overrides",
            },
            HelpEntry {
                key: "Mouse",
                action: "Pick preset card",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Table Theme Gallery"
    }

    fn tab_label(&self) -> &'static str {
        "Tables"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    fn key_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Default::default(),
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn gallery_defaults() {
        let gallery = TableThemeGallery::with_log_path(None);
        assert_eq!(gallery.selected, 0);
        assert_eq!(gallery.preview_mode, PreviewMode::Markdown);
        assert!(!gallery.header_emphasis);
        assert_eq!(gallery.zebra_strength, ZebraStrength::Subtle);
        assert_eq!(gallery.border_style, BorderStyle::Preset);
        assert!(!gallery.highlight_row);
    }

    #[test]
    fn gallery_toggles_and_cycles() {
        let mut gallery = TableThemeGallery::with_log_path(None);

        gallery.update(&key_press(KeyCode::Char('v')));
        assert_eq!(gallery.preview_mode, PreviewMode::Widget);
        gallery.update(&key_press(KeyCode::Char('m')));
        assert_eq!(gallery.preview_mode, PreviewMode::Markdown);
        gallery.update(&key_press(KeyCode::Char('w')));
        assert_eq!(gallery.preview_mode, PreviewMode::Widget);

        gallery.update(&key_press(KeyCode::Char('h')));
        assert!(gallery.header_emphasis);

        gallery.update(&key_press(KeyCode::Char('z')));
        assert_eq!(gallery.zebra_strength, ZebraStrength::Strong);
        gallery.update(&key_press(KeyCode::Char('z')));
        assert_eq!(gallery.zebra_strength, ZebraStrength::Off);
        gallery.update(&key_press(KeyCode::Char('z')));
        assert_eq!(gallery.zebra_strength, ZebraStrength::Subtle);

        gallery.update(&key_press(KeyCode::Char('b')));
        assert_eq!(gallery.border_style, BorderStyle::Subtle);
        gallery.update(&key_press(KeyCode::Char('b')));
        assert_eq!(gallery.border_style, BorderStyle::High);
        gallery.update(&key_press(KeyCode::Char('b')));
        assert_eq!(gallery.border_style, BorderStyle::Preset);

        gallery.update(&key_press(KeyCode::Char('l')));
        assert!(gallery.highlight_row);
    }

    #[test]
    fn gallery_selection_navigation_and_wrap() {
        let mut gallery = TableThemeGallery::with_log_path(None);
        gallery.grid_columns.set(3);
        gallery.selected = 0;

        gallery.update(&key_press(KeyCode::Right));
        assert_eq!(gallery.selected, 1);
        gallery.update(&key_press(KeyCode::Down));
        assert_eq!(gallery.selected, 4);
        gallery.update(&key_press(KeyCode::Left));
        assert_eq!(gallery.selected, 3);
        gallery.update(&key_press(KeyCode::Up));
        assert_eq!(gallery.selected, 0);

        gallery.selected = PRESETS.len().saturating_sub(1);
        gallery.update(&key_press(KeyCode::Tab));
        assert_eq!(gallery.selected, 0);

        gallery.selected = 3;
        gallery.update(&key_press(KeyCode::Home));
        assert_eq!(gallery.selected, 0);
        gallery.update(&key_press(KeyCode::End));
        assert_eq!(gallery.selected, PRESETS.len().saturating_sub(1));
    }

    #[test]
    fn gallery_reset_restores_defaults_without_touching_selection() {
        let mut gallery = TableThemeGallery::with_log_path(None);
        gallery.selected = 2;
        gallery.preview_mode = PreviewMode::Widget;
        gallery.header_emphasis = true;
        gallery.zebra_strength = ZebraStrength::Strong;
        gallery.border_style = BorderStyle::High;
        gallery.highlight_row = true;

        gallery.update(&key_press(KeyCode::Char('r')));

        assert_eq!(gallery.selected, 2);
        assert_eq!(gallery.preview_mode, PreviewMode::Markdown);
        assert!(!gallery.header_emphasis);
        assert_eq!(gallery.zebra_strength, ZebraStrength::Subtle);
        assert_eq!(gallery.border_style, BorderStyle::Preset);
        assert!(!gallery.highlight_row);
    }

    #[test]
    fn gallery_view_preserves_state_across_renders() {
        let mut gallery = TableThemeGallery::with_log_path(None);
        gallery.selected = 4;
        gallery.preview_mode = PreviewMode::Widget;
        gallery.header_emphasis = true;
        gallery.zebra_strength = ZebraStrength::Strong;
        gallery.border_style = BorderStyle::High;
        gallery.highlight_row = true;

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        gallery.view(&mut frame, Rect::new(0, 0, 120, 40));

        assert_eq!(gallery.selected, 4);
        assert_eq!(gallery.preview_mode, PreviewMode::Widget);
        assert!(gallery.header_emphasis);
        assert_eq!(gallery.zebra_strength, ZebraStrength::Strong);
        assert_eq!(gallery.border_style, BorderStyle::High);
        assert!(gallery.highlight_row);
    }
}
