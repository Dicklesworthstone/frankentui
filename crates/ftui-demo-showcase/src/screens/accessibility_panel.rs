#![forbid(unsafe_code)]

//! Accessibility Control Panel — demonstrates a11y modes and contrast checks.
//!
//! bd-iuvb.8

use std::collections::VecDeque;

use std::cell::Cell;

use ftui_core::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_runtime::{AccessibilityFrame, Cmd};
use ftui_style::{Style, StyleFlags};
use ftui_text::{Line, Span, Text, WrapMode};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::app::{A11yEventKind, A11yTelemetryEvent};
use crate::theme;

const MAX_EVENTS: usize = 6;

/// Width the toggles/preview column needs for its widest line, plus its border.
///
/// `render_preview` draws "Links look like this and code looks like fn main()"
/// at 50 cells; nothing the toggles draw is wider (the `Shift+A` hint is 33).
const TOGGLES_COLUMN_WIDTH: u16 = 52;

/// Width the WCAG/telemetry column needs for its widest line, plus its border.
///
/// `render_telemetry`'s empty state, "No a11y events yet. Toggle a mode to emit
/// telemetry.", is 52 cells and outgrows the WCAG legend's 40.
const TELEMETRY_COLUMN_WIDTH: u16 = 54;

/// Width below which the tree column stops being worth a column of its own.
///
/// Tree dumps truncate by design, but the disabled notice contains the 35-cell
/// word "(ProgramConfig::with_accessibility)", which word wrapping cannot break.
const TREE_COLUMN_MIN_WIDTH: u16 = 37;

/// Three columns fit only once every column can show its content whole.
const THREE_COLUMN_MIN_WIDTH: u16 =
    TOGGLES_COLUMN_WIDTH + TREE_COLUMN_MIN_WIDTH + TELEMETRY_COLUMN_WIDTH;

/// Two columns fit once the two fixed-width columns do; the tree moves into the
/// left column underneath the preview.
const TWO_COLUMN_MIN_WIDTH: u16 = TOGGLES_COLUMN_WIDTH + TELEMETRY_COLUMN_WIDTH;

/// Rows the toggles block needs: four lines plus its border.
const TOGGLES_HEIGHT: u16 = 6;

/// Rows the preview block needs: five lines plus its border.
const PREVIEW_HEIGHT: u16 = 7;

/// Rows the WCAG block needs: eight lines plus its border.
const WCAG_HEIGHT: u16 = 10;

/// Rows below which a tree block shows too little of the dump to be worth it:
/// its `nodes=/focused=` header, two dump lines, and the border.
const TREE_MIN_HEIGHT: u16 = 5;

/// Rows the telemetry block takes in the stacked layout: two entries plus its
/// border.
///
/// One row less than it used to take, which is the row the toggles block needs
/// to stop cutting its last line; the tree keeps the rest either way.
const STACKED_TELEMETRY_HEIGHT: u16 = 4;

#[derive(Clone, Copy)]
struct A11yEventEntry {
    kind: A11yEventKind,
    tick: u64,
    high_contrast: bool,
    reduced_motion: bool,
    large_text: bool,
}

/// Most recent screen-reader announcements kept for display.
const MAX_ANNOUNCEMENTS: usize = 4;

#[derive(Clone)]
struct AnnouncementEntry {
    frame: u64,
    urgency: String,
    text: String,
}

/// Accessibility control panel screen.
pub struct AccessibilityPanel {
    a11y: theme::A11ySettings,
    base_theme: theme::ThemeId,
    events: VecDeque<A11yEventEntry>,
    /// Size of the runtime's last accessibility tree (0 until the runtime
    /// delivered one via `Model::on_accessibility`).
    tree_nodes: usize,
    /// Frame index the tree summary refers to.
    tree_frame: u64,
    /// Reading-order dump, paired with node IDs for focus highlighting.
    tree_lines: Vec<(u64, String)>,
    tree_received: bool,
    tree_focused: Option<u64>,
    tree_scroll: Cell<usize>,
    layout_tree: Cell<Rect>,
    /// Latest screen-reader announcements delivered by the runtime.
    announcements: VecDeque<AnnouncementEntry>,
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
            tree_nodes: 0,
            tree_frame: 0,
            tree_lines: Vec::new(),
            tree_received: false,
            tree_focused: None,
            tree_scroll: Cell::new(0),
            layout_tree: Cell::new(Rect::default()),
            announcements: VecDeque::with_capacity(MAX_ANNOUNCEMENTS),
            layout_toggles: Cell::new(Rect::default()),
            layout_wcag: Cell::new(Rect::default()),
        }
    }

    /// Mirror the runtime's accessibility tree for the frame it was built
    /// from (delivered through `Model::on_accessibility` whenever the tree
    /// changed): keep the node count and the latest announcements.
    pub fn record_accessibility(&mut self, a11y: &AccessibilityFrame<'_>) {
        self.tree_nodes = a11y.tree.node_count();
        self.tree_frame = a11y.frame_idx;
        self.tree_received = true;
        self.tree_focused = a11y.tree.focused_id();
        self.tree_lines = a11y
            .order
            .iter()
            .flat_map(|&id| {
                a11y.tree
                    .dump_text(&[id])
                    .lines()
                    .map(|line| (id, line.to_owned()))
                    .collect::<Vec<_>>()
            })
            .collect();
        self.clamp_tree_scroll();
        tracing::debug!(
            target: crate::app::TARGET_ACCESSIBILITY_PANEL,
            nodes = self.tree_nodes,
            focused = ?self.tree_focused,
            frame = self.tree_frame,
            "accessibility tree received"
        );
        for announcement in a11y.announcements {
            if self.announcements.len() == MAX_ANNOUNCEMENTS {
                self.announcements.pop_front();
            }
            self.announcements.push_back(AnnouncementEntry {
                frame: a11y.frame_idx,
                urgency: announcement.urgency.to_string(),
                text: announcement.text.clone(),
            });
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
        self.layout_toggles.set(inner);
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
                Span::styled(label.to_string(), label_style),
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
        for entry in self.announcements.iter().rev() {
            lines.push(Line::from_spans([
                Span::styled(format!("[{:>4}] ", entry.frame), theme::muted()),
                Span::styled(format!("SR {} ", entry.urgency), theme::code()),
                Span::styled(entry.text.clone(), theme::body()),
            ]));
        }
        if self.events.is_empty() {
            const PLACEHOLDER: &str = "No a11y events yet. Toggle a mode to emit telemetry.";
            if inner.width >= PLACEHOLDER.len() as u16 || inner.height < 2 {
                lines.push(Line::from_spans([Span::styled(
                    PLACEHOLDER,
                    theme::muted(),
                )]));
            } else {
                lines.push(Line::from_spans([Span::styled(
                    "No a11y events yet.",
                    theme::muted(),
                )]));
                lines.push(Line::from_spans([Span::styled(
                    "Toggle a mode to emit telemetry.",
                    theme::muted(),
                )]));
            }
        } else {
            for entry in self.events.iter().rev() {
                let (label, state) = if inner.width >= 44 {
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
                    (label, state)
                } else {
                    let label = match entry.kind {
                        A11yEventKind::Panel => "Panel",
                        A11yEventKind::HighContrast => "HiContrast",
                        A11yEventKind::ReducedMotion => "Red Motion",
                        A11yEventKind::LargeText => "Large Text",
                    };
                    let state = format!(
                        "H:{} M:{} L:{}",
                        if entry.high_contrast { "ON" } else { "OFF" },
                        if entry.reduced_motion { "ON" } else { "OFF" },
                        if entry.large_text { "ON" } else { "OFF" }
                    );
                    (label, state)
                };
                lines.push(Line::from_spans([
                    Span::styled(format!("[{:>4}] ", entry.tick), theme::muted()),
                    Span::styled(label, theme::body()),
                    Span::styled(" · ", theme::muted()),
                    Span::styled(state, theme::code()),
                ]));
            }
        }

        Paragraph::new(Text::from_lines(lines))
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    fn clamp_tree_scroll(&self) {
        let limit = self
            .tree_lines
            .len()
            .saturating_sub(usize::from(self.layout_tree.get().height));
        self.tree_scroll.set(self.tree_scroll.get().min(limit));
    }

    fn render_tree(&self, frame: &mut Frame, area: Rect) {
        self.layout_tree.set(Rect::default());
        if area.is_empty() {
            return;
        }
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Tree · previous frame ")
            .style(Style::new().fg(theme::screen_accent::ADVANCED));
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }
        if !self.tree_received {
            Paragraph::new("accessibility tree disabled (ProgramConfig::with_accessibility)")
                .style(theme::muted())
                .wrap(WrapMode::Word)
                .render(inner, frame);
            return;
        }
        let focused = self
            .tree_focused
            .map_or_else(|| "-".to_owned(), |id| id.to_string());
        Paragraph::new(format!("nodes={} focused={focused}", self.tree_nodes))
            .style(theme::muted())
            .render(Rect::new(inner.x, inner.y, inner.width, 1), frame);
        let body = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        self.layout_tree.set(body);
        self.clamp_tree_scroll();
        let lines = self
            .tree_lines
            .iter()
            .skip(self.tree_scroll.get())
            .take(usize::from(body.height))
            .map(|(id, text)| {
                let style = if Some(*id) == self.tree_focused {
                    theme::body().attrs(StyleFlags::BOLD | StyleFlags::REVERSE)
                } else {
                    theme::code()
                };
                Line::from_spans([Span::styled(text.clone(), style)])
            })
            .collect::<Vec<_>>();
        let paragraph = Paragraph::new(Text::from_lines(lines));
        paragraph.render(body, frame);
        if let Some(builder) = frame.a11y.as_deref_mut()
            && let Some(metadata) =
                ftui_a11y::Accessible::accessibility_nodes(&paragraph, body).first()
            && let Some(node) = builder.node_mut(metadata.id)
        {
            // Keep the displayed text available to screen readers as a
            // description, without recursively quoting this inspector's own
            // previous dump in the next frame's node name.
            node.description = node.name.take();
            node.name = Some("Accessibility tree nodes".to_owned());
        }
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
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
        {
            let page = usize::from(self.layout_tree.get().height).max(1);
            let offset = self.tree_scroll.get();
            self.tree_scroll.set(match key.code {
                KeyCode::Up => offset.saturating_sub(1),
                KeyCode::Down => offset.saturating_add(1),
                KeyCode::PageUp => offset.saturating_sub(page),
                KeyCode::PageDown => offset.saturating_add(page),
                KeyCode::Home => 0,
                KeyCode::End => self.tree_lines.len(),
                _ => offset,
            });
            self.clamp_tree_scroll();
        }
        if let Event::Mouse(me) = event {
            if self.layout_tree.get().contains(me.x, me.y) {
                let offset = self.tree_scroll.get();
                self.tree_scroll.set(match me.kind {
                    MouseEventKind::ScrollUp => offset.saturating_sub(3),
                    MouseEventKind::ScrollDown => offset.saturating_add(3),
                    _ => offset,
                });
                self.clamp_tree_scroll();
            }
            // Mouse events are checked via handle_mouse() at the app level
            let _ = self.handle_mouse(me.kind, me.x, me.y);
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        self.layout_toggles.set(Rect::default());
        self.layout_wcag.set(Rect::default());
        self.layout_tree.set(Rect::default());
        if area.is_empty() {
            return;
        }

        let overview_height = if area.height >= 24 { 7 } else { 3 };
        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(overview_height), Constraint::Min(1)])
            .split(area);

        self.render_overview(frame, rows[0]);

        // The side columns carry lines that must not be cut, so they take the
        // width their content needs and the tree absorbs whatever is left. The
        // tree only earns a column of its own once one is wide enough to be
        // readable; below that it stacks under the preview, and below that
        // again everything stacks full width.
        if rows[1].width >= THREE_COLUMN_MIN_WIDTH && rows[1].height >= 8 {
            let cols = Flex::horizontal()
                .constraints([
                    Constraint::Fixed(TOGGLES_COLUMN_WIDTH),
                    Constraint::Fill,
                    Constraint::Fixed(TELEMETRY_COLUMN_WIDTH),
                ])
                .split(rows[1]);
            let left_rows = Flex::vertical()
                .constraints([Constraint::Fixed(TOGGLES_HEIGHT), Constraint::Min(1)])
                .split(cols[0]);
            self.layout_toggles.set(left_rows[0]);
            self.render_toggles(frame, left_rows[0]);
            self.render_preview(frame, left_rows[1]);
            self.render_tree(frame, cols[1]);

            let right_rows = Flex::vertical()
                .constraints([Constraint::Fixed(WCAG_HEIGHT), Constraint::Min(1)])
                .split(cols[2]);
            self.layout_wcag.set(right_rows[0]);
            self.render_wcag(frame, right_rows[0]);
            self.render_telemetry(frame, right_rows[1]);
        } else if rows[1].width >= TWO_COLUMN_MIN_WIDTH && rows[1].height >= 8 {
            let cols = Flex::horizontal()
                .constraints([Constraint::Fixed(TOGGLES_COLUMN_WIDTH), Constraint::Fill])
                .split(rows[1]);
            let left_rows = Flex::vertical()
                .constraints([
                    Constraint::Fixed(TOGGLES_HEIGHT),
                    Constraint::Fixed(PREVIEW_HEIGHT),
                    Constraint::Min(1),
                ])
                .split(cols[0]);
            self.layout_toggles.set(left_rows[0]);
            self.render_toggles(frame, left_rows[0]);
            self.render_preview(frame, left_rows[1]);
            self.render_tree(frame, left_rows[2]);

            let right_rows = Flex::vertical()
                .constraints([Constraint::Fixed(WCAG_HEIGHT), Constraint::Min(1)])
                .split(cols[1]);
            self.layout_wcag.set(right_rows[0]);
            self.render_wcag(frame, right_rows[0]);
            self.render_telemetry(frame, right_rows[1]);
        } else {
            // Stacked at full width, so no line has to be cut and the tree
            // gets every row the other two do not need. The toggles block
            // takes TOGGLES_HEIGHT rather than the 5 rows it used to get,
            // which cut the "Shift+A opens the compact overlay" hint off the
            // bottom at exactly the sizes most terminals open at.
            let telemetry_height = if rows[1].height >= 16 {
                STACKED_TELEMETRY_HEIGHT
            } else {
                0
            };
            let stack = Flex::vertical()
                .constraints([
                    Constraint::Fixed(TOGGLES_HEIGHT),
                    Constraint::Min(1),
                    Constraint::Fixed(telemetry_height),
                ])
                .split(rows[1]);
            self.layout_toggles.set(stack[0]);
            self.render_toggles(frame, stack[0]);
            self.render_tree(frame, stack[1]);
            self.render_telemetry(frame, stack[2]);
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "↑/↓ PgUp/PgDn",
                action: "Scroll accessibility tree",
            },
            HelpEntry {
                key: "Home/End",
                action: "First/last tree node",
            },
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
    use ftui_a11y::node::{A11yNodeInfo, A11yRole};
    use ftui_a11y::tree::A11yTreeBuilder;
    use ftui_core::event::{KeyEvent, MouseButton, MouseEventKind};
    use ftui_render::grapheme_pool::GraphemePool;

    fn panel_with_tree(count: u64, focused: Option<u64>) -> AccessibilityPanel {
        let mut builder = A11yTreeBuilder::new();
        for id in 0..count {
            let mut node = A11yNodeInfo::new(id, A11yRole::Button, Rect::new(0, 0, 4, 1));
            node.name = Some(format!("Node {id}"));
            builder.add_node(node);
        }
        builder.set_focused(focused);
        let tree = builder.build();
        let order: Vec<_> = (0..count).collect();
        let mut panel = AccessibilityPanel::new();
        panel.record_accessibility(&AccessibilityFrame {
            frame_idx: 7,
            tree: &tree,
            order: &order,
            announcements: &[],
            dropped: 0,
        });
        panel
    }

    #[test]
    fn tree_column_scrolls_and_clamps() {
        let mut panel = panel_with_tree(30, Some(25));
        assert_eq!(panel.tree_lines.len(), 30, "do not truncate the live tree");
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 13, &mut pool);
        panel.render_tree(&mut frame, Rect::new(0, 0, 60, 13));
        assert_eq!(panel.layout_tree.get().height, 10);
        for _ in 0..3 {
            panel.update(&Event::Key(KeyEvent::new(KeyCode::PageDown)));
        }
        assert_eq!(panel.tree_scroll.get(), 20);
        panel.render_tree(&mut frame, Rect::new(0, 0, 60, 13));
        let text = ftui_harness::buffer_to_text(&frame.buffer);
        assert!(text.contains("nodes=30 focused=25"));
        assert!(text.contains("Node 29"));
        assert!(!text.contains("Node 0\""));
        assert!(
            frame
                .buffer
                .get(1, 7)
                .unwrap()
                .attrs
                .has_flag(ftui_render::cell::StyleFlags::REVERSE)
        );
        panel.update(&Event::Key(KeyEvent::new(KeyCode::Home)));
        assert_eq!(panel.tree_scroll.get(), 0);
        panel.update(&Event::Key(
            KeyEvent::new(KeyCode::PageDown).with_kind(KeyEventKind::Release),
        ));
        assert_eq!(panel.tree_scroll.get(), 0);
    }

    #[test]
    fn disabled_tree_shows_hint_and_empty_tree_is_distinct() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 8, &mut pool);
        AccessibilityPanel::new().render_tree(&mut frame, Rect::new(0, 0, 80, 8));
        assert!(
            ftui_harness::buffer_to_text(&frame.buffer).contains("accessibility tree disabled")
        );
        let mut empty_frame = Frame::new(80, 8, &mut pool);
        panel_with_tree(0, None).render_tree(&mut empty_frame, Rect::new(0, 0, 80, 8));
        assert!(ftui_harness::buffer_to_text(&empty_frame.buffer).contains("nodes=0 focused=-"));
    }

    #[test]
    fn tree_resize_clamps_scroll_and_keeps_toggles_clickable() {
        let mut panel = panel_with_tree(30, None);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        panel.view(&mut frame, Rect::new(0, 0, 80, 24));
        let toggles = panel.layout_toggles.get();
        assert_eq!(
            panel.handle_mouse(
                MouseEventKind::Down(MouseButton::Left),
                toggles.x,
                toggles.y
            ),
            Some(A11yToggleAction::HighContrast)
        );
        assert_eq!(
            panel.handle_mouse(
                MouseEventKind::Down(MouseButton::Left),
                toggles.x,
                toggles.y - 1,
            ),
            None,
            "the border must not activate a toggle"
        );
        panel.update(&Event::Key(KeyEvent::new(KeyCode::End)));
        assert!(panel.tree_scroll.get() > 0);
        let mut taller = Frame::new(80, 50, &mut pool);
        panel.view(&mut taller, Rect::new(0, 0, 80, 50));
        assert_eq!(panel.tree_scroll.get(), 0, "all nodes now fit");
        for (width, height) in [(40, 10), (1, 1), (0, 0)] {
            let mut small = Frame::new(width, height, &mut pool);
            panel.view(&mut small, Rect::new(0, 0, width, height));
        }
        assert!(panel.layout_toggles.get().is_empty());
        assert!(panel.layout_tree.get().is_empty());
    }

    #[test]
    fn tree_inspector_keeps_readable_details_out_of_its_own_node_name() {
        let panel = panel_with_tree(3, Some(1));
        let mut builder = A11yTreeBuilder::new();
        let mut pool = GraphemePool::new();
        let order = {
            let mut frame = Frame::new(80, 24, &mut pool);
            frame.set_a11y(&mut builder);
            panel.view(&mut frame, Rect::new(0, 0, 80, 24));
            frame.finish_a11y();
            frame.take_a11y_order()
        };
        let tree = builder.build();
        let node = order
            .iter()
            .filter_map(|id| tree.node(*id))
            .find(|node| node.name.as_deref() == Some("Accessibility tree nodes"))
            .expect("the tree inspector must itself be accessible");
        assert!(node.description.as_deref().unwrap().contains("Node 0"));
        assert!(!tree.dump_text(&order).contains("Button \"Node 0\""));
    }

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

    #[test]
    fn telemetry_placeholder_not_cut_at_narrow_widths() {
        let panel = AccessibilityPanel::new();
        let mut pool = GraphemePool::new();
        for width in 40..=80 {
            let mut frame = Frame::new(width, 24, &mut pool);
            panel.view(&mut frame, Rect::new(0, 0, width, 24));
            let text = ftui_harness::buffer_to_text(&frame.buffer);
            assert!(
                text.contains("No a11y events yet."),
                "missing placeholder header at width {width}"
            );
            assert!(
                text.contains("Toggle a mode to emit telemetry."),
                "missing placeholder action at width {width}"
            );
            for line in text.lines() {
                assert!(
                    !line.contains("emit te│") && !line.contains("emit telemetry│"),
                    "line clipped at border at width {width}: {line}"
                );
            }
        }
    }

    #[test]
    fn telemetry_events_not_cut_at_narrow_widths() {
        let mut panel = AccessibilityPanel::new();
        panel.record_event(&A11yTelemetryEvent {
            kind: A11yEventKind::ReducedMotion,
            tick: 42,
            screen: "AccessibilityPanel",
            panel_visible: true,
            high_contrast: false,
            reduced_motion: true,
            large_text: false,
        });
        let mut pool = GraphemePool::new();
        for width in 40..=80 {
            let mut frame = Frame::new(width, 24, &mut pool);
            panel.view(&mut frame, Rect::new(0, 0, width, 24));
            let text = ftui_harness::buffer_to_text(&frame.buffer);
            assert!(
                text.contains("42"),
                "telemetry tick missing at width {width}"
            );
            if width < 46 {
                assert!(
                    text.contains("Red Motion"),
                    "compact label missing at width {width}"
                );
                assert!(
                    text.contains("M:ON"),
                    "compact state missing at width {width}"
                );
            } else {
                assert!(
                    text.contains("Reduced Motion"),
                    "full label missing at width {width}"
                );
                assert!(
                    text.contains("RM:ON"),
                    "full state missing at width {width}"
                );
            }
            for line in text.lines() {
                assert!(
                    !line.contains("Reduced Mo│") && !line.contains("RM:O│"),
                    "event line clipped at border at width {width}: {line}"
                );
            }
        }
    }
}
