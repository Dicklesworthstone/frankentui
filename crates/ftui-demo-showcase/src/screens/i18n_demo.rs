#![forbid(unsafe_code)]

//! Internationalization (i18n) demo screen (bd-ic6i.5).

use std::cell::Cell;

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_i18n::catalog::{LocaleStrings, StringCatalog};
use ftui_i18n::plural::PluralForms;
use ftui_layout::{Constraint, Flex, FlowDirection};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::{
    display_width, grapheme_count, grapheme_width, graphemes, truncate_to_width_with_info,
    truncate_with_ellipsis, wrap_text, WrapMode,
};
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::Widget;
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;

use super::{HelpEntry, Screen};
use crate::theme;

const LOCALES: &[LocaleInfo] = &[
    LocaleInfo {
        tag: "en",
        name: "English",
        native: "English",
        rtl: false,
    },
    LocaleInfo {
        tag: "es",
        name: "Spanish",
        native: "Espa\u{f1}ol",
        rtl: false,
    },
    LocaleInfo {
        tag: "fr",
        name: "French",
        native: "Fran\u{e7}ais",
        rtl: false,
    },
    LocaleInfo {
        tag: "ru",
        name: "Russian",
        native: "\u{420}\u{443}\u{441}\u{441}\u{43a}\u{438}\u{439}",
        rtl: false,
    },
    LocaleInfo {
        tag: "ar",
        name: "Arabic",
        native: "\u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629}",
        rtl: true,
    },
    LocaleInfo {
        tag: "ja",
        name: "Japanese",
        native: "\u{65e5}\u{672c}\u{8a9e}",
        rtl: false,
    },
];

struct LocaleInfo {
    tag: &'static str,
    name: &'static str,
    native: &'static str,
    rtl: bool,
}

const PANEL_COUNT: usize = 4;
const PANEL_NAMES: [&str; PANEL_COUNT] = ["Overview", "Plurals", "RTL Layout", "Stress Lab"];

struct SampleCase {
    id: &'static str,
    label: &'static str,
    text: &'static str,
}

struct SampleSet {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    samples: &'static [SampleCase],
}

struct TrickyCase {
    label: &'static str,
    text: &'static str,
    expected_width: usize,
}

const SAMPLE_SET_COMBINING: &[SampleCase] = &[
    SampleCase {
        id: "combining_e_acute",
        label: "Combining acute",
        text: "e\u{0301}cole",
    },
    SampleCase {
        id: "angstrom",
        label: "A-ring (NFD)",
        text: "A\u{030A}ngstro\u{0308}m",
    },
    SampleCase {
        id: "stacked_marks",
        label: "Stacked marks",
        text: "Z\u{0335}\u{0336}\u{0337}algo",
    },
];

const SAMPLE_SET_CJK: &[SampleCase] = &[
    SampleCase {
        id: "cjk_hello",
        label: "CJK hello",
        text: "\u{4F60}\u{597D}\u{4E16}\u{754C}",
    },
    SampleCase {
        id: "japanese_sentence",
        label: "Japanese sentence",
        text: "\u{65E5}\u{672C}\u{8A9E}\u{306E}\u{9577}\u{3044}\u{6587}\u{7AE0}",
    },
    SampleCase {
        id: "mixed_kanji_kana",
        label: "Kanji + kana",
        text: "\u{6F22}\u{5B57}\u{3068}\u{304B}\u{306A}\u{306E}\u{6DF7}\u{5728}",
    },
];

const SAMPLE_SET_RTL: &[SampleCase] = &[
    SampleCase {
        id: "arabic_hello",
        label: "Arabic greeting",
        text:
            "\u{645}\u{631}\u{62D}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645}",
    },
    SampleCase {
        id: "arabic_mixed",
        label: "Arabic + Latin",
        text: "\u{645}\u{631}\u{62D}\u{628}\u{627} world 123",
    },
    SampleCase {
        id: "hebrew_hello",
        label: "Hebrew greeting",
        text: "\u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{05E2}\u{05D5}\u{05DC}\u{05DD}",
    },
];

const SAMPLE_SET_EMOJI: &[SampleCase] = &[
    SampleCase {
        id: "zwj_astronaut",
        label: "ZWJ astronaut",
        text: "\u{1F469}\u{200D}\u{1F680} \u{1F680}",
    },
    SampleCase {
        id: "family_emoji",
        label: "Family emoji",
        text: "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
    },
    SampleCase {
        id: "flags_skin_tone",
        label: "Flags + skin tone",
        text: "\u{1F1FA}\u{1F1F8} \u{1F1EF}\u{1F1F5} \u{1F44D}\u{1F3FD}",
    },
];

const SAMPLE_SETS: &[SampleSet] = &[
    SampleSet {
        id: "combining",
        name: "Combining Marks",
        description: "Diacritics and stacked marks",
        samples: SAMPLE_SET_COMBINING,
    },
    SampleSet {
        id: "cjk",
        name: "CJK Width",
        description: "Wide glyphs + mixed scripts",
        samples: SAMPLE_SET_CJK,
    },
    SampleSet {
        id: "rtl",
        name: "RTL Text",
        description: "Arabic/Hebrew with mixing",
        samples: SAMPLE_SET_RTL,
    },
    SampleSet {
        id: "emoji",
        name: "Emoji & ZWJ",
        description: "ZWJ sequences and flags",
        samples: SAMPLE_SET_EMOJI,
    },
];

const TRICKY_CASES: &[TrickyCase] = &[
    TrickyCase {
        label: "Combining",
        text: "e\u{0301}",
        expected_width: 1,
    },
    TrickyCase {
        label: "CJK",
        text: "\u{4F60}",
        expected_width: 2,
    },
    TrickyCase {
        label: "Emoji",
        text: "\u{1F44B}",
        expected_width: 2,
    },
    TrickyCase {
        label: "Flag",
        text: "\u{1F1FA}\u{1F1F8}",
        expected_width: 2,
    },
    TrickyCase {
        label: "ZWJ",
        text: "\u{1F469}\u{200D}\u{1F680}",
        expected_width: 2,
    },
];

pub struct I18nDemo {
    locale_idx: usize,
    catalog: StringCatalog,
    plural_count: i64,
    interp_name: &'static str,
    panel: usize,
    sample_set_idx: usize,
    sample_idx: usize,
    cursor_idx: usize,
    tick_count: u64,
    layout_locale_bar: Cell<Rect>,
    layout_panel: Cell<Rect>,
}

impl Default for I18nDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl I18nDemo {
    pub fn new() -> Self {
        Self {
            locale_idx: 0,
            catalog: build_catalog(),
            plural_count: 1,
            interp_name: "Alice",
            panel: 0,
            sample_set_idx: 0,
            sample_idx: 0,
            cursor_idx: 0,
            tick_count: 0,
            layout_locale_bar: Cell::new(Rect::new(0, 0, 0, 0)),
            layout_panel: Cell::new(Rect::new(0, 0, 0, 0)),
        }
    }

    fn current_locale(&self) -> &'static str {
        LOCALES[self.locale_idx].tag
    }
    fn current_info(&self) -> &'static LocaleInfo {
        &LOCALES[self.locale_idx]
    }
    fn flow(&self) -> FlowDirection {
        if self.current_info().rtl {
            FlowDirection::Rtl
        } else {
            FlowDirection::Ltr
        }
    }
    fn next_locale(&mut self) {
        self.locale_idx = (self.locale_idx + 1) % LOCALES.len();
    }
    fn prev_locale(&mut self) {
        self.locale_idx = (self.locale_idx + LOCALES.len() - 1) % LOCALES.len();
    }

    fn current_sample_set(&self) -> &'static SampleSet {
        &SAMPLE_SETS[self.sample_set_idx]
    }

    fn current_sample(&self) -> &'static SampleCase {
        &self.current_sample_set().samples[self.sample_idx]
    }

    fn sample_count(&self) -> usize {
        self.current_sample_set().samples.len()
    }

    fn clamp_cursor(&mut self) {
        let total = grapheme_count(self.current_sample().text);
        if total == 0 {
            self.cursor_idx = 0;
        } else if self.cursor_idx >= total {
            self.cursor_idx = total - 1;
        }
    }

    fn move_cursor(&mut self, delta: i32) {
        let total = grapheme_count(self.current_sample().text);
        if total == 0 {
            self.cursor_idx = 0;
            return;
        }
        let max_idx = (total - 1) as i32;
        let next = (self.cursor_idx as i32 + delta).clamp(0, max_idx);
        self.cursor_idx = next as usize;
    }

    fn next_sample_set(&mut self) {
        self.sample_set_idx = (self.sample_set_idx + 1) % SAMPLE_SETS.len();
        self.sample_idx = 0;
        self.cursor_idx = 0;
    }

    fn prev_sample_set(&mut self) {
        self.sample_set_idx = (self.sample_set_idx + SAMPLE_SETS.len() - 1) % SAMPLE_SETS.len();
        self.sample_idx = 0;
        self.cursor_idx = 0;
    }

    fn next_sample(&mut self) {
        let count = self.sample_count();
        if count == 0 {
            self.sample_idx = 0;
            self.cursor_idx = 0;
            return;
        }
        self.sample_idx = (self.sample_idx + 1) % count;
        self.cursor_idx = 0;
    }

    fn prev_sample(&mut self) {
        let count = self.sample_count();
        if count == 0 {
            self.sample_idx = 0;
            self.cursor_idx = 0;
            return;
        }
        self.sample_idx = (self.sample_idx + count - 1) % count;
        self.cursor_idx = 0;
    }

    fn toggle_rtl(&mut self) {
        let current_rtl = self.current_info().rtl;
        let target_rtl = !current_rtl;
        if let Some(idx) = LOCALES.iter().position(|loc| loc.rtl == target_rtl) {
            self.locale_idx = idx;
        }
    }

    fn reset_to_defaults(&mut self) {
        let catalog = std::mem::replace(&mut self.catalog, build_catalog());
        *self = Self::new();
        self.catalog = catalog;
    }

    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) -> Cmd<()> {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let locale_bar = self.layout_locale_bar.get();
                if locale_bar.contains(x, y) {
                    let rel_x = x.saturating_sub(locale_bar.x) as usize;
                    let half = locale_bar.width as usize / 2;
                    if rel_x < half {
                        self.prev_locale();
                    } else {
                        self.next_locale();
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                let panel = self.layout_panel.get();
                if panel.contains(x, y) {
                    if self.panel == 1 {
                        self.plural_count = self.plural_count.saturating_add(1);
                    } else if self.panel == 3 {
                        self.prev_sample();
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                let panel = self.layout_panel.get();
                if panel.contains(x, y) {
                    if self.panel == 1 {
                        self.plural_count = (self.plural_count - 1).max(0);
                    } else if self.panel == 3 {
                        self.next_sample();
                    }
                }
            }
            _ => {}
        }
        Cmd::None
    }

    fn report_path() -> String {
        std::env::var("FTUI_I18N_REPORT_PATH")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "i18n_stress_report.jsonl".to_string())
    }

    fn report_width() -> usize {
        std::env::var("FTUI_I18N_REPORT_WIDTH")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(32)
    }

    fn export_stress_report(&self) -> Result<String, String> {
        let path = Self::report_path();
        let max_width = Self::report_width();
        let set = self.current_sample_set();
        let sample = self.current_sample();
        let display = display_width(sample.text);
        let grapheme_total = grapheme_count(sample.text);
        let grapheme_sum: usize = graphemes(sample.text).map(grapheme_width).sum();
        let truncated = truncate_with_ellipsis(sample.text, max_width, "...");
        let truncated_width = display_width(&truncated);
        let truncated_flag = truncated_width < display;

        let payload = json!({
            "event": "i18n_stress_report",
            "set_id": set.id,
            "set_name": set.name,
            "sample_id": sample.id,
            "sample_label": sample.label,
            "width_metrics": {
                "display_width": display,
                "grapheme_count": grapheme_total,
                "grapheme_sum": grapheme_sum,
            },
            "truncation_state": {
                "max_width": max_width,
                "truncated": truncated_flag,
                "truncated_width": truncated_width,
                "text": truncated,
            },
            "outcome": "ok",
        });

        let line = serde_json::to_string(&payload).map_err(|err| err.to_string())?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| writeln!(file, "{}", line))
            .map_err(|err| err.to_string())?;

        Ok(path)
    }

    fn render_locale_bar(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let items: Vec<String> = LOCALES
            .iter()
            .enumerate()
            .map(|(i, loc)| {
                if i == self.locale_idx {
                    format!("[{}]", loc.native)
                } else {
                    loc.native.to_string()
                }
            })
            .collect();
        Paragraph::new(items.join("  "))
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::SURFACE))
            .alignment(Alignment::Center)
            .block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme::fg::MUTED)),
            )
            .render(area, frame);
    }

    fn render_overview_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let locale = self.current_locale();
        let info = self.current_info();
        let flow = self.flow();
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .flow_direction(flow)
            .gap(theme::spacing::INLINE)
            .split(area);
        {
            let title = self
                .catalog
                .get(locale, "demo.title")
                .unwrap_or("i18n Demo");
            let greeting = self.catalog.get(locale, "greeting").unwrap_or("Hello");
            let welcome = self
                .catalog
                .format(locale, "welcome", &[("name", self.interp_name)])
                .unwrap_or_else(|| format!("Welcome, {}!", self.interp_name));
            let dir = self
                .catalog
                .get(locale, "direction")
                .unwrap_or(if info.rtl { "RTL" } else { "LTR" });
            let text = format!(
                "--- {} ---\n\n  {}\n  {}\n\n  Locale: {} ({})\n  Direction: {}\n  Flow: {:?}",
                title, greeting, welcome, info.name, info.native, dir, flow
            );
            Paragraph::new(text)
                .style(Style::new().fg(theme::fg::PRIMARY))
                .block(
                    Block::new()
                        .title("String Lookup")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::new().fg(theme::accent::ACCENT_1)),
                )
                .render(cols[0], frame);
        }
        {
            let report = self.catalog.coverage_report();
            let mut lines = vec![
                "--- Coverage Report ---".to_string(),
                String::new(),
                format!("  Total keys: {}", report.total_keys),
                format!("  Locales: {}", report.locales.len()),
                String::new(),
            ];
            for lc in &report.locales {
                let marker = if lc.locale == locale { " <--" } else { "" };
                lines.push(format!(
                    "  {} {:.0}% ({}/{}){}",
                    lc.locale, lc.coverage_percent, lc.present, report.total_keys, marker
                ));
                if !lc.missing.is_empty() {
                    for key in &lc.missing {
                        lines.push(format!("    \u{2717} {}", key));
                    }
                }
            }
            lines.extend(["".into(), "  Fallback chain: en".into()]);
            Paragraph::new(lines.join("\n"))
                .style(Style::new().fg(theme::fg::PRIMARY))
                .block(
                    Block::new()
                        .title("Coverage Report")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::new().fg(theme::accent::ACCENT_3)),
                )
                .render(cols[1], frame);
        }
    }

    fn render_plural_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let locale = self.current_locale();
        let mut lines = vec![
            format!("--- Pluralization Demo (count = {}) ---", self.plural_count),
            String::new(),
        ];
        for loc in LOCALES {
            let items = self
                .catalog
                .format_plural(loc.tag, "items", self.plural_count, &[])
                .unwrap_or_else(|| "(missing)".into());
            let files = self
                .catalog
                .format_plural(loc.tag, "files", self.plural_count, &[])
                .unwrap_or_else(|| "(missing)".into());
            lines.push(format!(
                "  {} ({}){}:",
                loc.name,
                loc.tag,
                if loc.tag == locale { " <--" } else { "" }
            ));
            lines.push(format!("    items: {}", items));
            lines.push(format!("    files: {}", files));
            lines.push(String::new());
        }
        lines.push("  Use Up/Down to change count".into());
        lines.push("  Counts to try: 0, 1, 2, 3, 5, 11, 21, 100, 101".into());
        Paragraph::new(lines.join("\n"))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .block(
                Block::new()
                    .title("Pluralization Rules")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme::accent::ACCENT_1)),
            )
            .render(area, frame);
    }

    fn render_rtl_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(3), Constraint::Fill, Constraint::Fill])
            .gap(0)
            .split(area);
        Paragraph::new("RTL Layout Mirroring \u{2014} Flex children reverse in RTL")
            .style(Style::new().fg(theme::fg::PRIMARY))
            .alignment(Alignment::Center)
            .block(
                Block::new()
                    .borders(Borders::BOTTOM)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme::fg::MUTED)),
            )
            .render(rows[0], frame);
        self.render_direction_sample(frame, rows[1], FlowDirection::Ltr);
        self.render_direction_sample(frame, rows[2], FlowDirection::Rtl);
    }

    fn render_direction_sample(&self, frame: &mut Frame, area: Rect, flow: FlowDirection) {
        let label = if flow.is_rtl() { "RTL" } else { "LTR" };
        let bc = if flow.is_rtl() {
            theme::accent::ACCENT_1
        } else {
            theme::accent::ACCENT_3
        };
        let title_s = format!("{} Layout", label);
        let outer = Block::new()
            .title(title_s.as_str())
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(bc));
        let inner = outer.inner(area);
        outer.render(area, frame);
        if inner.is_empty() {
            return;
        }
        let cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(30.0),
                Constraint::Percentage(40.0),
                Constraint::Percentage(30.0),
            ])
            .flow_direction(flow)
            .gap(theme::spacing::INLINE)
            .split(inner);
        let labels = ["Sidebar", "Content", "Panel"];
        let fgs = [
            theme::accent::ACCENT_1,
            theme::fg::PRIMARY,
            theme::accent::ACCENT_3,
        ];
        for (i, (&col, &lbl)) in cols.iter().zip(labels.iter()).enumerate() {
            if col.is_empty() {
                continue;
            }
            Paragraph::new(format!("{} ({})", lbl, i + 1))
                .style(Style::new().fg(fgs[i]))
                .alignment(Alignment::Center)
                .block(
                    Block::new()
                        .borders(Borders::ALL)
                        .border_style(Style::new().fg(theme::fg::MUTED)),
                )
                .render(col, frame);
        }
    }

    fn render_stress_lab_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        if area.height < 10 {
            let sample = self.current_sample();
            let width = display_width(sample.text);
            let text = format!(
                "Stress Lab (compact)\nSample: {}\n{}\nWidth: {} cells",
                sample.label, sample.text, width
            );
            Paragraph::new(text)
                .style(Style::new().fg(theme::fg::PRIMARY))
                .block(
                    Block::new()
                        .title("Unicode Stress Lab")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::new().fg(theme::accent::ACCENT_2)),
                )
                .render(area, frame);
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(4), Constraint::Fill, Constraint::Fixed(8)])
            .gap(1)
            .split(area);
        self.render_stress_header(frame, rows[0]);
        self.render_stress_body(frame, rows[1]);
        self.render_stress_footer(frame, rows[2]);
    }

    fn render_stress_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let set = self.current_sample_set();
        let sample = self.current_sample();
        let total_width = display_width(sample.text);
        let grapheme_total = grapheme_count(sample.text);
        let grapheme_sum: usize = graphemes(sample.text).map(grapheme_width).sum();
        let width_ok = total_width == grapheme_sum;
        let status = if width_ok { "ok" } else { "diff" };
        let lines = [
            format!("Set: {} \u{2014} {}", set.name, set.description),
            format!(
                "Sample {}/{}: {} ({})",
                self.sample_idx + 1,
                self.sample_count().max(1),
                sample.label,
                sample.id
            ),
            format!(
                "Width: {} cells | Graphemes: {} | Sum widths: {} ({})",
                total_width, grapheme_total, grapheme_sum, status
            ),
            "Shift+Left/Right: cursor  Up/Down: sample  [ / ]: set  E: export".to_string(),
        ];
        Paragraph::new(lines.join("\n"))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .block(
                Block::new()
                    .title("Unicode Stress Lab")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme::accent::ACCENT_2)),
            )
            .render(area, frame);
    }

    fn render_stress_body(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let sample = self.current_sample();
        let outer = Block::new()
            .title("Wrap + Truncate")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::accent::ACCENT_3));
        let inner = outer.inner(area);
        outer.render(area, frame);
        if inner.is_empty() {
            return;
        }

        let wrap_width = inner.width.saturating_sub(2).max(8) as usize;
        let wrapped = wrap_text(sample.text, wrap_width, WrapMode::WordChar);
        let truncated = truncate_with_ellipsis(sample.text, wrap_width, "...");
        let truncated_width = display_width(&truncated);
        let (plain_trunc, plain_width) = truncate_to_width_with_info(sample.text, wrap_width);

        let mut lines = Vec::new();
        lines.push("Original:".to_string());
        lines.push(sample.text.to_string());
        lines.push(String::new());
        lines.push(format!("Wrapped ({} cols):", wrap_width));
        for line in wrapped.iter().take(3) {
            lines.push(line.clone());
        }
        if wrapped.len() > 3 {
            lines.push("...".to_string());
        }
        lines.push(String::new());
        lines.push(format!(
            "Truncate ({} cols): {} (w={})",
            wrap_width, truncated, truncated_width
        ));
        lines.push(format!("Raw truncate: {} (w={})", plain_trunc, plain_width));

        Paragraph::new(lines.join("\n"))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
    }

    fn render_stress_footer(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        if area.width < 60 {
            let rows = Flex::vertical()
                .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
                .gap(1)
                .split(area);
            self.render_grapheme_inspector(frame, rows[0]);
            self.render_tricky_cases(frame, rows[1]);
            return;
        }

        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(55.0), Constraint::Percentage(45.0)])
            .gap(1)
            .split(area);
        self.render_grapheme_inspector(frame, cols[0]);
        self.render_tricky_cases(frame, cols[1]);
    }

    fn render_grapheme_inspector(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let sample = self.current_sample();
        let grapheme_list: Vec<&str> = graphemes(sample.text).collect();
        let total = grapheme_list.len();
        let mut lines = Vec::new();
        if total == 0 {
            lines.push("No graphemes found.".to_string());
        } else {
            let cursor = self.cursor_idx.min(total - 1);
            let header = format!("Cursor {}/{}  (Shift+Left/Right)", cursor + 1, total);
            lines.push(header);
            lines.push("Idx  Grapheme  Width  Bytes".to_string());

            let max_rows = area.height.saturating_sub(3) as usize;
            let window = max_rows.max(1);
            let start = cursor.saturating_sub(window / 2);
            let end = (start + window).min(total);
            let start = end.saturating_sub(window);

            for (offset, g) in grapheme_list[start..end].iter().enumerate() {
                let idx = start + offset;
                let g = *g;
                let width = grapheme_width(g);
                let bytes = g.len();
                let marker = if idx == cursor { ">" } else { " " };
                lines.push(format!(
                    "{marker}{idx:>2}  {g}  w={width}  b={bytes}",
                    marker = marker,
                    idx = idx,
                    g = g,
                    width = width,
                    bytes = bytes
                ));
            }
        }

        Paragraph::new(lines.join("\n"))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .block(
                Block::new()
                    .title("Grapheme Inspector")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme::accent::ACCENT_1)),
            )
            .render(area, frame);
    }

    fn render_tricky_cases(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        lines.push("Known tricky cases (expected width)".to_string());
        lines.push(String::new());
        for case in TRICKY_CASES {
            let measured = display_width(case.text);
            let ok = measured == case.expected_width;
            let status = if ok { "ok" } else { "diff" };
            let preview_width = area.width.saturating_sub(16).max(4) as usize;
            let preview = truncate_with_ellipsis(case.text, preview_width, "...");
            lines.push(format!(
                "{:<9} {:<8} exp={} got={} {}",
                case.label, preview, case.expected_width, measured, status
            ));
        }
        Paragraph::new(lines.join("\n"))
            .style(Style::new().fg(theme::fg::PRIMARY))
            .block(
                Block::new()
                    .title("Tricky Widths")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme::accent::ACCENT_3)),
            )
            .render(area, frame);
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let info = self.current_info();
        let pl = PANEL_NAMES.get(self.panel).unwrap_or(&"?");
        let detail = if self.panel == 3 {
            let sample_total = self.sample_count().max(1);
            let graphemes = grapheme_count(self.current_sample().text);
            format!(
                "Set: {}  Sample: {}/{}  Cursor: {}/{}",
                self.current_sample_set().id,
                self.sample_idx + 1,
                sample_total,
                self.cursor_idx + 1,
                graphemes.max(1)
            )
        } else {
            format!("Dir: {}", if info.rtl { "RTL" } else { "LTR" })
        };
        Paragraph::new(format!(
            " Tab/1-4: panels ({})  L/R: locale  {}  Current: {} ({}) ",
            pl, detail, info.name, info.tag
        ))
        .style(
            Style::new()
                .fg(theme::bg::SURFACE)
                .bg(theme::accent::ACCENT_1),
        )
        .render(area, frame);
    }
}

impl Screen for I18nDemo {
    type Message = ();
    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            return self.handle_mouse(mouse.kind, mouse.x, mouse.y);
        }
        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
            ..
        }) = event
        {
            let shift = modifiers.contains(Modifiers::SHIFT);
            match code {
                KeyCode::Right if !shift => self.next_locale(),
                KeyCode::Left if !shift => self.prev_locale(),
                KeyCode::Right if shift && self.panel == 3 => self.move_cursor(1),
                KeyCode::Left if shift && self.panel == 3 => self.move_cursor(-1),
                KeyCode::Up => {
                    if self.panel == 1 {
                        self.plural_count = self.plural_count.saturating_add(1);
                    } else if self.panel == 3 {
                        self.prev_sample();
                    }
                }
                KeyCode::Down => {
                    if self.panel == 1 {
                        self.plural_count = (self.plural_count - 1).max(0);
                    } else if self.panel == 3 {
                        self.next_sample();
                    }
                }
                KeyCode::Char('[') if self.panel == 3 => self.prev_sample_set(),
                KeyCode::Char(']') if self.panel == 3 => self.next_sample_set(),
                KeyCode::Char('e') | KeyCode::Char('E') if self.panel == 3 => {
                    let _ = self.export_stress_report();
                }
                KeyCode::Char('d') | KeyCode::Char('D') => self.toggle_rtl(),
                KeyCode::Char('l') | KeyCode::Char('L') => self.next_locale(),
                KeyCode::Char('r') | KeyCode::Char('R') => self.reset_to_defaults(),
                KeyCode::Tab => {
                    self.panel = (self.panel + 1) % PANEL_COUNT;
                }
                KeyCode::BackTab => {
                    self.panel = (self.panel + PANEL_COUNT - 1) % PANEL_COUNT;
                }
                KeyCode::Char('1') => self.panel = 0,
                KeyCode::Char('2') => self.panel = 1,
                KeyCode::Char('3') => self.panel = 2,
                KeyCode::Char('4') => self.panel = 3,
                _ => {}
            }
        }
        Cmd::None
    }
    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(3), Constraint::Fill, Constraint::Fixed(1)])
            .split(area);
        self.layout_locale_bar.set(rows[0]);
        self.layout_panel.set(rows[1]);
        self.render_locale_bar(frame, rows[0]);
        match self.panel {
            0 => self.render_overview_panel(frame, rows[1]),
            1 => self.render_plural_panel(frame, rows[1]),
            2 => self.render_rtl_panel(frame, rows[1]),
            3 => self.render_stress_lab_panel(frame, rows[1]),
            _ => {}
        }
        self.render_status_bar(frame, rows[2]);
    }
    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "Left/Right",
                action: "Switch locale",
            },
            HelpEntry {
                key: "Shift+Left/Right",
                action: "Move grapheme cursor (Stress)",
            },
            HelpEntry {
                key: "Up/Down",
                action: "Count (Plurals) / Sample (Stress)",
            },
            HelpEntry {
                key: "[ / ]",
                action: "Switch sample set (Stress)",
            },
            HelpEntry {
                key: "E",
                action: "Export stress report (Stress)",
            },
            HelpEntry {
                key: "D",
                action: "Toggle RTL/LTR locale",
            },
            HelpEntry {
                key: "L",
                action: "Cycle to next locale",
            },
            HelpEntry {
                key: "R",
                action: "Reset to defaults",
            },
            HelpEntry {
                key: "Click",
                action: "Locale bar left/right half",
            },
            HelpEntry {
                key: "Wheel",
                action: "Scroll count/sample in panel",
            },
            HelpEntry {
                key: "Tab/1-4",
                action: "Switch panel",
            },
        ]
    }
    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }
    fn title(&self) -> &'static str {
        "i18n Stress Lab"
    }
    fn tab_label(&self) -> &'static str {
        "i18n"
    }
}

fn build_catalog() -> StringCatalog {
    let mut catalog = StringCatalog::new();
    let mut en = LocaleStrings::new();
    en.insert("demo.title", "Internationalization");
    en.insert("greeting", "Hello!");
    en.insert("welcome", "Welcome, {name}!");
    en.insert("direction", "Left-to-Right");
    en.insert_plural(
        "items",
        PluralForms {
            one: "{count} item".into(),
            other: "{count} items".into(),
            ..Default::default()
        },
    );
    en.insert_plural(
        "files",
        PluralForms {
            one: "{count} file".into(),
            other: "{count} files".into(),
            ..Default::default()
        },
    );
    catalog.add_locale("en", en);

    let mut es = LocaleStrings::new();
    es.insert("demo.title", "Internacionalizaci\u{f3}n");
    es.insert("greeting", "\u{a1}Hola!");
    es.insert("welcome", "\u{a1}Bienvenido, {name}!");
    es.insert("direction", "Izquierda a derecha");
    es.insert_plural(
        "items",
        PluralForms {
            one: "{count} elemento".into(),
            other: "{count} elementos".into(),
            ..Default::default()
        },
    );
    es.insert_plural(
        "files",
        PluralForms {
            one: "{count} archivo".into(),
            other: "{count} archivos".into(),
            ..Default::default()
        },
    );
    catalog.add_locale("es", es);

    let mut fr = LocaleStrings::new();
    fr.insert("demo.title", "Internationalisation");
    fr.insert("greeting", "Bonjour\u{a0}!");
    fr.insert("welcome", "Bienvenue, {name}\u{a0}!");
    fr.insert("direction", "Gauche \u{e0} droite");
    fr.insert_plural(
        "items",
        PluralForms {
            one: "{count} \u{e9}l\u{e9}ment".into(),
            other: "{count} \u{e9}l\u{e9}ments".into(),
            ..Default::default()
        },
    );
    fr.insert_plural(
        "files",
        PluralForms {
            one: "{count} fichier".into(),
            other: "{count} fichiers".into(),
            ..Default::default()
        },
    );
    catalog.add_locale("fr", fr);

    let mut ru = LocaleStrings::new();
    ru.insert("demo.title", "\u{418}\u{43d}\u{442}\u{435}\u{440}\u{43d}\u{430}\u{446}\u{438}\u{43e}\u{43d}\u{430}\u{43b}\u{438}\u{437}\u{430}\u{446}\u{438}\u{44f}");
    ru.insert("greeting", "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}!");
    ru.insert("welcome", "\u{414}\u{43e}\u{431}\u{440}\u{43e} \u{43f}\u{43e}\u{436}\u{430}\u{43b}\u{43e}\u{432}\u{430}\u{442}\u{44c}, {name}!");
    ru.insert(
        "direction",
        "\u{421}\u{43b}\u{435}\u{432}\u{430} \u{43d}\u{430}\u{43f}\u{440}\u{430}\u{432}\u{43e}",
    );
    ru.insert_plural(
        "items",
        PluralForms {
            one: "{count} \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}".into(),
            few: Some("{count} \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{430}".into()),
            many: Some(
                "{count} \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{43e}\u{432}".into(),
            ),
            other: "{count} \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{43e}\u{432}".into(),
            ..Default::default()
        },
    );
    ru.insert_plural(
        "files",
        PluralForms {
            one: "{count} \u{444}\u{430}\u{439}\u{43b}".into(),
            few: Some("{count} \u{444}\u{430}\u{439}\u{43b}\u{430}".into()),
            many: Some("{count} \u{444}\u{430}\u{439}\u{43b}\u{43e}\u{432}".into()),
            other: "{count} \u{444}\u{430}\u{439}\u{43b}\u{43e}\u{432}".into(),
            ..Default::default()
        },
    );
    catalog.add_locale("ru", ru);

    let mut ar = LocaleStrings::new();
    ar.insert(
        "demo.title",
        "\u{627}\u{644}\u{62a}\u{62f}\u{648}\u{64a}\u{644}",
    );
    ar.insert("greeting", "\u{645}\u{631}\u{62d}\u{628}\u{627}\u{64b}!");
    ar.insert("welcome", "\u{623}\u{647}\u{644}\u{627}\u{64b} {name}!");
    ar.insert("direction", "\u{645}\u{646} \u{627}\u{644}\u{64a}\u{645}\u{64a}\u{646} \u{625}\u{644}\u{649} \u{627}\u{644}\u{64a}\u{633}\u{627}\u{631}");
    ar.insert_plural(
        "items",
        PluralForms {
            zero: Some("{count} \u{639}\u{646}\u{627}\u{635}\u{631}".into()),
            one: "\u{639}\u{646}\u{635}\u{631} \u{648}\u{627}\u{62d}\u{62f}".into(),
            two: Some("\u{639}\u{646}\u{635}\u{631}\u{627}\u{646}".into()),
            few: Some("{count} \u{639}\u{646}\u{627}\u{635}\u{631}".into()),
            many: Some("{count} \u{639}\u{646}\u{635}\u{631}\u{627}\u{64b}".into()),
            other: "{count} \u{639}\u{646}\u{635}\u{631}".into(),
        },
    );
    ar.insert_plural(
        "files",
        PluralForms {
            zero: Some("{count} \u{645}\u{644}\u{641}\u{627}\u{62a}".into()),
            one: "\u{645}\u{644}\u{641} \u{648}\u{627}\u{62d}\u{62f}".into(),
            two: Some("\u{645}\u{644}\u{641}\u{627}\u{646}".into()),
            few: Some("{count} \u{645}\u{644}\u{641}\u{627}\u{62a}".into()),
            many: Some("{count} \u{645}\u{644}\u{641}\u{64b}\u{627}".into()),
            other: "{count} \u{645}\u{644}\u{641}".into(),
        },
    );
    catalog.add_locale("ar", ar);

    let mut ja = LocaleStrings::new();
    ja.insert("demo.title", "\u{56fd}\u{969b}\u{5316}");
    ja.insert(
        "greeting",
        "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}\u{ff01}",
    );
    ja.insert(
        "welcome",
        "\u{3088}\u{3046}\u{3053}\u{305d}\u{3001}{name}\u{3055}\u{3093}\u{ff01}",
    );
    ja.insert("direction", "\u{5de6}\u{304b}\u{3089}\u{53f3}");
    ja.insert_plural(
        "items",
        PluralForms {
            one: "{count}\u{500b}\u{306e}\u{30a2}\u{30a4}\u{30c6}\u{30e0}".into(),
            other: "{count}\u{500b}\u{306e}\u{30a2}\u{30a4}\u{30c6}\u{30e0}".into(),
            ..Default::default()
        },
    );
    ja.insert_plural(
        "files",
        PluralForms {
            one: "{count}\u{500b}\u{306e}\u{30d5}\u{30a1}\u{30a4}\u{30eb}".into(),
            other: "{count}\u{500b}\u{306e}\u{30d5}\u{30a1}\u{30a4}\u{30eb}".into(),
            ..Default::default()
        },
    );
    catalog.add_locale("ja", ja);

    catalog.set_fallback_chain(vec!["en".into()]);
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::MouseEvent;
    use ftui_render::grapheme_pool::GraphemePool;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
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
    fn render_hash(screen: &I18nDemo, w: u16, h: u16) -> u64 {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(w, h, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, w, h));
        let mut hasher = DefaultHasher::new();
        for y in 0..h {
            for x in 0..w {
                if let Some(ch) = frame
                    .buffer
                    .get(x, y)
                    .and_then(|cell| cell.content.as_char())
                {
                    ch.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }

    #[test]
    fn default_locale_is_english() {
        assert_eq!(I18nDemo::new().current_locale(), "en");
    }
    #[test]
    fn cycle_locales() {
        let mut d = I18nDemo::new();
        for e in ["es", "fr", "ru", "ar", "ja", "en"] {
            d.next_locale();
            assert_eq!(d.current_locale(), e);
        }
    }
    #[test]
    fn prev_locale_wraps() {
        let mut d = I18nDemo::new();
        d.prev_locale();
        assert_eq!(d.current_locale(), "ja");
    }
    #[test]
    fn arabic_is_rtl() {
        let mut d = I18nDemo::new();
        while d.current_locale() != "ar" {
            d.next_locale();
        }
        assert!(d.current_info().rtl);
        assert_eq!(d.flow(), FlowDirection::Rtl);
    }
    #[test]
    fn catalog_has_all_locales() {
        let c = build_catalog();
        let l = c.locales();
        for loc in LOCALES {
            assert!(l.contains(&loc.tag), "missing: {}", loc.tag);
        }
    }
    #[test]
    fn catalog_greeting_all() {
        let c = build_catalog();
        for loc in LOCALES {
            assert!(
                c.get(loc.tag, "greeting").is_some(),
                "no greeting: {}",
                loc.tag
            );
        }
    }
    #[test]
    fn catalog_plurals_english() {
        let c = build_catalog();
        assert_eq!(
            c.format_plural("en", "items", 1, &[]),
            Some("1 item".into())
        );
        assert_eq!(
            c.format_plural("en", "items", 5, &[]),
            Some("5 items".into())
        );
    }
    #[test]
    fn catalog_plurals_russian() {
        let c = build_catalog();
        assert_eq!(
            c.format_plural("ru", "files", 1, &[]),
            Some("1 \u{444}\u{430}\u{439}\u{43b}".into())
        );
        assert_eq!(
            c.format_plural("ru", "files", 3, &[]),
            Some("3 \u{444}\u{430}\u{439}\u{43b}\u{430}".into())
        );
        assert_eq!(
            c.format_plural("ru", "files", 5, &[]),
            Some("5 \u{444}\u{430}\u{439}\u{43b}\u{43e}\u{432}".into())
        );
    }
    #[test]
    fn catalog_interpolation() {
        assert_eq!(
            build_catalog().format("en", "welcome", &[("name", "Bob")]),
            Some("Welcome, Bob!".into())
        );
    }
    #[test]
    fn catalog_fallback() {
        let c = build_catalog();
        assert!(c.get("ja", "greeting").is_some());
        assert_eq!(c.get("xx", "greeting"), Some("Hello!"));
    }
    #[test]
    fn render_produces_output() {
        assert_ne!(render_hash(&I18nDemo::new(), 120, 40), 0);
    }
    #[test]
    fn render_deterministic() {
        let d = I18nDemo::new();
        assert_eq!(render_hash(&d, 80, 24), render_hash(&d, 80, 24));
    }
    #[test]
    fn panel_switching() {
        let mut d = I18nDemo::new();
        for e in [1, 2, 3, 0] {
            d.update(&press(KeyCode::Tab));
            assert_eq!(d.panel, e);
        }
    }
    #[test]
    fn number_keys_select_panel() {
        let mut d = I18nDemo::new();
        d.update(&press(KeyCode::Char('3')));
        assert_eq!(d.panel, 2);
        d.update(&press(KeyCode::Char('4')));
        assert_eq!(d.panel, 3);
        d.update(&press(KeyCode::Char('1')));
        assert_eq!(d.panel, 0);
    }
    #[test]
    fn plural_count_adjustable() {
        let mut d = I18nDemo::new();
        d.panel = 1;
        d.update(&press(KeyCode::Up));
        assert_eq!(d.plural_count, 2);
        d.update(&press(KeyCode::Down));
        assert_eq!(d.plural_count, 1);
        d.update(&press(KeyCode::Down));
        assert_eq!(d.plural_count, 0);
        d.update(&press(KeyCode::Down));
        assert_eq!(d.plural_count, 0);
    }
    #[test]
    fn all_panels_render_each_locale() {
        let mut d = I18nDemo::new();
        for p in 0..PANEL_COUNT {
            d.panel = p;
            for (i, locale) in LOCALES.iter().enumerate() {
                d.locale_idx = i;
                assert_ne!(render_hash(&d, 100, 30), 0, "p={} l={}", p, locale.tag);
            }
        }
    }
    #[test]
    fn locale_key_events() {
        let mut d = I18nDemo::new();
        d.update(&press(KeyCode::Right));
        assert_eq!(d.current_locale(), "es");
        d.update(&press(KeyCode::Left));
        assert_eq!(d.current_locale(), "en");
    }
    #[test]
    fn small_terminal_no_panic() {
        assert_ne!(render_hash(&I18nDemo::new(), 30, 8), 0);
    }
    #[test]
    fn sample_sets_have_samples() {
        for set in SAMPLE_SETS {
            assert!(
                !set.samples.is_empty(),
                "sample set should not be empty: {}",
                set.id
            );
        }
    }
    #[test]
    fn sample_widths_match_grapheme_sum() {
        for set in SAMPLE_SETS {
            for sample in set.samples {
                let total = display_width(sample.text);
                let sum: usize = graphemes(sample.text).map(grapheme_width).sum();
                assert_eq!(
                    total, sum,
                    "width mismatch for sample {} (set {})",
                    sample.id, set.id
                );
            }
        }
    }

    #[test]
    fn toggle_rtl_switches_locale() {
        let mut d = I18nDemo::new();
        assert!(!d.current_info().rtl);
        d.toggle_rtl();
        assert!(d.current_info().rtl);
        assert_eq!(d.current_locale(), "ar");
        d.toggle_rtl();
        assert!(!d.current_info().rtl);
        assert_eq!(d.current_locale(), "en");
    }

    #[test]
    fn reset_to_defaults_restores_state() {
        let mut d = I18nDemo::new();
        d.locale_idx = 3;
        d.panel = 2;
        d.plural_count = 42;
        d.reset_to_defaults();
        assert_eq!(d.locale_idx, 0);
        assert_eq!(d.panel, 0);
        assert_eq!(d.plural_count, 1);
    }

    #[test]
    fn d_key_toggles_rtl() {
        let mut d = I18nDemo::new();
        d.update(&press(KeyCode::Char('d')));
        assert!(d.current_info().rtl);
        d.update(&press(KeyCode::Char('D')));
        assert!(!d.current_info().rtl);
    }

    #[test]
    fn l_key_cycles_locale() {
        let mut d = I18nDemo::new();
        d.update(&press(KeyCode::Char('l')));
        assert_eq!(d.current_locale(), "es");
        d.update(&press(KeyCode::Char('L')));
        assert_eq!(d.current_locale(), "fr");
    }

    #[test]
    fn r_key_resets() {
        let mut d = I18nDemo::new();
        d.panel = 3;
        d.locale_idx = 4;
        d.update(&press(KeyCode::Char('r')));
        assert_eq!(d.panel, 0);
        assert_eq!(d.locale_idx, 0);
    }

    #[test]
    fn mouse_click_locale_bar_switches_locale() {
        let mut d = I18nDemo::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        d.view(&mut frame, Rect::new(0, 0, 80, 24));
        d.update(&mouse_click(50, 1));
        assert_eq!(d.current_locale(), "es");
        d.update(&mouse_click(10, 1));
        assert_eq!(d.current_locale(), "en");
    }

    #[test]
    fn mouse_scroll_plural_panel() {
        let mut d = I18nDemo::new();
        d.panel = 1;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        d.view(&mut frame, Rect::new(0, 0, 80, 24));
        d.update(&mouse_scroll_up(40, 5));
        assert_eq!(d.plural_count, 2);
        d.update(&mouse_scroll_down(40, 5));
        assert_eq!(d.plural_count, 1);
    }

    #[test]
    fn mouse_scroll_stress_panel() {
        let mut d = I18nDemo::new();
        d.panel = 3;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        d.view(&mut frame, Rect::new(0, 0, 80, 24));
        assert_eq!(d.sample_idx, 0);
        d.update(&mouse_scroll_down(40, 10));
        assert_eq!(d.sample_idx, 1);
        d.update(&mouse_scroll_up(40, 10));
        assert_eq!(d.sample_idx, 0);
    }

    #[test]
    fn keybindings_include_new_entries() {
        let d = I18nDemo::new();
        let bindings = d.keybindings();
        let keys: Vec<&str> = bindings.iter().map(|b| b.key).collect();
        assert!(keys.contains(&"D"), "missing D keybinding");
        assert!(keys.contains(&"L"), "missing L keybinding");
        assert!(keys.contains(&"R"), "missing R keybinding");
        assert!(keys.contains(&"Click"), "missing Click keybinding");
        assert!(keys.contains(&"Wheel"), "missing Wheel keybinding");
    }
}
