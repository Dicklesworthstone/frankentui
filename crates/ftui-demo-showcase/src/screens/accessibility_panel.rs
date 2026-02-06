#![forbid(unsafe_code)]

//! Accessibility Control Panel — demonstrates a11y modes and contrast checks.
//!
//! bd-iuvb.8

use std::collections::VecDeque;

use std::cell::Cell;

use ftui_core::event::{Event, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::{Line, Span, Text};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::app::{A11yEventKind, A11yTelemetryEvent};
use crate::theme;

const MAX_EVENTS: usize = 6;

#[derive(Clone, Copy)]
struct A11yEventEntry {
    kind: A11yEventKind,
    tick: u64,
    high_contrast: bool,
    reduced_motion: bool,
    large_text: bool,
}

/// Accessibility control panel screen.
pub struct AccessibilityPanel {
    a11y: theme::A11ySettings,
    base_theme: theme::ThemeId,
    events: VecDeque<A11yEventEntry>,
    layout_toggles: Cell<Rect>,
    layout_wcag: Cell<Rect>,
}

impl Default for AccessibilityPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AccessibilityPanel {
    /// Create a new accessibility control panel.
    pub fn new() -> Self {
        Self {
            a11y: theme::A11ySettings::default(),
            base_theme: theme::ThemeId::CyberpunkAurora,
            events: VecDeque::with_capacity(MAX_EVENTS),
            layout_toggles: Cell::new(Rect::default()),
            layout_wcag: Cell::new(Rect::default()),
        }
    }

    /// Sync the screen state with the app-level accessibility settings.
    pub fn sync_a11y(&mut self, a11y: theme::A11ySettings, base_theme: theme::ThemeId) {
        self.a11y = a11y;
        self.base_theme = base_theme;
    }

    /// Record a telemetry event for the panel display.
    pub fn record_event(&mut self, event: &A11yTelemetryEvent) {
        let entry = A11yEventEntry {
            kind: event.kind,
            tick: event.tick,
            high_contrast: event.high_contrast,
            reduced_motion: event.reduced_motion,
            large_text: event.large_text,
        };
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(entry);
    }

    fn contrast_ratio(fg: PackedRgba, bg: PackedRgba) -> f32 {
        fn linearize(v: f32) -> f32 {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(c: PackedRgba) -> f32 {
            let r = linearize(c.r() as f32 / 255.0);
            let g = linearize(c.g() as f32 / 255.0);
            let b = linearize(c.b() as f32 / 255.0);
            0.2126 * r + 0.7152 * g + 0.0722 * b
        }

        let l1 = luminance(fg);
        let l2 = luminance(bg);
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn wcag_rating(ratio: f32) -> (&'static str, Style) {
        if ratio >= 7.0 {
            ("AAA", Style::new().fg(theme::accent::SUCCESS))
        } else if ratio >= 4.5 {
            ("AA", Style::new().fg(theme::accent::INFO))
        } else if ratio >= 3.0 {
            ("AA Large", Style::new().fg(theme::accent::WARNING))
        } else {
            ("Fail", Style::new().fg(theme::accent::ERROR))
        }
    }

    fn render_overview(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Accessibility Control Panel ")
            .title_alignment(Alignment::Center)
            .style(
                Style::new()
                    .fg(theme::screen_accent::ADVANCED)
                    .bg(theme::bg::DEEP),
            );
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let active_theme = theme::current_theme_name();
        let base_theme = self.base_theme.name();
        let contrast_label = if self.a11y.high_contrast {
            "High Contrast"
        } else {
            "Standard"
        };
        let motion_label = if self.a11y.reduced_motion {
            "Reduced (0.0x)"
        } else {
            "Full (1.0x)"
        };

        let mut lines = Vec::new();
        lines.push(Line::from_spans([
            Span::styled("Active Theme: ", theme::muted()),
            Span::styled(active_theme, theme::title()),
        ]));
        lines.push(Line::from_spans([
            Span::styled("Base Theme: ", theme::muted()),
            Span::styled(base_theme, theme::body()),
            Span::styled("  Mode: ", theme::muted()),
            Span::styled(
                contrast_label,
                if self.a11y.high_contrast {
                    theme::success()
                } else {
                    theme::muted()
                },
            ),
        ]));
        lines.push(Line::from_spans([
            Span::styled("Motion: ", theme::muted()),
            Span::styled(motion_label, theme::body()),
            Span::styled("  Large Text: ", theme::muted()),
            Span::styled(
                if self.a11y.large_text { "ON" } else { "OFF" },
                if self.a11y.large_text {
                    theme::success()
                } else {
                    theme::muted()
                },
            ),
        ]));
        lines.push(Line::from_spans([Span::styled(
            "Shortcuts: h = contrast, m = motion, l = large text",
            theme::muted(),
        )]));

        Paragraph::new(Text::from_lines(lines)).render(inner, frame);
    }

    fn render_toggles(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Toggles ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let key_style =
            theme::apply_large_text(Style::new().fg(theme::accent::INFO).attrs(StyleFlags::BOLD));
        let label_style = theme::body();
        let on_style = theme::apply_large_text(
            Style::new()
                .fg(theme::accent::SUCCESS)
                .attrs(StyleFlags::BOLD),
        );
        let off_style = theme::apply_large_text(Style::new().fg(theme::fg::MUTED));

        let toggle_line = |key: &str, label: &str, enabled: bool| {
            let value = if enabled { "ON" } else { "OFF" };
            let value_style = if enabled { on_style } else { off_style };
            Line::from_spans([
                Span::styled(format!(" [{key}] "), key_style),
                Span::styled(label, label_style),
                Span::styled(": ", label_style),
                Span::styled(value, value_style),
            ])
        };

        let lines = vec![
            toggle_line("h", "High Contrast", self.a11y.high_contrast),
            toggle_line("m", "Reduced Motion", self.a11y.reduced_motion),
            toggle_line("l", "Large Text", self.a11y.large_text),
            Line::from_spans([Span::styled(
                "Shift+A opens the compact overlay",
                theme::muted(),
            )]),
        ];

        Paragraph::new(Text::from_lines(lines)).render(inner, frame);
    }

    fn render_wcag(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" WCAG Contrast ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let palette = theme::palette(theme::current_theme());
        let checks = [
            ("Primary on Base", palette.fg_primary, palette.bg_base),
            ("Secondary on Base", palette.fg_secondary, palette.bg_base),
            ("Accent Primary", palette.accent_primary, palette.bg_base),
            ("Accent Warning", palette.accent_warning, palette.bg_base),
            ("Accent Error", palette.accent_error, palette.bg_base),
        ];

        let mut min_ratio = f32::MAX;
        let mut lines = Vec::new();
        for (label, fg, bg) in checks {
            let ratio = Self::contrast_ratio(fg, bg);
            if ratio < min_ratio {
                min_ratio = ratio;
            }
            let (rating, rating_style) = Self::wcag_rating(ratio);
            let ratio_text = format!("{ratio:>4.1}:1");
            lines.push(Line::from_spans([
                Span::styled(format!("{label:<18} "), theme::body()),
                Span::styled(ratio_text, theme::code()),
                Span::styled(" ", theme::muted()),
                Span::styled(rating, rating_style),
            ]));
        }

        let (min_rating, min_style) = Self::wcag_rating(min_ratio);
        lines.push(Line::from(""));
        lines.push(Line::from_spans([
            Span::styled("Minimum ratio: ", theme::muted()),
            Span::styled(format!("{min_ratio:.1}:1"), theme::code()),
            Span::styled(" ", theme::muted()),
            Span::styled(min_rating, min_style),
        ]));
        lines.push(Line::from_spans([Span::styled(
            "AA >= 4.5, AAA >= 7.0, Large Text >= 3.0",
            theme::muted(),
        )]));

        Paragraph::new(Text::from_lines(lines)).render(inner, frame);
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Live Preview ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let motion_label = if self.a11y.reduced_motion {
            "Animations paused"
        } else {
            "Animations active"
        };

        let lines = vec![
            Line::from_spans([Span::styled("Preview text", theme::title())]),
            Line::from_spans([Span::styled(
                "The quick brown fox jumps over the lazy dog.",
                theme::body(),
            )]),
            Line::from_spans([
                Span::styled("Links look like ", theme::body()),
                Span::styled("this", theme::link()),
                Span::styled(" and code looks like ", theme::body()),
                Span::styled("fn main()", theme::code()),
            ]),
            Line::from_spans([
                Span::styled("Status: ", theme::body()),
                Span::styled("OK", theme::success()),
                Span::styled("  ", theme::muted()),
                Span::styled("Error", theme::error_style()),
            ]),
            Line::from_spans([Span::styled(motion_label, theme::muted())]),
        ];

        Paragraph::new(Text::from_lines(lines)).render(inner, frame);
    }

    fn render_telemetry(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" A11y Telemetry ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::screen_accent::ADVANCED));
        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines = Vec::new();
        if self.events.is_empty() {
            lines.push(Line::from_spans([Span::styled(
                "No a11y events yet. Toggle a mode to emit telemetry.",
                theme::muted(),
            )]));
        } else {
            for entry in self.events.iter().rev() {
                let label = match entry.kind {
                    A11yEventKind::Panel => "Panel",
                    A11yEventKind::HighContrast => "High Contrast",
                    A11yEventKind::ReducedMotion => "Reduced Motion",
                    A11yEventKind::LargeText => "Large Text",
                };
                let state = format!(
                    "HC:{} RM:{} LT:{}",
                    if entry.high_contrast { "ON" } else { "OFF" },
                    if entry.reduced_motion { "ON" } else { "OFF" },
                    if entry.large_text { "ON" } else { "OFF" }
                );
                lines.push(Line::from_spans([
                    Span::styled(format!("[{:>4}] ", entry.tick), theme::muted()),
                    Span::styled(label, theme::body()),
                    Span::styled(" · ", theme::muted()),
                    Span::styled(state, theme::code()),
                ]));
            }
        }

        Paragraph::new(Text::from_lines(lines)).render(inner, frame);
    }
}

/// Toggle action that the app dispatches (accessibility events are app-level).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A11yToggleAction {
    HighContrast,
    ReducedMotion,
    LargeText,
}

impl AccessibilityPanel {
    /// Handle a mouse event. Returns an optional toggle action for the app to dispatch.
    pub fn handle_mouse(&self, kind: MouseEventKind, x: u16, y: u16) -> Option<A11yToggleAction> {
        let toggles = self.layout_toggles.get();
        if toggles.contains(x, y)
            && let MouseEventKind::Down(MouseButton::Left) = kind
        {
            // Map row offset to toggle action
            let row = y.saturating_sub(toggles.y);
            match row {
                0 => return Some(A11yToggleAction::HighContrast),
                1 => return Some(A11yToggleAction::ReducedMotion),
                2 => return Some(A11yToggleAction::LargeText),
                _ => {}
            }
        }
        None
    }
}

impl Screen for AccessibilityPanel {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(me) = event {
            // Mouse events are checked via handle_mouse() at the app level
            let _ = self.handle_mouse(me.kind, me.x, me.y);
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(7), Constraint::Min(1)])
            .split(area);

        self.render_overview(frame, rows[0]);

        let (left_area, right_area) = if rows[1].width >= 90 {
            let cols = Flex::horizontal()
                .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
                .split(rows[1]);
            (cols[0], cols[1])
        } else {
            let stack = Flex::vertical()
                .constraints([Constraint::Percentage(52.0), Constraint::Percentage(48.0)])
                .split(rows[1]);
            (stack[0], stack[1])
        };

        let left_rows = Flex::vertical()
            .constraints([Constraint::Fixed(8), Constraint::Min(1)])
            .split(left_area);
        self.layout_toggles.set(left_rows[0]);
        self.render_toggles(frame, left_rows[0]);
        self.render_preview(frame, left_rows[1]);

        let right_rows = Flex::vertical()
            .constraints([Constraint::Fixed(10), Constraint::Min(1)])
            .split(right_area);
        self.layout_wcag.set(right_rows[0]);
        self.render_wcag(frame, right_rows[0]);
        self.render_telemetry(frame, right_rows[1]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "h",
                action: "Toggle high contrast",
            },
            HelpEntry {
                key: "m",
                action: "Toggle reduced motion",
            },
            HelpEntry {
                key: "l",
                action: "Toggle large text",
            },
            HelpEntry {
                key: "Shift+A",
                action: "Toggle A11y overlay",
            },
            HelpEntry {
                key: "Ctrl+T",
                action: "Cycle base theme",
            },
            HelpEntry {
                key: "Click",
                action: "Toggle setting",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Accessibility"
    }

    fn tab_label(&self) -> &'static str {
        "A11y"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{MouseButton, MouseEventKind};

    #[test]
    fn click_toggles_high_contrast() {
        let panel = AccessibilityPanel::new();
        panel.layout_toggles.set(Rect::new(0, 0, 40, 5));
        let action = panel.handle_mouse(MouseEventKind::Down(MouseButton::Left), 10, 0);
        assert_eq!(action, Some(A11yToggleAction::HighContrast));
    }

    #[test]
    fn click_toggles_reduced_motion() {
        let panel = AccessibilityPanel::new();
        panel.layout_toggles.set(Rect::new(0, 0, 40, 5));
        let action = panel.handle_mouse(MouseEventKind::Down(MouseButton::Left), 10, 1);
        assert_eq!(action, Some(A11yToggleAction::ReducedMotion));
    }

    #[test]
    fn click_toggles_large_text() {
        let panel = AccessibilityPanel::new();
        panel.layout_toggles.set(Rect::new(0, 0, 40, 5));
        let action = panel.handle_mouse(MouseEventKind::Down(MouseButton::Left), 10, 2);
        assert_eq!(action, Some(A11yToggleAction::LargeText));
    }

    #[test]
    fn click_outside_toggles_returns_none() {
        let panel = AccessibilityPanel::new();
        panel.layout_toggles.set(Rect::new(0, 0, 40, 5));
        let action = panel.handle_mouse(MouseEventKind::Down(MouseButton::Left), 50, 0);
        assert_eq!(action, None);
    }

    #[test]
    fn mouse_move_ignored() {
        let panel = AccessibilityPanel::new();
        panel.layout_toggles.set(Rect::new(0, 0, 40, 5));
        let action = panel.handle_mouse(MouseEventKind::Moved, 10, 0);
        assert_eq!(action, None);
    }

    #[test]
    fn keybindings_include_click() {
        let panel = AccessibilityPanel::new();
        let bindings = panel.keybindings();
        assert!(bindings.iter().any(|b| b.key == "Click"));
    }
}
