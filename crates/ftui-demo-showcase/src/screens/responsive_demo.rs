#![forbid(unsafe_code)]

//! Responsive layout demo screen.
//!
//! Demonstrates the responsive layout system:
//! - [`ResponsiveLayout`] switching layout structure at breakpoints
//! - [`Visibility`] hiding/showing panels based on breakpoint
//! - [`Responsive<T>`] for per-breakpoint value adaptation
//! - Live breakpoint indicator showing the current tier

use std::cell::Cell;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_layout::{
    Breakpoint, Breakpoints, Constraint, Flex, Responsive, ResponsiveLayout, Visibility,
};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Responsive layout demo screen.
pub struct ResponsiveDemo {
    /// Current terminal width (updated on resize/view).
    width: u16,
    /// Current terminal height.
    height: u16,
    /// Custom breakpoints toggle.
    use_custom_breakpoints: bool,
    /// Tick counter for subtle animations.
    tick_count: u64,
    /// Cached indicator area for mouse hit-testing.
    layout_indicator: Cell<Rect>,
}

impl Default for ResponsiveDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponsiveDemo {
    /// Create a new responsive demo screen.
    pub fn new() -> Self {
        Self {
            width: 80,
            height: 24,
            use_custom_breakpoints: false,
            tick_count: 0,
            layout_indicator: Cell::new(Rect::default()),
        }
    }

    /// Handle mouse interactions.
    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let indicator = self.layout_indicator.get();
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if indicator.contains(x, y) {
                    self.use_custom_breakpoints = !self.use_custom_breakpoints;
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if indicator.contains(x, y) {
                    // Reset to defaults
                    self.use_custom_breakpoints = false;
                    self.width = 80;
                }
            }
            MouseEventKind::ScrollUp => {
                self.width = self.width.saturating_add(10).min(300);
            }
            MouseEventKind::ScrollDown => {
                self.width = self.width.saturating_sub(10).max(20);
            }
            _ => {}
        }
    }

    fn breakpoints(&self) -> Breakpoints {
        if self.use_custom_breakpoints {
            Breakpoints::new(50, 80, 110)
        } else {
            Breakpoints::DEFAULT
        }
    }

    fn current_breakpoint(&self) -> Breakpoint {
        self.breakpoints().classify_width(self.width)
    }

    // -- Render helpers -----------------------------------------------------

    fn render_breakpoint_indicator(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let bp = self.current_breakpoint();
        let bps = self.breakpoints();

        let indicator = format!(
            " Breakpoint: {} │ Width: {} │ Thresholds: sm≥{} md≥{} lg≥{} xl≥{} ",
            bp_label(bp),
            self.width,
            bps.sm,
            bps.md,
            bps.lg,
            bps.xl,
        );

        let style = Style::new().fg(theme::fg::PRIMARY).bg(bp_color(bp)).bold();

        Paragraph::new(&*indicator).style(style).render(area, frame);
    }

    fn render_main_content(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let bp = self.current_breakpoint();

        // Responsive layout: single column on small, sidebar+content on medium+
        let layout = ResponsiveLayout::new(Flex::vertical().constraints([Constraint::Fill]))
            .at(
                Breakpoint::Md,
                Flex::horizontal().constraints([Constraint::Fixed(28), Constraint::Fill]),
            )
            .at(
                Breakpoint::Lg,
                Flex::horizontal().constraints([
                    Constraint::Fixed(28),
                    Constraint::Fill,
                    Constraint::Fixed(24),
                ]),
            )
            .with_breakpoints(self.breakpoints());

        let result = layout.split_for(bp, area);

        match result.rects.len() {
            1 => {
                // Single column: stacked info
                self.render_stacked_view(frame, result.rects[0]);
            }
            2 => {
                // Sidebar + content
                self.render_sidebar(frame, result.rects[0]);
                self.render_content_panel(frame, result.rects[1]);
            }
            3 => {
                // Sidebar + content + aside
                self.render_sidebar(frame, result.rects[0]);
                self.render_content_panel(frame, result.rects[1]);
                self.render_aside(frame, result.rects[2]);
            }
            _ => {}
        }
    }

    fn render_stacked_view(&self, frame: &mut Frame, area: Rect) {
        let chunks = Flex::vertical()
            .constraints([Constraint::Fixed(5), Constraint::Fixed(6), Constraint::Fill])
            .split(area);

        self.render_info_block("Layout Info", &self.layout_info_text(), frame, chunks[0]);
        self.render_info_block("Visibility", &self.visibility_info_text(), frame, chunks[1]);
        self.render_info_block(
            "Responsive Values",
            &self.responsive_values_text(),
            frame,
            chunks[2],
        );
    }

    fn render_sidebar(&self, frame: &mut Frame, area: Rect) {
        let bp = self.current_breakpoint();
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Sidebar")
            .style(Style::new().fg(theme::fg::PRIMARY));
        let inner = block.inner(area);
        block.render(area, frame);

        let lines = format!(
            "Breakpoint: {}\nLayout: {}\n\n{}\n\n[b] Toggle BPs\n[Current: {}]",
            bp_label(bp),
            match bp {
                Breakpoint::Xs | Breakpoint::Sm => "stacked",
                Breakpoint::Md => "2-col",
                _ => "3-col",
            },
            self.visibility_info_text(),
            if self.use_custom_breakpoints {
                "custom"
            } else {
                "default"
            },
        );
        Paragraph::new(&*lines)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(inner, frame);
    }

    fn render_content_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Content")
            .style(Style::new().fg(theme::fg::PRIMARY));
        let inner = block.inner(area);
        block.render(area, frame);

        let text = format!(
            "{}\n\n{}\n\nThe layout adapts to the terminal width.\n\
             Resize your terminal to see the layout switch\n\
             between 1-column, 2-column, and 3-column modes.",
            self.layout_info_text(),
            self.responsive_values_text(),
        );
        Paragraph::new(&*text)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(inner, frame);
    }

    fn render_aside(&self, frame: &mut Frame, area: Rect) {
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Aside")
            .style(Style::new().fg(theme::fg::PRIMARY));
        let inner = block.inner(area);
        block.render(area, frame);

        let bp = self.current_breakpoint();
        let text = format!(
            "Only visible\nat Lg+ ({}).\n\nTick: {}",
            bp_label(bp),
            self.tick_count,
        );
        Paragraph::new(&*text)
            .style(Style::new().fg(theme::fg::MUTED))
            .render(inner, frame);
    }

    fn render_info_block(&self, title: &str, content: &str, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title)
            .style(Style::new().fg(theme::fg::PRIMARY));
        let inner = block.inner(area);
        block.render(area, frame);
        Paragraph::new(content)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(inner, frame);
    }

    fn layout_info_text(&self) -> String {
        let bp = self.current_breakpoint();
        let columns = match bp {
            Breakpoint::Xs | Breakpoint::Sm => 1,
            Breakpoint::Md => 2,
            _ => 3,
        };
        format!(
            "Columns: {} │ Breakpoint: {} │ {}×{}",
            columns,
            bp_label(bp),
            self.width,
            self.height,
        )
    }

    fn visibility_info_text(&self) -> String {
        let bp = self.current_breakpoint();
        let sidebar_vis = Visibility::visible_above(Breakpoint::Md);
        let aside_vis = Visibility::visible_above(Breakpoint::Lg);

        format!(
            "Sidebar: {} (md+)\nAside:   {} (lg+)",
            if sidebar_vis.is_visible(bp) {
                "visible"
            } else {
                "hidden"
            },
            if aside_vis.is_visible(bp) {
                "visible"
            } else {
                "hidden"
            },
        )
    }

    fn responsive_values_text(&self) -> String {
        let bp = self.current_breakpoint();

        let padding = Responsive::new(1u16)
            .at(Breakpoint::Sm, 2)
            .at(Breakpoint::Md, 3)
            .at(Breakpoint::Lg, 4);

        let font_label = Responsive::new("compact")
            .at(Breakpoint::Sm, "normal")
            .at(Breakpoint::Md, "comfortable")
            .at(Breakpoint::Lg, "spacious");

        format!(
            "Padding: {} │ Style: {}",
            padding.resolve(bp),
            font_label.resolve(bp),
        )
    }
}

// ---------------------------------------------------------------------------
// Screen trait
// ---------------------------------------------------------------------------

impl Screen for ResponsiveDemo {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            self.handle_mouse(mouse.kind, mouse.x, mouse.y);
            return Cmd::None;
        }
        match event {
            Event::Key(KeyEvent {
                code,
                kind: KeyEventKind::Press,
                ..
            }) => match code {
                KeyCode::Char('b') => {
                    self.use_custom_breakpoints = !self.use_custom_breakpoints;
                }
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.width = self.width.saturating_add(10).min(300);
                }
                KeyCode::Char('-') => {
                    self.width = self.width.saturating_sub(10).max(20);
                }
                _ => {}
            },
            Event::Resize { width, height } => {
                self.width = *width;
                self.height = *height;
            }
            _ => {}
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Update width from area (more reliable than resize events in tests).
        // We use area.width since the frame gives us the actual content area.
        let width = area.width;
        let bp = self.breakpoints().classify_width(width);

        // Top indicator + main content
        let chunks = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Fill])
            .split(area);

        // Render breakpoint indicator at top
        self.layout_indicator.set(chunks[0]);
        self.render_breakpoint_indicator(frame, chunks[0]);

        // Render main content with responsive layout
        // Re-derive layout inline using area.width for test accuracy
        let layout = ResponsiveLayout::new(Flex::vertical().constraints([Constraint::Fill]))
            .at(
                Breakpoint::Md,
                Flex::horizontal().constraints([Constraint::Fixed(28), Constraint::Fill]),
            )
            .at(
                Breakpoint::Lg,
                Flex::horizontal().constraints([
                    Constraint::Fixed(28),
                    Constraint::Fill,
                    Constraint::Fixed(24),
                ]),
            )
            .with_breakpoints(self.breakpoints());

        let result = layout.split_for(bp, chunks[1]);

        match result.rects.len() {
            1 => {
                self.render_stacked_view(frame, result.rects[0]);
            }
            2 => {
                self.render_sidebar(frame, result.rects[0]);
                self.render_content_panel(frame, result.rects[1]);
            }
            3 => {
                self.render_sidebar(frame, result.rects[0]);
                self.render_content_panel(frame, result.rects[1]);
                self.render_aside(frame, result.rects[2]);
            }
            _ => {}
        }
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "b",
                action: "Toggle custom breakpoints",
            },
            HelpEntry {
                key: "+ / -",
                action: "Adjust simulated width",
            },
            HelpEntry {
                key: "Click",
                action: "Toggle breakpoints",
            },
            HelpEntry {
                key: "Scroll",
                action: "Adjust width",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Responsive Layout"
    }

    fn tab_label(&self) -> &'static str {
        "Resp"
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bp_label(bp: Breakpoint) -> &'static str {
    match bp {
        Breakpoint::Xs => "XS (<60)",
        Breakpoint::Sm => "SM (60-89)",
        Breakpoint::Md => "MD (90-119)",
        Breakpoint::Lg => "LG (120-159)",
        Breakpoint::Xl => "XL (160+)",
    }
}

fn bp_color(bp: Breakpoint) -> theme::ColorToken {
    match bp {
        Breakpoint::Xs => theme::accent::ACCENT_5,
        Breakpoint::Sm => theme::accent::ACCENT_6,
        Breakpoint::Md => theme::accent::ACCENT_3,
        Breakpoint::Lg => theme::accent::ACCENT_1,
        Breakpoint::Xl => theme::accent::ACCENT_4,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::Modifiers;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn initial_state() {
        let screen = ResponsiveDemo::new();
        assert_eq!(screen.title(), "Responsive Layout");
        assert_eq!(screen.tab_label(), "Resp");
        assert!(!screen.use_custom_breakpoints);
    }

    #[test]
    fn toggle_breakpoints() {
        let mut screen = ResponsiveDemo::new();
        assert!(!screen.use_custom_breakpoints);

        screen.update(&press(KeyCode::Char('b')));
        assert!(screen.use_custom_breakpoints);

        screen.update(&press(KeyCode::Char('b')));
        assert!(!screen.use_custom_breakpoints);
    }

    #[test]
    fn resize_updates_dimensions() {
        let mut screen = ResponsiveDemo::new();
        screen.update(&Event::Resize {
            width: 120,
            height: 40,
        });
        assert_eq!(screen.width, 120);
        assert_eq!(screen.height, 40);
    }

    #[test]
    fn breakpoint_detection() {
        let mut s = ResponsiveDemo::new();
        s.width = 40;
        assert_eq!(s.current_breakpoint(), Breakpoint::Xs);
        s.width = 70;
        assert_eq!(s.current_breakpoint(), Breakpoint::Sm);
        s.width = 100;
        assert_eq!(s.current_breakpoint(), Breakpoint::Md);
        s.width = 130;
        assert_eq!(s.current_breakpoint(), Breakpoint::Lg);
        s.width = 170;
        assert_eq!(s.current_breakpoint(), Breakpoint::Xl);
    }

    #[test]
    fn custom_breakpoints() {
        let mut screen = ResponsiveDemo::new();
        screen.use_custom_breakpoints = true;
        screen.width = 55;
        // Custom: sm≥50, so 55 = Sm
        assert_eq!(screen.current_breakpoint(), Breakpoint::Sm);
    }

    #[test]
    fn keybindings_non_empty() {
        let screen = ResponsiveDemo::new();
        assert!(!screen.keybindings().is_empty());
    }

    #[test]
    fn tick_updates_count() {
        let mut screen = ResponsiveDemo::new();
        screen.tick(42);
        assert_eq!(screen.tick_count, 42);
    }

    #[test]
    fn view_empty_area_no_panic() {
        let screen = ResponsiveDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        screen.view(&mut frame, Rect::default());
    }

    #[test]
    fn view_small_terminal() {
        let screen = ResponsiveDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 40, 10));
        // Should render single-column layout without panic
    }

    #[test]
    fn view_medium_terminal() {
        let screen = ResponsiveDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(100, 30, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 100, 30));
        // Should render 2-column layout
    }

    #[test]
    fn view_large_terminal() {
        let screen = ResponsiveDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(130, 40, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 130, 40));
        // Should render 3-column layout
    }

    #[test]
    fn responsive_values_text_varies() {
        let mut screen = ResponsiveDemo::new();

        screen.width = 40; // Xs
        let xs_text = screen.responsive_values_text();
        assert!(xs_text.contains("compact"));

        screen.width = 100; // Md
        let md_text = screen.responsive_values_text();
        assert!(md_text.contains("comfortable"));
    }

    #[test]
    fn layout_info_reflects_columns() {
        let mut screen = ResponsiveDemo::new();

        screen.width = 40;
        assert!(screen.layout_info_text().contains("Columns: 1"));

        screen.width = 100;
        assert!(screen.layout_info_text().contains("Columns: 2"));

        screen.width = 130;
        assert!(screen.layout_info_text().contains("Columns: 3"));
    }

    #[test]
    fn visibility_info_varies() {
        let mut screen = ResponsiveDemo::new();

        screen.width = 40; // Xs
        let text = screen.visibility_info_text();
        assert!(text.contains("Sidebar: hidden"));
        assert!(text.contains("Aside:   hidden"));

        screen.width = 100; // Md
        let text = screen.visibility_info_text();
        assert!(text.contains("Sidebar: visible"));
        assert!(text.contains("Aside:   hidden"));

        screen.width = 130; // Lg
        let text = screen.visibility_info_text();
        assert!(text.contains("Sidebar: visible"));
        assert!(text.contains("Aside:   visible"));
    }

    #[test]
    fn default_impl() {
        let screen = ResponsiveDemo::default();
        assert_eq!(screen.width, 80);
    }

    #[test]
    fn click_indicator_toggles_breakpoints() {
        use ftui_core::event::MouseEvent;
        let mut screen = ResponsiveDemo::new();
        screen.layout_indicator.set(Rect::new(0, 0, 80, 1));
        assert!(!screen.use_custom_breakpoints);
        screen.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            10,
            0,
        )));
        assert!(screen.use_custom_breakpoints);
    }

    #[test]
    fn right_click_indicator_resets() {
        use ftui_core::event::MouseEvent;
        let mut screen = ResponsiveDemo::new();
        screen.layout_indicator.set(Rect::new(0, 0, 80, 1));
        screen.use_custom_breakpoints = true;
        screen.width = 150;
        screen.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::Down(MouseButton::Right),
            10,
            0,
        )));
        assert!(!screen.use_custom_breakpoints);
        assert_eq!(screen.width, 80);
    }

    #[test]
    fn scroll_adjusts_width() {
        use ftui_core::event::MouseEvent;
        let mut screen = ResponsiveDemo::new();
        assert_eq!(screen.width, 80);
        screen.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            10,
            10,
        )));
        assert_eq!(screen.width, 90);
        screen.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            10,
            10,
        )));
        assert_eq!(screen.width, 80);
    }

    #[test]
    fn scroll_width_bounded() {
        use ftui_core::event::MouseEvent;
        let mut screen = ResponsiveDemo::new();
        screen.width = 300;
        screen.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollUp,
            10,
            10,
        )));
        assert_eq!(screen.width, 300); // capped

        screen.width = 20;
        screen.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::ScrollDown,
            10,
            10,
        )));
        assert_eq!(screen.width, 20); // floor
    }

    #[test]
    fn mouse_move_ignored() {
        use ftui_core::event::MouseEvent;
        let mut screen = ResponsiveDemo::new();
        let initial_width = screen.width;
        screen.update(&Event::Mouse(MouseEvent::new(
            MouseEventKind::Moved,
            10,
            10,
        )));
        assert_eq!(screen.width, initial_width);
    }
}
