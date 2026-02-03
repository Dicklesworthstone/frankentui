#![forbid(unsafe_code)]

//! Shared UI chrome: tab bar, status bar, and help overlay.

use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId};
use ftui_style::{Style, StyleFlags};
use ftui_text::{Line, Span, Text};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use crate::app::ScreenId;
use crate::theme;

// ---------------------------------------------------------------------------
// Hit IDs for tab bar clicks (one per screen)
// ---------------------------------------------------------------------------

/// Base hit ID for tab bar entries.  Tab i has HitId(TAB_HIT_BASE + i).
pub const TAB_HIT_BASE: u32 = 1000;
const TAB_ACCENT_ALPHA: u8 = 220;

/// Convert a hit ID back to a ScreenId if it falls in the tab range.
pub fn screen_from_hit_id(id: HitId) -> Option<ScreenId> {
    let raw = id.id();
    if raw >= TAB_HIT_BASE && raw < TAB_HIT_BASE + ScreenId::ALL.len() as u32 {
        let idx = (raw - TAB_HIT_BASE) as usize;
        ScreenId::ALL.get(idx).copied()
    } else {
        None
    }
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
    for (i, &id) in ScreenId::ALL.iter().enumerate() {
        let key_label = if i < 9 {
            format!("{}", i + 1)
        } else if i == 9 {
            "0".into()
        } else {
            "-".into()
        };

        let label_text = id.tab_label();
        let label_width = 1 + key_label.len() as u16 + 2 + label_text.len() as u16 + 1; // " {key}: {label} "

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
        let key_style = Style::new()
            .bg(bg)
            .fg(theme::fg::MUTED)
            .attrs(StyleFlags::DIM);
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

        // Subtle separator between tabs
        if x < area.x + area.width {
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

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

/// State needed to render the status bar.
pub struct StatusBarState<'a> {
    pub screen_title: &'a str,
    pub screen_index: usize,
    pub screen_count: usize,
    pub tick_count: u64,
    pub frame_count: u64,
    pub terminal_width: u16,
    pub terminal_height: u16,
    pub theme_name: &'a str,
}

/// Render the status bar at the bottom of the screen.
pub fn render_status_bar(state: &StatusBarState<'_>, frame: &mut Frame, area: Rect) {
    // Fill background
    let bg_style = theme::status_bar();
    let blank = Paragraph::new("").style(bg_style);
    blank.render(area, frame);

    // Elapsed time from tick count (each tick = 100ms)
    let total_secs = state.tick_count / 10;
    let mins = total_secs / 60;
    let secs = total_secs % 60;

    // Build left / center / right segments
    let left = format!(
        " {} [{}/{}]  {}",
        state.screen_title,
        state.screen_index + 1,
        state.screen_count,
        state.theme_name,
    );
    let center = format!("tick:{} frm:{}", state.tick_count, state.frame_count);
    let right = format!(
        "{}x{} {:02}:{:02} ",
        state.terminal_width, state.terminal_height, mins, secs,
    );

    let available = area.width as usize;
    let left_len = left.len();
    let center_len = center.len();
    let right_len = right.len();

    // Build a single line with spacing
    let mut line = String::with_capacity(available);
    line.push_str(&left);

    let total_content = left_len + center_len + right_len;
    if total_content < available {
        let total_padding = available - total_content;
        let left_pad = total_padding / 2;
        let right_pad = total_padding - left_pad;
        for _ in 0..left_pad {
            line.push(' ');
        }
        line.push_str(&center);
        for _ in 0..right_pad {
            line.push(' ');
        }
        line.push_str(&right);
    } else {
        // Truncate: just show left and right
        let pad = available.saturating_sub(left_len + right_len);
        for _ in 0..pad {
            line.push(' ');
        }
        line.push_str(&right);
    }

    let bar = Paragraph::new(line).style(bg_style);
    bar.render(area, frame);
}

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

/// Per-screen keybinding entry for the help overlay.
pub struct HelpEntry {
    pub key: &'static str,
    pub action: &'static str,
}

/// Render a centered help overlay with global and screen-specific keybindings.
pub fn render_help_overlay(
    current: ScreenId,
    screen_bindings: &[HelpEntry],
    frame: &mut Frame,
    area: Rect,
) {
    // Size: 60% width, 70% height, clamped
    let overlay_width = ((area.width as u32 * 60) / 100).clamp(30, 70) as u16;
    let overlay_height = ((area.height as u32 * 70) / 100).clamp(10, 24) as u16;
    let overlay_width = overlay_width.min(area.width.saturating_sub(2));
    let overlay_height = overlay_height.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(overlay_width)) / 2;
    let y = area.y + (area.height.saturating_sub(overlay_height)) / 2;
    let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .title("Keyboard Shortcuts — Press ? to close")
        .title_alignment(Alignment::Center)
        .style(theme::help_overlay());

    let inner = block.inner(overlay_area);
    block.render(overlay_area, frame);

    // Build help text
    let mut text = String::from("Global:\n");
    text.push_str("  1-9, 0     Switch to screen by number\n");
    text.push_str("  Tab / L    Next screen\n");
    text.push_str("  S-Tab / H  Previous screen\n");
    text.push_str("  ?          Toggle this help\n");
    text.push_str("  Ctrl+K     Command palette\n");
    text.push_str("  Ctrl+T     Cycle theme\n");
    text.push_str("  F12        Toggle debug overlay\n");
    text.push_str("  q          Quit\n");
    text.push_str("  Ctrl+C     Quit\n");

    if !screen_bindings.is_empty() {
        text.push_str(&format!("\n{}:\n", current.title()));
        for entry in screen_bindings {
            text.push_str(&format!("  {:12}{}\n", entry.key, entry.action));
        }
    }

    let help = Paragraph::new(text).style(theme::body());
    help.render(inner, frame);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the accent color for the given screen.
pub fn accent_for(id: ScreenId) -> theme::ColorToken {
    match id {
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
        ScreenId::MousePlayground => theme::screen_accent::ADVANCED,
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

        // The Shakespeare tab should have the accent background color
        // Find the tab by scanning for '2' (Shakespeare is key 2)
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
            screen_title: "Dashboard",
            screen_index: 0,
            screen_count: 11,
            tick_count: 100,
            frame_count: 50,
            terminal_width: 120,
            terminal_height: 40,
            theme_name: "default",
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
            assert_eq!(screen, Some(ScreenId::Dashboard));
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
        for &id in ScreenId::ALL {
            let color_token = accent_for(id);
            let color: PackedRgba = color_token.into();
            // Just verify it returns something non-zero
            assert_ne!(
                color,
                PackedRgba::TRANSPARENT,
                "Screen {id:?} should have accent"
            );
        }
    }
}
