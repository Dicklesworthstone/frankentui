#![forbid(unsafe_code)]

//! Widget Builder Sandbox screen.
//!
//! Lets users assemble a small UI from a handful of widgets, toggle props,
//! and swap deterministic presets. Intended as an interactive playground
//! that still keeps rendering deterministic and explainable.

use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::Write;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::WrapMode;
use ftui_widgets::Badge;
use ftui_widgets::StatefulWidget;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::ProgressBar;
use ftui_widgets::sparkline::Sparkline;
use serde::{Deserialize, Serialize};

use super::{HelpEntry, Screen};
use crate::theme;

const ACCENTS: [theme::ColorToken; 6] = [
    theme::accent::ACCENT_1,
    theme::accent::ACCENT_2,
    theme::accent::ACCENT_3,
    theme::accent::ACCENT_4,
    theme::accent::ACCENT_5,
    theme::accent::ACCENT_6,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WidgetKind {
    Paragraph,
    List,
    Progress,
    Sparkline,
    Badge,
}

impl WidgetKind {
    fn label(self) -> &'static str {
        match self {
            Self::Paragraph => "Paragraph",
            Self::List => "List",
            Self::Progress => "Progress",
            Self::Sparkline => "Sparkline",
            Self::Badge => "Badge",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::List => "list",
            Self::Progress => "progress",
            Self::Sparkline => "sparkline",
            Self::Badge => "badge",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "paragraph" => Some(Self::Paragraph),
            "list" => Some(Self::List),
            "progress" => Some(Self::Progress),
            "sparkline" => Some(Self::Sparkline),
            "badge" => Some(Self::Badge),
            _ => None,
        }
    }

    fn tag(self) -> u8 {
        match self {
            Self::Paragraph => 1,
            Self::List => 2,
            Self::Progress => 3,
            Self::Sparkline => 4,
            Self::Badge => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WidgetConfig {
    kind: WidgetKind,
    title: String,
    enabled: bool,
    bordered: bool,
    show_title: bool,
    accent_idx: usize,
    value: u16,
}

impl WidgetConfig {
    fn new(kind: WidgetKind, title: impl Into<String>, value: u16) -> Self {
        Self {
            kind,
            title: title.into(),
            enabled: true,
            bordered: true,
            show_title: true,
            accent_idx: 0,
            value,
        }
    }

    fn accent(&self) -> theme::ColorToken {
        ACCENTS[self.accent_idx % ACCENTS.len()]
    }
}

#[derive(Debug, Clone)]
struct Preset {
    name: String,
    widgets: Vec<WidgetConfig>,
    read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WidgetSnapshot {
    kind: String,
    title: String,
    enabled: bool,
    bordered: bool,
    show_title: bool,
    accent_idx: usize,
    value: u16,
}

impl WidgetSnapshot {
    fn from_config(config: &WidgetConfig) -> Self {
        Self {
            kind: config.kind.id().to_string(),
            title: config.title.clone(),
            enabled: config.enabled,
            bordered: config.bordered,
            show_title: config.show_title,
            accent_idx: config.accent_idx,
            value: config.value,
        }
    }

    fn into_config(self) -> Option<WidgetConfig> {
        let kind = WidgetKind::from_id(&self.kind)?;
        Some(WidgetConfig {
            kind,
            title: self.title,
            enabled: self.enabled,
            bordered: self.bordered,
            show_title: self.show_title,
            accent_idx: self.accent_idx,
            value: self.value,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PresetSnapshot {
    name: String,
    read_only: bool,
    widgets: Vec<WidgetSnapshot>,
}

impl PresetSnapshot {
    fn from_preset(preset: &Preset) -> Self {
        Self {
            name: preset.name.clone(),
            read_only: preset.read_only,
            widgets: preset
                .widgets
                .iter()
                .map(WidgetSnapshot::from_config)
                .collect(),
        }
    }

    fn from_current(name: String, widgets: &[WidgetConfig], read_only: bool) -> Self {
        Self {
            name,
            read_only,
            widgets: widgets.iter().map(WidgetSnapshot::from_config).collect(),
        }
    }

    fn into_preset(self) -> Option<Preset> {
        let mut widgets = Vec::with_capacity(self.widgets.len());
        for widget in self.widgets {
            widgets.push(widget.into_config()?);
        }
        Some(Preset {
            name: self.name,
            widgets,
            read_only: self.read_only,
        })
    }
}

#[derive(Debug, Serialize)]
struct WidgetBuilderExport {
    event: String,
    run_id: String,
    preset_id: String,
    preset_name: String,
    preset_index: usize,
    widget_count: usize,
    props_hash: u64,
    preset: PresetSnapshot,
    outcome: String,
}

impl Preset {
    fn new(name: impl Into<String>, widgets: Vec<WidgetConfig>) -> Self {
        Self {
            name: name.into(),
            widgets,
            read_only: true,
        }
    }
}

/// Widget Builder Sandbox screen state.
pub struct WidgetBuilder {
    presets: Vec<Preset>,
    active_preset: usize,
    widgets: Vec<WidgetConfig>,
    selected_widget: usize,
    preset_state: RefCell<ListState>,
    widget_state: RefCell<ListState>,
    last_export: Option<String>,
    custom_counter: u32,
}

impl Default for WidgetBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl WidgetBuilder {
    pub fn new() -> Self {
        let presets = vec![
            Preset::new(
                "Starter Kit",
                vec![
                    WidgetConfig::new(WidgetKind::Paragraph, "Intro Paragraph", 42),
                    WidgetConfig::new(WidgetKind::List, "Checklist", 1),
                    WidgetConfig::new(WidgetKind::Progress, "Build Progress", 65),
                    WidgetConfig::new(WidgetKind::Sparkline, "Throughput", 70),
                ],
            ),
            Preset::new(
                "Status Wall",
                vec![
                    WidgetConfig::new(WidgetKind::Badge, "Status Badge", 0),
                    WidgetConfig::new(WidgetKind::Progress, "Upload", 30),
                    WidgetConfig::new(WidgetKind::Sparkline, "Latency", 55),
                    WidgetConfig::new(WidgetKind::Paragraph, "Summary", 20),
                ],
            ),
            Preset::new(
                "Minimal",
                vec![
                    WidgetConfig::new(WidgetKind::Paragraph, "Message", 10),
                    WidgetConfig::new(WidgetKind::Badge, "Priority", 0),
                    WidgetConfig::new(WidgetKind::List, "Steps", 2),
                ],
            ),
        ];

        let widgets = presets
            .first()
            .map(|preset| preset.widgets.clone())
            .unwrap_or_default();

        let mut preset_state = ListState::default();
        preset_state.select(Some(0));

        let mut widget_state = ListState::default();
        widget_state.select(Some(0));

        Self {
            presets,
            active_preset: 0,
            widgets,
            selected_widget: 0,
            preset_state: RefCell::new(preset_state),
            widget_state: RefCell::new(widget_state),
            last_export: None,
            custom_counter: 1,
        }
    }

    fn clamp_selection(&mut self) {
        if self.widgets.is_empty() {
            self.selected_widget = 0;
            self.widget_state.borrow_mut().select(None);
            return;
        }
        if self.selected_widget >= self.widgets.len() {
            self.selected_widget = self.widgets.len() - 1;
        }
        self.widget_state
            .borrow_mut()
            .select(Some(self.selected_widget));
    }

    fn load_preset(&mut self, idx: usize) {
        if let Some(preset) = self.presets.get(idx) {
            self.widgets = preset.widgets.clone();
            self.active_preset = idx;
            self.preset_state.borrow_mut().select(Some(idx));
            self.selected_widget = 0;
            self.clamp_selection();
        }
    }

    fn save_preset(&mut self) {
        let name = format!("Custom {}", self.custom_counter);
        self.custom_counter += 1;
        let mut preset = Preset {
            name,
            widgets: self.widgets.clone(),
            read_only: false,
        };
        // Normalize accents to keep saved presets deterministic.
        for widget in &mut preset.widgets {
            widget.accent_idx %= ACCENTS.len();
        }
        self.presets.push(preset);
        self.load_preset(self.presets.len() - 1);
    }

    fn export_jsonl(&mut self) {
        let path = Self::export_path();
        match self.export_jsonl_to_path(&path) {
            Ok(_) => {
                self.last_export = Some(format!("ok: {}", path));
            }
            Err(err) => {
                self.last_export = Some(format!("error: {}", err));
            }
        }
    }

    fn export_jsonl_to_path(&self, path: &str) -> Result<String, String> {
        let payload = self.build_export_payload()?;
        let line = serde_json::to_string(&payload).map_err(|err| err.to_string())?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| writeln!(file, "{}", line))
            .map_err(|err| err.to_string())?;
        Ok(line)
    }

    fn build_export_payload(&self) -> Result<WidgetBuilderExport, String> {
        let preset = self
            .presets
            .get(self.active_preset)
            .ok_or_else(|| "active preset missing".to_string())?;
        let preset_snapshot =
            PresetSnapshot::from_current(preset.name.clone(), &self.widgets, preset.read_only);
        Ok(WidgetBuilderExport {
            event: "widget_builder_export".to_string(),
            run_id: Self::export_run_id(),
            preset_id: preset_id(&preset.name),
            preset_name: preset.name.clone(),
            preset_index: self.active_preset,
            widget_count: self.widgets.len(),
            props_hash: props_hash(&self.widgets),
            preset: preset_snapshot,
            outcome: "ok".to_string(),
        })
    }

    fn export_path() -> String {
        std::env::var("FTUI_WIDGET_BUILDER_EXPORT_PATH")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "widget_builder_export.jsonl".to_string())
    }

    fn export_run_id() -> String {
        std::env::var("FTUI_WIDGET_BUILDER_RUN_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "widget_builder".to_string())
    }

    fn toggle_selected(&mut self, f: impl FnOnce(&mut WidgetConfig)) {
        if let Some(widget) = self.widgets.get_mut(self.selected_widget) {
            f(widget);
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let preset_name = self
            .presets
            .get(self.active_preset)
            .map(|p| p.name.as_str())
            .unwrap_or("Unknown");
        let info = format!(
            "Preset: {}  |  Widgets: {}  |  [P] cycle  [S] save  [X] export",
            preset_name,
            self.widgets.len()
        );
        Paragraph::new(info)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(area, frame);
    }

    fn render_presets(&self, frame: &mut Frame, area: Rect) {
        let items = self
            .presets
            .iter()
            .map(|preset| {
                let marker = if preset.read_only { "" } else { "*" };
                let label = format!("{}{}", preset.name, marker);
                ListItem::new(label)
            })
            .collect::<Vec<_>>();

        let list = List::new(items)
            .block(
                Block::new()
                    .title("Presets")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(theme::content_border()),
            )
            .highlight_style(theme::list_item_style(true, true))
            .highlight_symbol("> ");

        let mut state = self.preset_state.borrow_mut();
        state.select(Some(self.active_preset));
        StatefulWidget::render(&list, area, frame, &mut state);
    }

    fn render_widget_tree(&self, frame: &mut Frame, area: Rect) {
        let items = self
            .widgets
            .iter()
            .enumerate()
            .map(|(idx, widget)| {
                let enabled = if widget.enabled { "on" } else { "off" };
                let label = format!("{:02}. {} [{}]", idx + 1, widget.kind.label(), enabled);
                ListItem::new(label)
            })
            .collect::<Vec<_>>();

        let list = List::new(items)
            .block(
                Block::new()
                    .title("Widget Tree")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(theme::content_border()),
            )
            .highlight_style(theme::list_item_style(true, true))
            .highlight_symbol("> ");

        let mut state = self.widget_state.borrow_mut();
        state.select(Some(self.selected_widget));
        StatefulWidget::render(&list, area, frame, &mut state);
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        if self.widgets.is_empty() {
            Paragraph::new("No widgets in preset")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(area, frame);
            return;
        }

        let count = self.widgets.len() as u32;
        let constraints = self
            .widgets
            .iter()
            .map(|_| Constraint::Ratio(1, count))
            .collect::<Vec<_>>();
        let rows = Flex::vertical().constraints(constraints).split(area);

        for (idx, widget) in self.widgets.iter().enumerate() {
            if idx >= rows.len() {
                break;
            }
            self.render_widget(widget, idx == self.selected_widget, frame, rows[idx]);
        }
    }

    fn render_widget(&self, widget: &WidgetConfig, selected: bool, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let accent = widget.accent();
        let border_style = theme::panel_border_style(selected, accent);
        let borders = if widget.bordered {
            Borders::ALL
        } else {
            Borders::NONE
        };
        let mut block = Block::new()
            .borders(borders)
            .border_type(BorderType::Rounded)
            .style(border_style);
        if widget.show_title {
            block = block.title(widget.title.as_str());
        }
        block.render(area, frame);
        let inner = block.inner(area);
        if inner.is_empty() {
            return;
        }

        if !widget.enabled {
            Paragraph::new("(disabled)")
                .style(Style::new().fg(theme::fg::MUTED))
                .render(inner, frame);
            return;
        }

        match widget.kind {
            WidgetKind::Paragraph => {
                let text = "Compose widgets, tweak props, and observe layout changes.";
                Paragraph::new(text)
                    .style(Style::new().fg(accent))
                    .wrap(WrapMode::Word)
                    .render(inner, frame);
            }
            WidgetKind::List => {
                let items = [
                    ListItem::new("Wireframe"),
                    ListItem::new("Implement"),
                    ListItem::new("Polish"),
                    ListItem::new("Ship"),
                ];
                let mut state = ListState::default();
                let idx = (widget.value as usize) % items.len();
                state.select(Some(idx));
                let list = List::new(items)
                    .highlight_style(theme::list_item_style(true, true))
                    .highlight_symbol("*");
                StatefulWidget::render(&list, inner, frame, &mut state);
            }
            WidgetKind::Progress => {
                let ratio = (widget.value.min(100) as f64) / 100.0;
                let label = format!("{}%", widget.value.min(100));
                ProgressBar::new()
                    .ratio(ratio)
                    .label(&label)
                    .style(Style::new().fg(theme::fg::MUTED))
                    .gauge_style(Style::new().fg(accent).bg(theme::alpha::SURFACE))
                    .render(inner, frame);
            }
            WidgetKind::Sparkline => {
                let base = [3u64, 5, 2, 6, 7, 4, 8, 6, 5, 7, 3, 6];
                let bump = (widget.value / 10) as u64;
                let series = base.iter().map(|v| (*v + bump) as f64).collect::<Vec<_>>();
                Sparkline::new(&series)
                    .style(Style::new().fg(accent))
                    .render(inner, frame);
            }
            WidgetKind::Badge => {
                let label = if widget.value > 50 {
                    "ACTIVE"
                } else {
                    "STANDBY"
                };
                let badge = Badge::new(label)
                    .with_style(Style::new().fg(theme::fg::PRIMARY).bg(accent))
                    .with_padding(1, 1);
                badge.render(inner, frame);
            }
        }
    }

    fn render_props(&self, frame: &mut Frame, area: Rect) {
        let mut lines = Vec::new();
        if let Some(widget) = self.widgets.get(self.selected_widget) {
            lines.push(format!(
                "Selected: {} (#{})",
                widget.kind.label(),
                self.selected_widget + 1
            ));
            lines.push(format!("Enabled: {}  (E)", on_off(widget.enabled)));
            lines.push(format!("Border: {}  (B)", on_off(widget.bordered)));
            lines.push(format!("Title: {}   (T)", on_off(widget.show_title)));
            lines.push(format!(
                "Accent: {}  (C)",
                widget.accent_idx % ACCENTS.len() + 1
            ));
            lines.push(format!("Value: {}  ([ / ])", widget.value));
        }
        if let Some(ref export) = self.last_export {
            lines.push("".to_string());
            lines.push(format!("Export JSONL: {export}"));
        }

        let block = Block::new()
            .title("Props")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(theme::content_border());
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }
        Paragraph::new(lines.join("\n"))
            .wrap(WrapMode::Word)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(inner, frame);
    }
}

impl Screen for WidgetBuilder {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        else {
            return Cmd::None;
        };

        if modifiers.contains(Modifiers::CTRL)
            || modifiers.contains(Modifiers::ALT)
            || modifiers.contains(Modifiers::SUPER)
        {
            return Cmd::None;
        }

        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.widgets.is_empty() {
                    self.selected_widget = (self.selected_widget + 1) % self.widgets.len();
                    self.clamp_selection();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.widgets.is_empty() {
                    self.selected_widget = if self.selected_widget == 0 {
                        self.widgets.len() - 1
                    } else {
                        self.selected_widget - 1
                    };
                    self.clamp_selection();
                }
            }
            KeyCode::Char('p') => {
                let next = (self.active_preset + 1) % self.presets.len();
                self.load_preset(next);
            }
            KeyCode::Char('P') => {
                let prev = if self.active_preset == 0 {
                    self.presets.len() - 1
                } else {
                    self.active_preset - 1
                };
                self.load_preset(prev);
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.load_preset(self.active_preset);
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.save_preset();
            }
            KeyCode::Char('x') | KeyCode::Char('X') => {
                self.export_jsonl();
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                self.toggle_selected(|widget| widget.enabled = !widget.enabled);
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                self.toggle_selected(|widget| widget.bordered = !widget.bordered);
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                self.toggle_selected(|widget| widget.show_title = !widget.show_title);
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.toggle_selected(|widget| {
                    widget.accent_idx = (widget.accent_idx + 1) % ACCENTS.len();
                });
            }
            KeyCode::Char('[') => {
                self.toggle_selected(|widget| {
                    widget.value = widget.value.saturating_sub(5);
                });
            }
            KeyCode::Char(']') => {
                self.toggle_selected(|widget| {
                    widget.value = (widget.value + 5).min(100);
                });
            }
            _ => {}
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .title("Widget Builder Sandbox")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(theme::panel_border_style(
                true,
                theme::screen_accent::WIDGET_GALLERY,
            ));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(2), Constraint::Fill, Constraint::Fixed(6)])
            .split(inner);
        if rows.is_empty() {
            return;
        }

        self.render_header(frame, rows[0]);

        if rows.len() < 2 {
            return;
        }

        let body = rows[1];
        let columns = Flex::horizontal()
            .constraints([Constraint::Fixed(28), Constraint::Fill])
            .split(body);

        if !columns.is_empty() {
            let sidebar = columns[0];
            let preset_height = (sidebar.height / 2).max(3);
            let sections = Flex::vertical()
                .constraints([Constraint::Fixed(preset_height), Constraint::Fill])
                .split(sidebar);

            if let Some(preset_area) = sections.first().copied() {
                self.render_presets(frame, preset_area);
            }

            if let Some(tree_area) = sections.get(1).copied() {
                self.render_widget_tree(frame, tree_area);
            }
        }

        if columns.len() > 1 {
            let preview = columns[1];
            self.render_preview(frame, preview);
        }

        if rows.len() > 2 {
            self.render_props(frame, rows[2]);
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "J/K or Up/Down",
                action: "Select widget",
            },
            HelpEntry {
                key: "P / Shift+P",
                action: "Cycle presets",
            },
            HelpEntry {
                key: "S",
                action: "Save current as preset",
            },
            HelpEntry {
                key: "X",
                action: "Export JSONL",
            },
            HelpEntry {
                key: "E",
                action: "Toggle enabled",
            },
            HelpEntry {
                key: "B",
                action: "Toggle border",
            },
            HelpEntry {
                key: "T",
                action: "Toggle title",
            },
            HelpEntry {
                key: "C",
                action: "Cycle accent",
            },
            HelpEntry {
                key: "[ / ]",
                action: "Adjust value",
            },
            HelpEntry {
                key: "R",
                action: "Reset to preset",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Widget Builder"
    }

    fn tab_label(&self) -> &'static str {
        "Builder"
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn preset_id(name: &str) -> String {
    let mut id = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
        } else {
            id.push('_');
        }
    }
    id
}

fn props_hash(widgets: &[WidgetConfig]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a 64 offset
    for widget in widgets {
        hash = fnv_step(hash, widget.kind.tag() as u64);
        hash = fnv_step(hash, widget.enabled as u8 as u64);
        hash = fnv_step(hash, widget.bordered as u8 as u64);
        hash = fnv_step(hash, widget.show_title as u8 as u64);
        hash = fnv_step(hash, widget.accent_idx as u64);
        hash = fnv_step(hash, widget.value as u64);
        for b in widget.title.as_bytes() {
            hash = fnv_step(hash, *b as u64);
        }
    }
    hash
}

fn fnv_step(mut hash: u64, value: u64) -> u64 {
    hash ^= value;
    hash = hash.wrapping_mul(0x00000100000001B3);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn preset_load_resets_selection() {
        let mut builder = WidgetBuilder::new();
        builder.selected_widget = 2;
        builder.load_preset(1);
        assert_eq!(builder.active_preset, 1);
        assert_eq!(builder.selected_widget, 0);
        assert!(builder.widgets.len() >= 3);
    }

    #[test]
    fn save_preset_copies_widgets() {
        let mut builder = WidgetBuilder::new();
        let original_len = builder.presets.len();
        builder
            .widgets
            .push(WidgetConfig::new(WidgetKind::Badge, "Extra", 0));
        builder.save_preset();
        assert_eq!(builder.presets.len(), original_len + 1);
        assert_eq!(builder.active_preset, builder.presets.len() - 1);
        assert_eq!(
            builder.widgets.len(),
            builder.presets.last().unwrap().widgets.len()
        );
        assert!(!builder.presets.last().unwrap().read_only);
    }

    #[test]
    fn preset_snapshot_round_trip_json() {
        let preset = Preset {
            name: "Custom".to_string(),
            widgets: vec![
                WidgetConfig::new(WidgetKind::Badge, "Flag", 0),
                WidgetConfig::new(WidgetKind::Paragraph, "Note", 12),
            ],
            read_only: false,
        };
        let snapshot = PresetSnapshot::from_preset(&preset);
        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let decoded: PresetSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");
        let restored = decoded.into_preset().expect("restore preset");
        assert_eq!(restored.name, preset.name);
        assert_eq!(restored.read_only, preset.read_only);
        assert_eq!(restored.widgets.len(), preset.widgets.len());
        assert_eq!(restored.widgets[0].kind, preset.widgets[0].kind);
        assert_eq!(restored.widgets[0].title, preset.widgets[0].title);
    }

    #[test]
    fn export_jsonl_uses_props_hash() {
        let builder = WidgetBuilder::new();
        let hash = props_hash(&builder.widgets);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time ok")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ftui_widget_builder_export_{nanos}.jsonl"));
        let path_str = path.to_str().expect("path utf8");
        let line = builder.export_jsonl_to_path(path_str).expect("export line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("parse json");
        assert_eq!(value["props_hash"].as_u64().unwrap_or(0), hash);
        assert_eq!(
            value["widget_count"].as_u64().unwrap_or(0) as usize,
            builder.widgets.len()
        );
        assert_eq!(
            value["preset_name"].as_str().unwrap_or(""),
            builder
                .presets
                .get(builder.active_preset)
                .map(|preset| preset.name.as_str())
                .unwrap_or("")
        );
    }
}
