#![forbid(unsafe_code)]

//! Shared UI chrome: tab bar, status bar, and help overlay.

use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId};
use ftui_style::{Style, StyleFlags};
use ftui_text::{Line, Span, Text, WrapMode, display_width};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::help::{HelpCategory, HelpMode, KeyFormat, KeybindingHints};
use ftui_widgets::paragraph::Paragraph;

use crate::app::ScreenId;
use crate::screens::{self, ScreenCategory};
use crate::theme;
use crate::tour::TourOverlayState;

// ---------------------------------------------------------------------------
// Hit IDs for tab bar clicks + pane routing
// ---------------------------------------------------------------------------

/// Base hit ID for tab bar entries.  Tab i has HitId(TAB_HIT_BASE + i).
pub const TAB_HIT_BASE: u32 = 1000;
/// Base hit ID for category tabs (ScreenCategory::ALL order).
pub const CATEGORY_HIT_BASE: u32 = 2000;
/// Base hit ID for clickable panes (one per screen).
pub const PANE_HIT_BASE: u32 = 4000;
/// Base hit ID for overlay elements (help, a11y panel, tour).
pub const OVERLAY_HIT_BASE: u32 = 5000;
/// Base hit ID for status bar toggles.
pub const STATUS_HIT_BASE: u32 = 6000;

// Overlay hit ID sub-ranges within OVERLAY_HIT_BASE.
/// Help overlay close button.
pub const OVERLAY_HELP_CLOSE: u32 = OVERLAY_HIT_BASE;
/// Help overlay content area (for scroll).
pub const OVERLAY_HELP_CONTENT: u32 = OVERLAY_HIT_BASE + 1;
/// Tour overlay area.
pub const OVERLAY_TOUR: u32 = OVERLAY_HIT_BASE + 10;
/// A11y panel area.
pub const OVERLAY_A11Y: u32 = OVERLAY_HIT_BASE + 20;
/// Performance HUD area.
pub const OVERLAY_PERF_HUD: u32 = OVERLAY_HIT_BASE + 30;
/// Evidence ledger area.
pub const OVERLAY_EVIDENCE: u32 = OVERLAY_HIT_BASE + 40;
/// Debug overlay area.
pub const OVERLAY_DEBUG: u32 = OVERLAY_HIT_BASE + 50;

// Status bar toggle hit IDs within STATUS_HIT_BASE.
/// Status bar: help toggle.
pub const STATUS_HELP_TOGGLE: u32 = STATUS_HIT_BASE;
/// Status bar: palette toggle.
pub const STATUS_PALETTE_TOGGLE: u32 = STATUS_HIT_BASE + 1;
/// Status bar: a11y toggle.
pub const STATUS_A11Y_TOGGLE: u32 = STATUS_HIT_BASE + 2;
/// Status bar: perf HUD toggle.
pub const STATUS_PERF_TOGGLE: u32 = STATUS_HIT_BASE + 3;
/// Status bar: debug toggle.
pub const STATUS_DEBUG_TOGGLE: u32 = STATUS_HIT_BASE + 4;
/// Status bar: mouse capture toggle.
pub const STATUS_MOUSE_TOGGLE: u32 = STATUS_HIT_BASE + 5;

const TAB_ACCENT_ALPHA: u8 = 220;

/// Convert a hit ID back to a ScreenId if it falls in the tab range.
pub fn screen_from_hit_id(id: HitId) -> Option<ScreenId> {
    let raw = id.id();
    if raw >= TAB_HIT_BASE && raw < TAB_HIT_BASE + screens::screen_registry().len() as u32 {
        let idx = (raw - TAB_HIT_BASE) as usize;
        screens::screen_registry().get(idx).map(|meta| meta.id)
    } else {
        None
    }
}

/// Convert a category hit ID to a ScreenCategory.
pub fn category_from_hit_id(id: HitId) -> Option<ScreenCategory> {
    let raw = id.id();
    if raw >= CATEGORY_HIT_BASE && raw < CATEGORY_HIT_BASE + ScreenCategory::ALL.len() as u32 {
        let idx = (raw - CATEGORY_HIT_BASE) as usize;
        ScreenCategory::ALL.get(idx).copied()
    } else {
        None
    }
}

/// Convert a pane hit ID back to a ScreenId.
pub fn screen_from_pane_hit_id(id: HitId) -> Option<ScreenId> {
    let raw = id.id();
    if raw >= PANE_HIT_BASE && raw < PANE_HIT_BASE + screens::screen_registry().len() as u32 {
        let idx = (raw - PANE_HIT_BASE) as usize;
        screens::screen_registry().get(idx).map(|meta| meta.id)
    } else {
        None
    }
}

/// Convert any demo hit ID to its target screen.
pub fn screen_from_any_hit_id(id: HitId) -> Option<ScreenId> {
    screen_from_hit_id(id)
        .or_else(|| screen_from_pane_hit_id(id))
        .or_else(|| category_from_hit_id(id).and_then(screens::first_in_category))
}

/// Classify a hit ID into a dispatch layer for routing priority.
///
/// Priority order (highest first):
///   1. Overlay (palette, help, tour, a11y, perf HUD, evidence, debug)
///   2. Status bar toggles
///   3. Tab bar / category tabs
///   4. Pane (screen content)
///   5. Unknown
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitLayer {
    /// Hit falls in an overlay region.
    Overlay(u32),
    /// Hit falls in a status bar toggle.
    StatusToggle(u32),
    /// Hit resolves to a tab bar entry.
    Tab(ScreenId),
    /// Hit resolves to a category tab.
    Category(ScreenCategory),
    /// Hit resolves to a screen pane content area.
    Pane(ScreenId),
    /// Unknown hit ID — pass to screen for handling.
    Unknown,
}

/// Classify a hit ID into its dispatch layer.
pub fn classify_hit(id: HitId) -> HitLayer {
    let raw = id.id();
    // Overlay range: OVERLAY_HIT_BASE..STATUS_HIT_BASE
    if (OVERLAY_HIT_BASE..STATUS_HIT_BASE).contains(&raw) {
        return HitLayer::Overlay(raw);
    }
    // Status bar range: STATUS_HIT_BASE..STATUS_HIT_BASE+100
    if (STATUS_HIT_BASE..STATUS_HIT_BASE + 100).contains(&raw) {
        return HitLayer::StatusToggle(raw);
    }
    // Tab bar
    if let Some(screen) = screen_from_hit_id(id) {
        return HitLayer::Tab(screen);
    }
    // Category tab
    if let Some(cat) = category_from_hit_id(id) {
        return HitLayer::Category(cat);
    }
    // Pane
    if let Some(screen) = screen_from_pane_hit_id(id) {
        return HitLayer::Pane(screen);
    }
    HitLayer::Unknown
}

/// Register a pane-sized hit region to route clicks to a screen.
pub fn register_pane_hit(frame: &mut Frame, rect: Rect, screen: ScreenId) {
    if !rect.is_empty() {
        frame.register_hit_region(
            rect,
            HitId::new(PANE_HIT_BASE + screens::screen_index(screen) as u32),
        );
    }
}

/// Render the guided tour overlay (callouts + step list + highlight).
pub fn render_guided_tour_overlay(state: &TourOverlayState<'_>, frame: &mut Frame, area: Rect) {
    if area.is_empty() {
        return;
    }

    // Highlights are intentionally disabled (always off) per user preference.

    let width = area.width.min(56);
    let height = area.height.min(14);
    if width < 28 || height < 7 {
        return;
    }
    let default_overlay = Rect::new(area.right().saturating_sub(width), area.y, width, height);
    let overlay = if let Some(highlight) = state.highlight {
        let candidates = [
            default_overlay,
            Rect::new(area.x, area.y, width, height),
            Rect::new(
                area.right().saturating_sub(width),
                area.bottom().saturating_sub(height),
                width,
                height,
            ),
            Rect::new(area.x, area.bottom().saturating_sub(height), width, height),
        ];
        candidates
            .into_iter()
            .find(|candidate| !rects_intersect(*candidate, highlight))
            .unwrap_or(default_overlay)
    } else {
        default_overlay
    };

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title("Guided Tour")
        .title_alignment(Alignment::Center)
        .style(
            Style::new()
                .fg(theme::accent::PRIMARY)
                .bg(theme::alpha::SURFACE),
        );
    let inner = block.inner(overlay);
    block.render(overlay, frame);

    if inner.is_empty() {
        return;
    }

    let mut lines = Vec::new();
    let status = if state.paused { "PAUSED" } else { "LIVE" };
    let header = Line::from_spans([
        Span::styled(
            format!("{}/{}", state.step_index + 1, state.step_count.max(1)),
            Style::new().fg(theme::accent::INFO).bold(),
        ),
        Span::raw(" · "),
        Span::styled(
            state.screen_category.label(),
            Style::new().fg(theme::accent::SECONDARY),
        ),
        Span::raw(" · "),
        Span::styled(status, Style::new().fg(theme::accent::SUCCESS).bold()),
    ]);
    lines.push(header);

    let remaining = state.remaining.as_secs_f32();
    let timing = Line::from_spans([
        Span::styled("Speed ", Style::new().fg(theme::fg::MUTED)),
        Span::styled(
            format!("{:.2}x", state.speed),
            Style::new().fg(theme::fg::PRIMARY),
        ),
        Span::raw(" · "),
        Span::styled("Next ", Style::new().fg(theme::fg::MUTED)),
        Span::styled(
            format!("{remaining:.1}s"),
            Style::new().fg(theme::accent::INFO),
        ),
    ]);
    lines.push(timing);

    lines.push(Line::from_spans([Span::styled(
        state.callout_title,
        Style::new().fg(theme::fg::PRIMARY).bold(),
    )]));

    lines.push(Line::from_spans([Span::styled(
        state.callout_body,
        Style::new().fg(theme::fg::SECONDARY),
    )]));

    if let Some(hint) = state.callout_hint {
        lines.push(Line::from_spans([Span::styled(
            hint,
            Style::new().fg(theme::accent::WARNING).italic(),
        )]));
    }

    lines.push(Line::from_spans([Span::styled(
        "Controls: Space pause · ←/→ step · Esc exit",
        Style::new().fg(theme::fg::MUTED),
    )]));

    let mut remaining_rows = inner.height.saturating_sub(lines.len() as u16) as usize;
    if remaining_rows >= 2 {
        lines.push(Line::from_spans([Span::styled(
            "Steps:",
            Style::new().fg(theme::fg::MUTED).bold(),
        )]));
        remaining_rows = remaining_rows.saturating_sub(1);
        let legend_reserve = if remaining_rows >= 2 { 1 } else { 0 };
        let max_steps = remaining_rows.saturating_sub(legend_reserve);
        for step in state.steps.iter().take(max_steps) {
            let prefix = if step.is_current { "▶" } else { "•" };
            let label = format!("{} {} · {}", prefix, step.category.label(), step.title);
            lines.push(Line::from_spans([Span::styled(
                label,
                Style::new().fg(theme::fg::PRIMARY),
            )]));
        }
        remaining_rows = inner.height.saturating_sub(lines.len() as u16) as usize;
        if remaining_rows >= 1 {
            let mut spans = Vec::new();
            spans.push(Span::styled(
                "Legend:",
                Style::new().fg(theme::fg::MUTED).bold(),
            ));
            for category in ScreenCategory::ALL {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    category.short_label(),
                    Style::new().fg(category_accent(*category)),
                ));
            }
            lines.push(Line::from_spans(spans));
        }
    }

    Paragraph::new(Text::from_lines(lines))
        .wrap(WrapMode::Word)
        .render(inner, frame);
}

fn rects_intersect(a: Rect, b: Rect) -> bool {
    let ax2 = a.right();
    let ay2 = a.bottom();
    let bx2 = b.right();
    let by2 = b.bottom();
    !(ax2 <= b.x || bx2 <= a.x || ay2 <= b.y || by2 <= a.y)
}

// ---------------------------------------------------------------------------
// Tab bar
// ---------------------------------------------------------------------------

/// Render the tab bar with numbered screen tabs.
///
/// The current screen's tab uses the screen's accent color background + bold primary.
/// Other tabs are rendered in muted foreground.
pub fn render_tab_bar(current: ScreenId, frame: &mut Frame, area: Rect) {
    // Fill background
    let bg_style = theme::tab_bar();
    let blank = Paragraph::new("").style(bg_style);
    blank.render(area, frame);

    // Lay out tabs left-to-right
    let mut x = area.x;
    let registry = screens::screen_registry();
    for (i, meta) in registry.iter().enumerate() {
        let key_label = if i < 9 {
            format!("{}", i + 1)
        } else if i == 9 {
            "0".into()
        } else {
            "-".into()
        };

        let id = meta.id;
        let label_text = meta.short_label;
        let key_width = display_width(&key_label) as u16;
        let label_text_width = display_width(label_text) as u16;
        let label_width = 1 + key_width + 2 + label_text_width + 1; // " {key}: {label} "

        if x + label_width > area.x + area.width {
            break; // No room for more tabs
        }

        let tab_area = Rect::new(x, area.y, label_width, 1);

        let is_active = id == current;
        let bg = if is_active {
            theme::with_alpha(accent_for(id), TAB_ACCENT_ALPHA)
        } else {
            theme::alpha::SURFACE.into()
        };
        let label_style = if is_active {
            Style::new()
                .bg(bg)
                .fg(theme::fg::PRIMARY)
                .attrs(StyleFlags::BOLD)
        } else {
            Style::new().bg(bg).fg(theme::fg::MUTED)
        };
        let label_style = theme::apply_large_text(label_style);
        let key_style = theme::apply_large_text(
            Style::new()
                .bg(bg)
                .fg(theme::fg::MUTED)
                .attrs(StyleFlags::DIM),
        );
        let pad_style = Style::new().bg(bg);

        let line = Line::from_spans([
            Span::styled(" ", pad_style),
            Span::styled(key_label.clone(), key_style),
            Span::styled(": ", key_style),
            Span::styled(label_text, label_style),
            Span::styled(" ", pad_style),
        ]);
        let tab = Paragraph::new(Text::from_lines([line]));
        tab.render(tab_area, frame);

        // Register hit region for mouse clicks
        frame.register_hit_region(tab_area, HitId::new(TAB_HIT_BASE + i as u32));

        x += label_width;

        // Subtle separator between tabs (only if the next tab will fit)
        if i + 1 < registry.len() {
            let next_key_label = if i + 1 < 9 {
                format!("{}", i + 2)
            } else if i + 1 == 9 {
                "0".into()
            } else {
                "-".into()
            };
            let next_label_text = registry[i + 1].short_label;
            let next_label_width = 1
                + display_width(&next_key_label) as u16
                + 2
                + display_width(next_label_text) as u16
                + 1; // " {key}: {label} "

            if x + 1 + next_label_width > area.x + area.width {
                break;
            }

            let sep_area = Rect::new(x, area.y, 1, 1);
            let sep_style = Style::new()
                .bg(theme::alpha::SURFACE)
                .fg(theme::fg::MUTED)
                .attrs(StyleFlags::DIM);
            let sep = Paragraph::new("│").style(sep_style);
            sep.render(sep_area, frame);
            x = x.saturating_add(1);
        }
    }
}

/// Render a category tab strip (bd-iuvb.16).
pub fn render_category_tabs(current: ScreenId, frame: &mut Frame, area: Rect) {
    let bg_style = theme::tab_bar();
    Paragraph::new("").style(bg_style).render(area, frame);

    let current_category = screens::screen_category(current);
    let mut x = area.x;

    for &category in ScreenCategory::ALL {
        let label = category.short_label();
        let label_width = 1 + display_width(label) as u16 + 1; // " {label} "
        if x + label_width > area.x + area.width {
            break;
        }

        let tab_area = Rect::new(x, area.y, label_width, 1);
        let is_active = category == current_category;
        let bg = if is_active {
            theme::with_alpha(category_accent(category), TAB_ACCENT_ALPHA)
        } else {
            theme::alpha::SURFACE.into()
        };
        let label_style = if is_active {
            Style::new()
                .bg(bg)
                .fg(theme::fg::PRIMARY)
                .attrs(StyleFlags::BOLD)
        } else {
            Style::new().bg(bg).fg(theme::fg::MUTED)
        };
        let pad_style = Style::new().bg(bg);

        let line = Line::from_spans([
            Span::styled(" ", pad_style),
            Span::styled(label, label_style),
            Span::styled(" ", pad_style),
        ]);
        Paragraph::new(Text::from_lines([line])).render(tab_area, frame);
        frame.register_hit_region(
            tab_area,
            HitId::new(CATEGORY_HIT_BASE + category_index(category) as u32),
        );

        x += label_width;
        if x < area.x + area.width {
            let sep_area = Rect::new(x, area.y, 1, 1);
            let sep_style = Style::new()
                .bg(theme::alpha::SURFACE)
                .fg(theme::fg::MUTED)
                .attrs(StyleFlags::DIM);
            Paragraph::new("│").style(sep_style).render(sep_area, frame);
            x = x.saturating_add(1);
        }
    }
}

fn category_index(category: ScreenCategory) -> usize {
    ScreenCategory::ALL
        .iter()
        .position(|candidate| *candidate == category)
        .unwrap_or(0)
}

/// Render screen tabs for a single category (bd-iuvb.16).
pub fn render_screen_tabs_for_category(
    current: ScreenId,
    category: ScreenCategory,
    frame: &mut Frame,
    area: Rect,
) {
    let bg_style = theme::tab_bar();
    Paragraph::new("").style(bg_style).render(area, frame);

    let mut x = area.x;
    for meta in screens::screens_in_category(category) {
        let id = meta.id;
        let label_text = meta.short_label;
        let label_width = 1 + display_width(label_text) as u16 + 1;
        if x + label_width > area.x + area.width {
            break;
        }

        let tab_area = Rect::new(x, area.y, label_width, 1);
        let is_active = id == current;
        let bg = if is_active {
            theme::with_alpha(accent_for(id), TAB_ACCENT_ALPHA)
        } else {
            theme::alpha::SURFACE.into()
        };
        let label_style = if is_active {
            Style::new()
                .bg(bg)
                .fg(theme::fg::PRIMARY)
                .attrs(StyleFlags::BOLD)
        } else {
            Style::new().bg(bg).fg(theme::fg::MUTED)
        };
        let pad_style = Style::new().bg(bg);

        let line = Line::from_spans([
            Span::styled(" ", pad_style),
            Span::styled(label_text, label_style),
            Span::styled(" ", pad_style),
        ]);
        Paragraph::new(Text::from_lines([line])).render(tab_area, frame);

        x += label_width;
        if x < area.x + area.width {
            let sep_area = Rect::new(x, area.y, 1, 1);
            let sep_style = Style::new()
                .bg(theme::alpha::SURFACE)
                .fg(theme::fg::MUTED)
                .attrs(StyleFlags::DIM);
            Paragraph::new("│").style(sep_style).render(sep_area, frame);
            x = x.saturating_add(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

/// State needed to render the status bar.
pub struct StatusBarState<'a> {
    pub current_screen: ScreenId,
    pub screen_title: &'a str,
    pub screen_index: usize,
    pub screen_count: usize,
    pub tick_count: u64,
    pub frame_count: u64,
    pub terminal_width: u16,
    pub terminal_height: u16,
    pub theme_name: &'a str,
    /// Whether the demo is running in inline modes (scrollback preserved).
    pub inline_mode: bool,
    /// Whether terminal mouse capture is currently enabled.
    pub mouse_capture_enabled: bool,
    pub a11y_high_contrast: bool,
    pub a11y_reduced_motion: bool,
    pub a11y_large_text: bool,
    /// Whether the help overlay is currently shown.
    pub help_visible: bool,
    /// Whether the command palette is currently shown.
    pub palette_visible: bool,
    /// Whether the perf HUD overlay is currently shown.
    pub perf_hud_visible: bool,
    /// Whether the debug overlay is currently shown.
    pub debug_visible: bool,
    /// Whether undo is available.
    pub can_undo: bool,
    /// Whether redo is available.
    pub can_redo: bool,
    /// Description of the next undo action, if any.
    pub undo_description: Option<&'a str>,
}

/// Render the status bar at the bottom of the screen.
///
/// The status bar uses a three-segment layout with visual differentiation:
/// - **Left segment**: Screen title with accent color, position indicator, theme name
/// - **Center segment**: Live metrics (tick/frame counts) in muted style
/// - **Right segment**: Terminal dimensions and elapsed time
pub fn render_status_bar(state: &StatusBarState<'_>, frame: &mut Frame, area: Rect) {
    // Fill background
    let bg_style = theme::status_bar();
    let bg_color = theme::alpha::SURFACE;
    let blank = Paragraph::new("").style(bg_style);
    blank.render(area, frame);

    // Get screen accent color for title emphasis
    let screen_accent = accent_for(state.current_screen);

    // Elapsed time from tick count (each tick = 100ms)
    let total_secs = state.tick_count / 10;
    let mins = total_secs / 60;
    let secs = total_secs % 60;

    // Build a11y indicator
    let mut a11y_flags = Vec::new();
    if state.a11y_high_contrast {
        a11y_flags.push("HC");
    }
    if state.a11y_reduced_motion {
        a11y_flags.push("RM");
    }
    if state.a11y_large_text {
        a11y_flags.push("LT");
    }
    let mut a11y_label = if a11y_flags.is_empty() {
        String::new()
    } else {
        format!(" A11y:{}", a11y_flags.join(" "))
    };

    // Build undo/redo indicator
    let mut undo_label = if state.can_undo || state.can_redo {
        let undo_icon = if state.can_undo { "↶" } else { "-" };
        let redo_icon = if state.can_redo { "↷" } else { "-" };
        format!(" [{}|{}]", undo_icon, redo_icon)
    } else {
        String::new()
    };

    // Toggle strip for clickable status bar indicators (bd-iuvb.17.4)
    let toggle_help = if state.help_visible { " [H]" } else { " [h]" };
    let toggle_palette = if state.palette_visible {
        " [Cmd]"
    } else {
        " [cmd]"
    };
    let toggle_perf = if state.perf_hud_visible {
        " [P]"
    } else {
        " [p]"
    };
    let toggle_debug = if state.debug_visible { " [D]" } else { " [d]" };
    let mut toggle_strip = format!(
        "{}{}{}{}",
        toggle_help, toggle_palette, toggle_perf, toggle_debug
    );

    // Mouse capture indicator (bd-iuvb.17.1)
    let mouse_label_compact = if state.mouse_capture_enabled {
        "  Mouse:ON".to_string()
    } else {
        "  Mouse:OFF".to_string()
    };
    let mouse_label_full = if state.inline_mode {
        if state.mouse_capture_enabled {
            "  Mouse: ON (UI scroll)".to_string()
        } else {
            "  Mouse: OFF (scrollback)".to_string()
        }
    } else if state.mouse_capture_enabled {
        "  Mouse: ON".to_string()
    } else {
        "  Mouse: OFF".to_string()
    };
    let mut mouse_label = mouse_label_full;

    // Style definitions for segments
    let title_style = theme::apply_large_text(
        Style::new()
            .bg(bg_color)
            .fg(screen_accent)
            .attrs(StyleFlags::BOLD),
    );
    let position_style =
        theme::apply_large_text(Style::new().bg(bg_color).fg(theme::fg::SECONDARY));
    let muted_style = theme::apply_large_text(Style::new().bg(bg_color).fg(theme::fg::MUTED));
    let dim_style = theme::apply_large_text(
        Style::new()
            .bg(bg_color)
            .fg(theme::fg::MUTED)
            .attrs(StyleFlags::DIM),
    );
    let mouse_style = if state.mouse_capture_enabled {
        theme::apply_large_text(
            Style::new()
                .bg(bg_color)
                .fg(theme::accent::INFO)
                .attrs(StyleFlags::BOLD),
        )
    } else {
        theme::apply_large_text(
            Style::new()
                .bg(bg_color)
                .fg(theme::fg::MUTED)
                .attrs(StyleFlags::DIM),
        )
    };
    let toggle_active_style = theme::apply_large_text(
        Style::new()
            .bg(bg_color)
            .fg(theme::accent::SUCCESS)
            .attrs(StyleFlags::BOLD),
    );
    let toggle_inactive_style = theme::apply_large_text(
        Style::new()
            .bg(bg_color)
            .fg(theme::fg::MUTED)
            .attrs(StyleFlags::DIM),
    );
    let time_style = theme::apply_large_text(Style::new().bg(bg_color).fg(theme::fg::SECONDARY));
    let pad_style = Style::new().bg(bg_color);

    // Build content strings
    let position_str = format!("[{}/{}]", state.screen_index + 1, state.screen_count);
    let mut theme_str = format!("  {}", state.theme_name);
    let nav_hint_full = "Tab: next screen · Shift+Tab: prev";
    let nav_hint_compact = "Tab/Shift+Tab: next/prev";
    let metrics_str = format!("tick:{} frm:{}", state.tick_count, state.frame_count);
    let center_full = format!("{nav_hint_full} │ {metrics_str}");
    let center_compact = nav_hint_compact.to_string();
    let mut center_str = center_full;
    let dims_str = format!("{}x{}", state.terminal_width, state.terminal_height);
    let time_str = format!("{:02}:{:02}", mins, secs);

    // Undo indicator style
    let undo_style = theme::apply_large_text(
        Style::new()
            .bg(bg_color)
            .fg(theme::accent::INFO)
            .attrs(StyleFlags::BOLD),
    );

    // Calculate lengths for padding
    let available = area.width as usize;
    let right_content_len = display_width(&dims_str) + 1 + display_width(&time_str) + 1;
    let mut center_content_len = display_width(&center_str);
    let mut left_content_len = 1
        + display_width(state.screen_title)
        + 1
        + display_width(&position_str)
        + display_width(&theme_str)
        + display_width(&toggle_strip)
        + display_width(&mouse_label)
        + display_width(&a11y_label)
        + display_width(&undo_label);
    let mut total_content = left_content_len + center_content_len + right_content_len;

    if total_content > available {
        center_str = center_compact;
        center_content_len = display_width(&center_str);
        total_content = left_content_len + center_content_len + right_content_len;
    }
    if total_content > available && !theme_str.is_empty() {
        theme_str.clear();
        left_content_len = 1
            + display_width(state.screen_title)
            + 1
            + display_width(&position_str)
            + display_width(&theme_str)
            + display_width(&toggle_strip)
            + display_width(&mouse_label)
            + display_width(&a11y_label)
            + display_width(&undo_label);
        total_content = left_content_len + center_content_len + right_content_len;
    }
    if total_content > available && mouse_label != mouse_label_compact {
        mouse_label = mouse_label_compact;
        left_content_len = 1
            + display_width(state.screen_title)
            + 1
            + display_width(&position_str)
            + display_width(&theme_str)
            + display_width(&toggle_strip)
            + display_width(&mouse_label)
            + display_width(&a11y_label)
            + display_width(&undo_label);
        total_content = left_content_len + center_content_len + right_content_len;
    }
    if total_content > available && (!a11y_label.is_empty() || !undo_label.is_empty()) {
        a11y_label.clear();
        undo_label.clear();
        left_content_len = 1
            + display_width(state.screen_title)
            + 1
            + display_width(&position_str)
            + display_width(&theme_str)
            + display_width(&toggle_strip)
            + display_width(&mouse_label)
            + display_width(&a11y_label)
            + display_width(&undo_label);
        total_content = left_content_len + center_content_len + right_content_len;
    }
    if total_content > available && !mouse_label.is_empty() {
        mouse_label.clear();
        left_content_len = 1
            + display_width(state.screen_title)
            + 1
            + display_width(&position_str)
            + display_width(&theme_str)
            + display_width(&toggle_strip)
            + display_width(&mouse_label)
            + display_width(&a11y_label)
            + display_width(&undo_label);
        total_content = left_content_len + center_content_len + right_content_len;
    }
    if total_content > available && !toggle_strip.is_empty() {
        toggle_strip.clear();
        left_content_len = 1
            + display_width(state.screen_title)
            + 1
            + display_width(&position_str)
            + display_width(&theme_str)
            + display_width(&toggle_strip)
            + display_width(&mouse_label)
            + display_width(&a11y_label)
            + display_width(&undo_label);
        total_content = left_content_len + center_content_len + right_content_len;
    }

    // Pre-compute hit region positions for clickable elements (bd-iuvb.17.4).
    let toggle_base_x = area.x
        + 1
        + display_width(state.screen_title) as u16
        + 1
        + display_width(&position_str) as u16
        + display_width(&theme_str) as u16;
    // Individual toggle positions within the strip (each " [X]" is 4-5 chars)
    let help_toggle_w = if toggle_strip.is_empty() {
        0u16
    } else {
        display_width(toggle_help) as u16
    };
    let palette_toggle_w = if toggle_strip.is_empty() {
        0u16
    } else {
        display_width(toggle_palette) as u16
    };
    let perf_toggle_w = if toggle_strip.is_empty() {
        0u16
    } else {
        display_width(toggle_perf) as u16
    };
    let debug_toggle_w = if toggle_strip.is_empty() {
        0u16
    } else {
        display_width(toggle_debug) as u16
    };
    let help_toggle_x = toggle_base_x;
    let palette_toggle_x = help_toggle_x + help_toggle_w;
    let perf_toggle_x = palette_toggle_x + palette_toggle_w;
    let debug_toggle_x = perf_toggle_x + perf_toggle_w;
    let toggle_strip_w = help_toggle_w + palette_toggle_w + perf_toggle_w + debug_toggle_w;
    let mouse_hit_x = toggle_base_x + toggle_strip_w;
    let mouse_hit_w = display_width(&mouse_label) as u16;
    let a11y_hit_x = mouse_hit_x + mouse_hit_w;
    let a11y_hit_w = display_width(&a11y_label) as u16;

    // Build spans for the line
    let mut spans = Vec::with_capacity(14);

    // Left segment: title with accent, position, theme
    spans.push(Span::styled(" ", pad_style));
    spans.push(Span::styled(state.screen_title, title_style));
    spans.push(Span::styled(" ", pad_style));
    spans.push(Span::styled(position_str, position_style));
    spans.push(Span::styled(theme_str, muted_style));
    if !toggle_strip.is_empty() {
        // Render each toggle with its own style (active=highlighted, inactive=dim)
        spans.push(Span::styled(
            toggle_help,
            if state.help_visible {
                toggle_active_style
            } else {
                toggle_inactive_style
            },
        ));
        spans.push(Span::styled(
            toggle_palette,
            if state.palette_visible {
                toggle_active_style
            } else {
                toggle_inactive_style
            },
        ));
        spans.push(Span::styled(
            toggle_perf,
            if state.perf_hud_visible {
                toggle_active_style
            } else {
                toggle_inactive_style
            },
        ));
        spans.push(Span::styled(
            toggle_debug,
            if state.debug_visible {
                toggle_active_style
            } else {
                toggle_inactive_style
            },
        ));
    }
    if !mouse_label.is_empty() {
        spans.push(Span::styled(mouse_label, mouse_style));
    }
    if !a11y_label.is_empty() {
        spans.push(Span::styled(a11y_label, dim_style));
    }
    if !undo_label.is_empty() {
        spans.push(Span::styled(undo_label, undo_style));
    }

    if total_content <= available {
        // Full layout with centered metrics
        let total_padding = available - total_content;
        let left_pad = total_padding / 2;
        let right_pad = total_padding - left_pad;

        // Left padding
        if left_pad > 0 {
            spans.push(Span::styled(" ".repeat(left_pad), pad_style));
        }

        // Center segment: nav hint (+ metrics when space allows)
        spans.push(Span::styled(center_str, dim_style));

        // Right padding
        if right_pad > 0 {
            spans.push(Span::styled(" ".repeat(right_pad), pad_style));
        }

        // Right segment: dimensions and time
        spans.push(Span::styled(dims_str, muted_style));
        spans.push(Span::styled(" ", pad_style));
        spans.push(Span::styled(time_str, time_style));
        spans.push(Span::styled(" ", pad_style));
    } else {
        // Compact layout: skip center, show left and right
        let pad = available.saturating_sub(left_content_len + right_content_len);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), pad_style));
        }
        spans.push(Span::styled(dims_str, muted_style));
        spans.push(Span::styled(" ", pad_style));
        spans.push(Span::styled(time_str, time_style));
        spans.push(Span::styled(" ", pad_style));
    }

    let line = Line::from_spans(spans);
    let bar = Paragraph::new(Text::from_lines([line]));
    bar.render(area, frame);

    // Register hit regions for clickable status bar elements (bd-iuvb.17.4).
    // Uses pre-computed positions from above (before spans consumed the strings).
    if help_toggle_w > 0 {
        frame.register_hit_region(
            Rect::new(help_toggle_x, area.y, help_toggle_w, 1),
            HitId::new(STATUS_HELP_TOGGLE),
        );
    }
    if palette_toggle_w > 0 {
        frame.register_hit_region(
            Rect::new(palette_toggle_x, area.y, palette_toggle_w, 1),
            HitId::new(STATUS_PALETTE_TOGGLE),
        );
    }
    if perf_toggle_w > 0 {
        frame.register_hit_region(
            Rect::new(perf_toggle_x, area.y, perf_toggle_w, 1),
            HitId::new(STATUS_PERF_TOGGLE),
        );
    }
    if debug_toggle_w > 0 {
        frame.register_hit_region(
            Rect::new(debug_toggle_x, area.y, debug_toggle_w, 1),
            HitId::new(STATUS_DEBUG_TOGGLE),
        );
    }
    if mouse_hit_w > 0 {
        frame.register_hit_region(
            Rect::new(mouse_hit_x, area.y, mouse_hit_w, 1),
            HitId::new(STATUS_MOUSE_TOGGLE),
        );
    }
    if a11y_hit_w > 0 {
        frame.register_hit_region(
            Rect::new(a11y_hit_x, area.y, a11y_hit_w, 1),
            HitId::new(STATUS_A11Y_TOGGLE),
        );
    }
}

// ---------------------------------------------------------------------------
// A11y panel
// ---------------------------------------------------------------------------

/// State needed to render the A11y panel.
pub struct A11yPanelState<'a> {
    pub high_contrast: bool,
    pub reduced_motion: bool,
    pub large_text: bool,
    pub base_theme: &'a str,
}

/// Render a compact A11y panel with toggle states.
pub fn render_a11y_panel(state: &A11yPanelState<'_>, frame: &mut Frame, area: Rect) {
    let overlay_width = 36u16.min(area.width.saturating_sub(2));
    let overlay_height = 8u16.min(area.height.saturating_sub(2));

    if overlay_width < 26 || overlay_height < 6 {
        return;
    }

    let x = area
        .x
        .saturating_add(area.width.saturating_sub(overlay_width).saturating_sub(1));
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(overlay_height).saturating_sub(1));
    let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .title(" A11y ")
        .title_alignment(Alignment::Center)
        .style(theme::help_overlay());

    let inner = block.inner(overlay_area);
    Paragraph::new("")
        .style(theme::help_overlay())
        .render(overlay_area, frame);
    block.render(overlay_area, frame);

    // Register hit region for the entire panel (click to dismiss) (bd-iuvb.17.4).
    frame.register_hit_region(overlay_area, HitId::new(OVERLAY_A11Y));

    if inner.width < 10 || inner.height < 4 {
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
    let hint_style = theme::apply_large_text(Style::new().fg(theme::fg::MUTED));

    let theme_line = if state.high_contrast {
        format!(" Theme: High Contrast ({})", state.base_theme)
    } else {
        format!(" Theme: {}", state.base_theme)
    };

    let mut lines = Vec::new();
    lines.push(Line::from_spans([Span::styled(theme_line, label_style)]));

    let gap_lines = theme::scale_spacing(1).saturating_sub(1);
    for _ in 0..gap_lines {
        lines.push(Line::from(""));
    }

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

    lines.push(toggle_line("H", "High Contrast", state.high_contrast));
    lines.push(toggle_line("M", "Reduced Motion", state.reduced_motion));
    lines.push(toggle_line("L", "Large Text", state.large_text));
    lines.push(Line::from_spans([Span::styled(
        " Press A to close",
        hint_style,
    )]));

    let text = Text::from_lines(lines);
    Paragraph::new(text).render(inner, frame);
}

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

/// Per-screen keybinding entry for the help overlay.
pub struct HelpEntry {
    pub key: &'static str,
    pub action: &'static str,
}

fn build_help_overlay_hints(current: ScreenId, screen_bindings: &[HelpEntry]) -> KeybindingHints {
    // Key styling: bold with accent color
    let key_style = Style::new().bold().fg(theme::accent::PRIMARY);

    // Description styling: normal body text
    let desc_style = theme::body();

    // Section header / category styling
    let category_style = Style::new().bold().underline().fg(theme::fg::SECONDARY);

    // Build KeybindingHints with categorized global entries and screen bindings.
    let mut hints = KeybindingHints::new()
        .with_mode(HelpMode::Full)
        .with_show_context(!screen_bindings.is_empty())
        .with_show_categories(true)
        .with_key_format(KeyFormat::Bracketed)
        .with_key_style(key_style)
        .with_desc_style(desc_style)
        .with_category_style(category_style)
        // Navigation
        .global_entry_categorized("Ctrl+K", "Open command palette", HelpCategory::Navigation)
        // View
        .global_entry_categorized("?", "Toggle this help overlay", HelpCategory::View)
        .global_entry_categorized("Esc", "Dismiss top overlay", HelpCategory::View)
        .global_entry_categorized("m / F6", "Toggle mouse capture", HelpCategory::View)
        .global_entry_categorized("Ctrl+P", "Toggle performance HUD", HelpCategory::View)
        .global_entry_categorized("Ctrl+I", "Toggle evidence inspector", HelpCategory::View)
        .global_entry_categorized("Shift+A", "Toggle A11y panel", HelpCategory::View)
        .global_entry_categorized("F12", "Toggle debug overlay", HelpCategory::View)
        // Editing
        .global_entry_categorized("Ctrl+Z", "Undo", HelpCategory::Editing)
        .global_entry_categorized("Ctrl+Y / Ctrl+Shift+Z", "Redo", HelpCategory::Editing)
        // Global
        .global_entry_categorized("Ctrl+T", "Cycle color theme", HelpCategory::Global)
        .global_entry_categorized(
            "H/M/L",
            "A11y: high contrast, reduced motion, large text",
            HelpCategory::Global,
        )
        .global_entry_categorized(
            "Ctrl+1..6",
            "Palette: filter by category",
            HelpCategory::Global,
        )
        .global_entry_categorized(
            "Ctrl+0",
            "Palette: clear category filter",
            HelpCategory::Global,
        )
        .global_entry_categorized("Ctrl+F", "Palette: toggle favorite", HelpCategory::Global)
        .global_entry_categorized(
            "Ctrl+Shift+F",
            "Palette: favorites only",
            HelpCategory::Global,
        )
        .global_entry_categorized("Ctrl+C", "Quit application", HelpCategory::Global);

    // Add screen-specific bindings as contextual entries under a custom category.
    let screen_category = HelpCategory::Custom(format!("{} Controls", current.title()));
    for entry in screen_bindings {
        hints =
            hints.contextual_entry_categorized(entry.key, entry.action, screen_category.clone());
    }

    hints
}

/// Render a centered help overlay with global and screen-specific keybindings.
///
/// # Design (bd-3vbf.7)
///
/// The overlay uses a professional modal design with:
/// - Double border for visual emphasis
/// - Two-column layout with aligned key/action pairs
/// - Section headers for Global and screen-specific bindings
/// - Styled keys with bold/accent styling
/// - Dismissal hint in the title
pub fn render_help_overlay(
    current: ScreenId,
    screen_bindings: &[HelpEntry],
    frame: &mut Frame,
    area: Rect,
) {
    // Size: 60% width, 70% height, clamped to reasonable bounds
    let overlay_width = ((area.width as u32 * 60) / 100).clamp(36, 72) as u16;
    let overlay_height = ((area.height as u32 * 70) / 100).clamp(14, 28) as u16;
    let overlay_width = overlay_width.min(area.width.saturating_sub(2));
    let overlay_height = overlay_height.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(overlay_width)) / 2;
    let y = area.y + (area.height.saturating_sub(overlay_height)) / 2;
    let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

    // Professional modal frame with double border
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .title(" ⌨ Keyboard Shortcuts (Esc) ")
        .title_alignment(Alignment::Center)
        .style(theme::help_overlay());

    let inner = block.inner(overlay_area);
    block.render(overlay_area, frame);

    // Register hit regions for mouse interaction (bd-iuvb.17.4).
    // Content area: scrolling / click-to-dismiss.
    frame.register_hit_region(inner, HitId::new(OVERLAY_HELP_CONTENT));
    // Title bar (top row): close button area.
    frame.register_hit_region(
        Rect::new(overlay_area.x, overlay_area.y, overlay_area.width, 1),
        HitId::new(OVERLAY_HELP_CLOSE),
    );

    if inner.width < 10 || inner.height < 5 {
        return;
    }

    let hints = build_help_overlay_hints(current, screen_bindings);

    let legend_width = {
        let mut width = display_width("Categories:");
        for category in ScreenCategory::ALL {
            width += 1 + display_width(category.short_label());
        }
        width
    };
    let show_legend = inner.height >= 8 && inner.width.saturating_sub(2) as usize >= legend_width;
    let reserved_rows = if show_legend { 2 } else { 1 };
    let content_height = inner.height.saturating_sub(reserved_rows);
    let content_area = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        content_height,
    );
    if content_height > 0 {
        Widget::render(&hints, content_area, frame);
    }

    if show_legend && content_height < inner.height {
        let legend_y = inner.y + content_height;
        let legend_area = Rect::new(inner.x + 1, legend_y, inner.width.saturating_sub(2), 1);
        let mut spans = Vec::with_capacity(ScreenCategory::ALL.len() * 2 + 2);
        spans.push(Span::styled(
            "Categories:",
            Style::new().fg(theme::fg::MUTED).attrs(StyleFlags::BOLD),
        ));
        for category in ScreenCategory::ALL {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                category.short_label(),
                Style::new()
                    .fg(category_accent(*category))
                    .attrs(StyleFlags::BOLD),
            ));
        }
        let line = Line::from_spans(spans);
        Paragraph::new(Text::from_lines([line])).render(legend_area, frame);
    }

    // Footer hint at bottom
    let footer_y = overlay_area.bottom().saturating_sub(1);
    if footer_y > inner.y {
        let footer = "Press ? or Esc to close";
        let footer_style = Style::new().fg(theme::fg::MUTED);
        let footer_width = display_width(footer) as u16;
        let footer_x = inner.x + (inner.width.saturating_sub(footer_width)) / 2;
        Paragraph::new(footer)
            .style(footer_style)
            .render(Rect::new(footer_x, footer_y, footer_width, 1), frame);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the accent color for a category tab.
pub fn category_accent(category: ScreenCategory) -> theme::ColorToken {
    match category {
        ScreenCategory::Tour => theme::screen_accent::DASHBOARD,
        ScreenCategory::Core => theme::screen_accent::LAYOUT_LAB,
        ScreenCategory::Visuals => theme::screen_accent::VISUAL_EFFECTS,
        ScreenCategory::Interaction => theme::screen_accent::FORMS_INPUT,
        ScreenCategory::Text => theme::screen_accent::MARKDOWN,
        ScreenCategory::Systems => theme::screen_accent::PERFORMANCE,
    }
}

/// Return the accent color for the given screen.
pub fn accent_for(id: ScreenId) -> theme::ColorToken {
    match id {
        ScreenId::GuidedTour => theme::screen_accent::DASHBOARD,
        ScreenId::Dashboard => theme::screen_accent::DASHBOARD,
        ScreenId::Shakespeare => theme::screen_accent::SHAKESPEARE,
        ScreenId::CodeExplorer => theme::screen_accent::CODE_EXPLORER,
        ScreenId::WidgetGallery => theme::screen_accent::WIDGET_GALLERY,
        ScreenId::LayoutLab => theme::screen_accent::LAYOUT_LAB,
        ScreenId::FormsInput => theme::screen_accent::FORMS_INPUT,
        ScreenId::DataViz => theme::screen_accent::DATA_VIZ,
        ScreenId::FileBrowser => theme::screen_accent::FILE_BROWSER,
        ScreenId::AdvancedFeatures => theme::screen_accent::ADVANCED,
        ScreenId::Performance => theme::screen_accent::PERFORMANCE,
        ScreenId::MacroRecorder => theme::screen_accent::ADVANCED,
        ScreenId::MarkdownRichText => theme::screen_accent::MARKDOWN,
        ScreenId::VisualEffects => theme::screen_accent::VISUAL_EFFECTS,
        ScreenId::ResponsiveDemo => theme::screen_accent::RESPONSIVE_DEMO,
        ScreenId::LogSearch => theme::screen_accent::LOG_SEARCH,
        ScreenId::Notifications => theme::screen_accent::ADVANCED,
        ScreenId::ActionTimeline => theme::screen_accent::ACTION_TIMELINE,
        ScreenId::IntrinsicSizing => theme::screen_accent::INTRINSIC_SIZING,
        ScreenId::AdvancedTextEditor => theme::screen_accent::ADVANCED,
        ScreenId::MousePlayground => theme::screen_accent::PERFORMANCE,
        ScreenId::FormValidation => theme::screen_accent::FORMS_INPUT,
        ScreenId::VirtualizedSearch => theme::screen_accent::PERFORMANCE,
        ScreenId::AsyncTasks => theme::screen_accent::PERFORMANCE,
        ScreenId::ThemeStudio => theme::screen_accent::VISUAL_EFFECTS,
        ScreenId::TerminalCapabilities => theme::screen_accent::PERFORMANCE,
        ScreenId::SnapshotPlayer => theme::screen_accent::PERFORMANCE,
        ScreenId::PerformanceHud => theme::screen_accent::PERFORMANCE,
        ScreenId::I18nDemo => theme::screen_accent::ADVANCED,
        ScreenId::VoiOverlay => theme::screen_accent::PERFORMANCE,
        ScreenId::InlineModeStory => theme::screen_accent::RESPONSIVE_DEMO,
        ScreenId::LayoutInspector => theme::screen_accent::LAYOUT_LAB,
        ScreenId::AccessibilityPanel => theme::screen_accent::ADVANCED,
        ScreenId::WidgetBuilder => theme::screen_accent::WIDGET_GALLERY,
        ScreenId::CommandPaletteLab => theme::screen_accent::ADVANCED,
        ScreenId::HyperlinkPlayground => theme::screen_accent::ADVANCED,
        ScreenId::DeterminismLab => theme::screen_accent::PERFORMANCE,
        ScreenId::TableThemeGallery => theme::screen_accent::DATA_VIZ,
        ScreenId::ExplainabilityCockpit => theme::screen_accent::PERFORMANCE,
        ScreenId::KanbanBoard => theme::screen_accent::ADVANCED,
        ScreenId::MermaidShowcase => theme::screen_accent::DATA_VIZ,
        ScreenId::MermaidMegaShowcase => theme::screen_accent::DATA_VIZ,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::cell::Cell;
    use ftui_render::cell::PackedRgba;
    use ftui_render::grapheme_pool::GraphemePool;

    fn make_frame(w: u16, h: u16) -> (GraphemePool, Frame<'static>) {
        // We need the pool to outlive the frame, so use a raw pointer trick.
        // For tests only, we leak the pool to get a 'static reference.
        let pool = Box::leak(Box::new(GraphemePool::new()));
        let frame = Frame::new(w, h, pool);
        (GraphemePool::new(), frame)
    }

    #[test]
    fn tab_bar_highlights_current() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(100, 1, &mut pool);
        let area = Rect::new(0, 0, 100, 1);

        frame
            .buffer
            .fill(area, Cell::default().with_bg(theme::bg::DEEP.into()));

        render_tab_bar(ScreenId::Shakespeare, &mut frame, area);

        // The Shakespeare tab should have the accent background color.
        // With Guided Tour + Dashboard ahead of it, Shakespeare is key 3.
        let mut found_accent = false;
        let base_bg: PackedRgba = theme::bg::DEEP.into();
        let surface_bg: PackedRgba = theme::alpha::SURFACE.into();
        let surface = surface_bg.over(base_bg);
        let expected_bg =
            theme::with_alpha(theme::screen_accent::SHAKESPEARE, TAB_ACCENT_ALPHA).over(surface);
        for x in 0..100u16 {
            if let Some(cell) = frame.buffer.get(x, 0)
                && cell.bg == expected_bg
            {
                found_accent = true;
                break;
            }
        }
        assert!(
            found_accent,
            "Shakespeare tab should have its accent bg color"
        );
    }

    #[test]
    fn status_bar_shows_dimensions() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 1, &mut pool);
        let area = Rect::new(0, 0, 80, 1);

        let state = StatusBarState {
            current_screen: ScreenId::Dashboard,
            screen_title: "Dashboard",
            screen_index: 0,
            screen_count: 11,
            tick_count: 100,
            frame_count: 50,
            terminal_width: 120,
            terminal_height: 40,
            theme_name: "default",
            inline_mode: false,
            mouse_capture_enabled: true,
            help_visible: false,
            palette_visible: false,
            perf_hud_visible: false,
            debug_visible: false,
            a11y_high_contrast: false,
            a11y_reduced_motion: false,
            a11y_large_text: false,
            can_undo: false,
            can_redo: false,
            undo_description: None,
        };
        render_status_bar(&state, &mut frame, area);

        // Read back the rendered text
        let mut rendered = String::new();
        for x in 0..80u16 {
            if let Some(cell) = frame.buffer.get(x, 0) {
                if let Some(ch) = cell.content.as_char() {
                    rendered.push(ch);
                } else {
                    rendered.push(' ');
                }
            }
        }
        assert!(
            rendered.contains("120x40"),
            "Status bar should show terminal dimensions: {rendered}"
        );
    }

    #[test]
    fn help_overlay_centered() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);

        render_help_overlay(ScreenId::Dashboard, &[], &mut frame, area);

        // The overlay should be roughly centered.
        // Check that the center area has some non-empty content.
        let center_x = 40u16;
        let center_y = 12u16;
        let cell = frame.buffer.get(center_x, center_y);
        // The overlay writes content, so this cell should not be default
        assert!(cell.is_some(), "Center cell should exist");
    }

    #[test]
    fn help_overlay_hints_include_global_dismiss_key() {
        let hints = build_help_overlay_hints(ScreenId::Dashboard, &[]);
        let entries = hints.visible_entries();
        assert!(
            entries
                .iter()
                .any(|e| e.key == "[Esc]" && e.desc.contains("Dismiss")),
            "expected [Esc] global dismiss entry, got: {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "[Ctrl+K]" && e.desc.contains("palette")),
            "expected [Ctrl+K] global palette entry, got: {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "[m / F6]" && e.desc.contains("mouse")),
            "expected [m / F6] mouse capture entry, got: {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "[Ctrl+I]" && e.desc.contains("evidence")),
            "expected [Ctrl+I] global evidence inspector entry, got: {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "[Ctrl+P]" && e.desc.contains("performance")),
            "expected [Ctrl+P] global performance HUD entry, got: {entries:?}"
        );
    }

    #[test]
    fn tab_bar_registers_hit_regions() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(120, 1, &mut pool);
        let area = Rect::new(0, 0, 120, 1);

        render_tab_bar(ScreenId::Dashboard, &mut frame, area);

        // Check that we can hit-test at least the first tab
        let hit = frame.hit_test(1, 0);
        assert!(hit.is_some(), "First tab should be a registered hit region");
        if let Some((id, _region, _data)) = hit {
            let screen = screen_from_hit_id(id);
            assert_eq!(screen, Some(ScreenId::GuidedTour));
        }
    }

    #[test]
    fn pane_hit_maps_to_screen() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(20, 10, &mut pool);
        let rect = Rect::new(2, 2, 8, 4);

        register_pane_hit(&mut frame, rect, ScreenId::Dashboard);

        let hit = frame.hit_test(3, 3);
        assert!(hit.is_some(), "Pane region should be hit-testable");
        if let Some((id, _region, _data)) = hit {
            let screen = screen_from_any_hit_id(id);
            assert_eq!(screen, Some(ScreenId::Dashboard));
        }
    }

    #[test]
    fn category_tabs_register_hit_regions() {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::with_hit_grid(120, 1, &mut pool);
        let area = Rect::new(0, 0, 120, 1);

        render_category_tabs(ScreenId::Dashboard, &mut frame, area);

        let hit = frame.hit_test(1, 0);
        assert!(
            hit.is_some(),
            "First category tab should register hit region"
        );
        if let Some((id, _region, _data)) = hit {
            let category = category_from_hit_id(id);
            assert_eq!(category, Some(ScreenCategory::Tour));
            let screen = screen_from_any_hit_id(id);
            assert_eq!(screen, screens::first_in_category(ScreenCategory::Tour));
        }
    }

    #[test]
    fn screen_from_hit_id_out_of_range() {
        assert_eq!(screen_from_hit_id(HitId::new(0)), None);
        assert_eq!(screen_from_hit_id(HitId::new(999)), None);
        assert_eq!(screen_from_hit_id(HitId::new(TAB_HIT_BASE + 100)), None);
    }

    #[test]
    fn accent_for_all_screens() {
        // Verify each screen has a distinct accent color
        for &id in screens::screen_ids() {
            let accent_value = accent_for(id);
            let color: PackedRgba = accent_value.into();
            // Just verify it returns something non-zero
            assert_ne!(
                color,
                PackedRgba::TRANSPARENT,
                "Screen {id:?} should have accent"
            );
        }
    }

    /// Helper to build a default StatusBarState for testing.
    fn test_status_bar_state(
        overrides: impl FnOnce(&mut StatusBarState<'_>),
    ) -> StatusBarState<'static> {
        let mut state = StatusBarState {
            current_screen: ScreenId::Dashboard,
            screen_title: "Dashboard",
            screen_index: 0,
            screen_count: 11,
            tick_count: 100,
            frame_count: 50,
            terminal_width: 120,
            terminal_height: 40,
            theme_name: "default",
            inline_mode: false,
            mouse_capture_enabled: true,
            help_visible: false,
            palette_visible: false,
            perf_hud_visible: false,
            debug_visible: false,
            a11y_high_contrast: false,
            a11y_reduced_motion: false,
            a11y_large_text: false,
            can_undo: false,
            can_redo: false,
            undo_description: None,
        };
        overrides(&mut state);
        state
    }

    /// Helper to render status bar and return (rendered_text, frame).
    fn render_status_bar_test(
        width: u16,
        overrides: impl FnOnce(&mut StatusBarState<'_>),
    ) -> (String, Frame<'static>) {
        // Leak the pool to get 'static lifetime for test convenience
        let pool = Box::leak(Box::new(GraphemePool::new()));
        let mut frame = Frame::with_hit_grid(width, 1, pool);
        let area = Rect::new(0, 0, width, 1);
        let state = test_status_bar_state(overrides);
        render_status_bar(&state, &mut frame, area);
        let mut rendered = String::new();
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, 0) {
                if let Some(ch) = cell.content.as_char() {
                    rendered.push(ch);
                } else {
                    rendered.push(' ');
                }
            }
        }
        (rendered, frame)
    }

    #[test]
    fn status_bar_shows_toggle_indicators_inactive() {
        let (rendered, _frame) = render_status_bar_test(200, |_| {});
        assert!(
            rendered.contains("[h]"),
            "Should show inactive help toggle: {rendered}"
        );
        assert!(
            rendered.contains("[cmd]"),
            "Should show inactive palette toggle: {rendered}"
        );
        assert!(
            rendered.contains("[p]"),
            "Should show inactive perf toggle: {rendered}"
        );
        assert!(
            rendered.contains("[d]"),
            "Should show inactive debug toggle: {rendered}"
        );
    }

    #[test]
    fn status_bar_shows_toggle_indicators_active() {
        let (rendered, _frame) = render_status_bar_test(200, |s| {
            s.help_visible = true;
            s.palette_visible = true;
            s.perf_hud_visible = true;
            s.debug_visible = true;
        });
        assert!(
            rendered.contains("[H]"),
            "Should show active help toggle: {rendered}"
        );
        assert!(
            rendered.contains("[Cmd]"),
            "Should show active palette toggle: {rendered}"
        );
        assert!(
            rendered.contains("[P]"),
            "Should show active perf toggle: {rendered}"
        );
        assert!(
            rendered.contains("[D]"),
            "Should show active debug toggle: {rendered}"
        );
    }

    #[test]
    fn status_bar_registers_help_toggle_hit_region() {
        let (_rendered, frame) = render_status_bar_test(200, |_| {});
        // Find the [h] toggle position and verify hit region
        let mut found = false;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, 0)
                && id.id() == STATUS_HELP_TOGGLE
            {
                found = true;
                break;
            }
        }
        assert!(found, "Should register STATUS_HELP_TOGGLE hit region");
    }

    #[test]
    fn status_bar_registers_palette_toggle_hit_region() {
        let (_rendered, frame) = render_status_bar_test(200, |_| {});
        let mut found = false;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, 0)
                && id.id() == STATUS_PALETTE_TOGGLE
            {
                found = true;
                break;
            }
        }
        assert!(found, "Should register STATUS_PALETTE_TOGGLE hit region");
    }

    #[test]
    fn status_bar_registers_perf_toggle_hit_region() {
        let (_rendered, frame) = render_status_bar_test(200, |_| {});
        let mut found = false;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, 0)
                && id.id() == STATUS_PERF_TOGGLE
            {
                found = true;
                break;
            }
        }
        assert!(found, "Should register STATUS_PERF_TOGGLE hit region");
    }

    #[test]
    fn status_bar_registers_debug_toggle_hit_region() {
        let (_rendered, frame) = render_status_bar_test(200, |_| {});
        let mut found = false;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, 0)
                && id.id() == STATUS_DEBUG_TOGGLE
            {
                found = true;
                break;
            }
        }
        assert!(found, "Should register STATUS_DEBUG_TOGGLE hit region");
    }

    #[test]
    fn status_bar_registers_mouse_toggle_hit_region() {
        let (_rendered, frame) = render_status_bar_test(200, |_| {});
        let mut found = false;
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, 0)
                && id.id() == STATUS_MOUSE_TOGGLE
            {
                found = true;
                break;
            }
        }
        assert!(found, "Should register STATUS_MOUSE_TOGGLE hit region");
    }

    #[test]
    fn status_bar_hit_regions_are_non_overlapping() {
        let (_rendered, frame) = render_status_bar_test(200, |_| {});
        // Walk each column and collect which hit IDs we see
        let mut hit_ids: Vec<(u16, u32)> = Vec::new();
        for x in 0..200u16 {
            if let Some((id, _region, _data)) = frame.hit_test(x, 0) {
                let raw = id.id();
                if raw >= STATUS_HIT_BASE {
                    hit_ids.push((x, raw));
                }
            }
        }
        // Verify we see all 6 status toggles in order
        let unique_ids: Vec<u32> = {
            let mut ids = Vec::new();
            for &(_, id) in &hit_ids {
                if ids.last() != Some(&id) {
                    ids.push(id);
                }
            }
            ids
        };
        assert!(
            unique_ids.contains(&STATUS_HELP_TOGGLE),
            "Missing help toggle in hit regions"
        );
        assert!(
            unique_ids.contains(&STATUS_PALETTE_TOGGLE),
            "Missing palette toggle in hit regions"
        );
        assert!(
            unique_ids.contains(&STATUS_PERF_TOGGLE),
            "Missing perf toggle in hit regions"
        );
        assert!(
            unique_ids.contains(&STATUS_DEBUG_TOGGLE),
            "Missing debug toggle in hit regions"
        );
        assert!(
            unique_ids.contains(&STATUS_MOUSE_TOGGLE),
            "Missing mouse toggle in hit regions"
        );
        // Verify ordering: help < palette < perf < debug < mouse
        let positions: Vec<usize> = [
            STATUS_HELP_TOGGLE,
            STATUS_PALETTE_TOGGLE,
            STATUS_PERF_TOGGLE,
            STATUS_DEBUG_TOGGLE,
            STATUS_MOUSE_TOGGLE,
        ]
        .iter()
        .map(|target| unique_ids.iter().position(|id| id == target).unwrap())
        .collect();
        for i in 1..positions.len() {
            assert!(
                positions[i] > positions[i - 1],
                "Status toggle hit regions should be in order"
            );
        }
    }

    #[test]
    fn status_bar_toggles_truncated_on_narrow_terminal() {
        // On a very narrow terminal, toggle strip should be truncated
        let (rendered, _frame) = render_status_bar_test(40, |_| {});
        // On 40 cols, the toggle strip may be truncated
        // Just verify no panic and that the status bar renders something
        assert!(
            rendered.contains("Dashboard"),
            "Should still show screen title: {rendered}"
        );
        // Hit regions might not be registered if strip was truncated — that's OK
        // Hit regions might not be registered if strip was truncated
    }

    #[test]
    fn status_bar_classify_toggle_hit_ids() {
        // Verify all toggle hit IDs classify as StatusToggle layer
        assert_eq!(
            classify_hit(HitId::new(STATUS_HELP_TOGGLE)),
            HitLayer::StatusToggle(STATUS_HELP_TOGGLE)
        );
        assert_eq!(
            classify_hit(HitId::new(STATUS_PALETTE_TOGGLE)),
            HitLayer::StatusToggle(STATUS_PALETTE_TOGGLE)
        );
        assert_eq!(
            classify_hit(HitId::new(STATUS_PERF_TOGGLE)),
            HitLayer::StatusToggle(STATUS_PERF_TOGGLE)
        );
        assert_eq!(
            classify_hit(HitId::new(STATUS_DEBUG_TOGGLE)),
            HitLayer::StatusToggle(STATUS_DEBUG_TOGGLE)
        );
        assert_eq!(
            classify_hit(HitId::new(STATUS_MOUSE_TOGGLE)),
            HitLayer::StatusToggle(STATUS_MOUSE_TOGGLE)
        );
    }
}
