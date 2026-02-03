#![forbid(unsafe_code)]

//! Internationalization (i18n) demo screen (bd-ic6i.5).

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_i18n::catalog::{LocaleStrings, StringCatalog};
use ftui_i18n::plural::PluralForms;
use ftui_layout::{Constraint, Flex, FlowDirection};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

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

pub struct I18nDemo {
    locale_idx: usize,
    catalog: StringCatalog,
    plural_count: i64,
    interp_name: &'static str,
    panel: usize,
    tick_count: u64,
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
            tick_count: 0,
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

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let info = self.current_info();
        let pn = ["Overview", "Plurals", "RTL Layout"];
        let pl = pn.get(self.panel).unwrap_or(&"?");
        Paragraph::new(format!(
            " Tab/1-3: panels ({})  L/R: locale  Up/Down: count  Current: {} ({})  Dir: {} ",
            pl,
            info.name,
            info.tag,
            if info.rtl { "RTL" } else { "LTR" }
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
                KeyCode::Up => {
                    self.plural_count = self.plural_count.saturating_add(1);
                }
                KeyCode::Down => {
                    self.plural_count = (self.plural_count - 1).max(0);
                }
                KeyCode::Tab => {
                    self.panel = (self.panel + 1) % 3;
                }
                KeyCode::BackTab => {
                    self.panel = (self.panel + 2) % 3;
                }
                KeyCode::Char('1') => self.panel = 0,
                KeyCode::Char('2') => self.panel = 1,
                KeyCode::Char('3') => self.panel = 2,
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
        self.render_locale_bar(frame, rows[0]);
        match self.panel {
            0 => self.render_overview_panel(frame, rows[1]),
            1 => self.render_plural_panel(frame, rows[1]),
            2 => self.render_rtl_panel(frame, rows[1]),
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
                key: "Up/Down",
                action: "Change plural count",
            },
            HelpEntry {
                key: "Tab/1-3",
                action: "Switch panel",
            },
        ]
    }
    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }
    fn title(&self) -> &'static str {
        "i18n Demo"
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
        for e in [1, 2, 0] {
            d.update(&press(KeyCode::Tab));
            assert_eq!(d.panel, e);
        }
    }
    #[test]
    fn number_keys_select_panel() {
        let mut d = I18nDemo::new();
        d.update(&press(KeyCode::Char('3')));
        assert_eq!(d.panel, 2);
        d.update(&press(KeyCode::Char('1')));
        assert_eq!(d.panel, 0);
    }
    #[test]
    fn plural_count_adjustable() {
        let mut d = I18nDemo::new();
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
        for p in 0..3 {
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
}
