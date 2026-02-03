#![forbid(unsafe_code)]

//! Intrinsic sizing demo screen (bd-2dow.7).
//!
//! Demonstrates content-aware layouts enabled by intrinsic sizing:
//! - Adaptive sidebar that collapses to icons at narrow widths
//! - Flexible cards that size to content
//! - Auto-sizing table columns
//! - Responsive form layout

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_widgets::Widget;
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::list::{List, ListItem};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::table::{Row, Table};

use super::{HelpEntry, Screen};
use crate::theme;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SCENARIO_COUNT: usize = 4;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Intrinsic sizing demo screen.
pub struct IntrinsicSizingDemo {
    /// Current demo scenario (0-3).
    scenario: usize,
    /// Tick counter for animations.
    tick_count: u64,
    /// Current terminal width.
    width: u16,
}

impl Default for IntrinsicSizingDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl IntrinsicSizingDemo {
    /// Create a new intrinsic sizing demo screen.
    pub fn new() -> Self {
        Self {
            scenario: 0,
            tick_count: 0,
            width: 80,
        }
    }

    fn scenario_title(&self) -> &'static str {
        match self.scenario {
            0 => "Adaptive Sidebar",
            1 => "Flexible Cards",
            2 => "Auto-Sizing Table",
            3 => "Responsive Form",
            _ => "Unknown",
        }
    }

    // -- Scenario: Adaptive Sidebar -----------------------------------------

    fn render_adaptive_sidebar(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let is_narrow = area.width < 60;
        let sidebar_width = if is_narrow { 4 } else { 20 };

        let chunks = Flex::horizontal()
            .constraints([Constraint::Fixed(sidebar_width), Constraint::Fill])
            .split(area);

        // Sidebar
        let sidebar_items: Vec<ListItem> = if is_narrow {
            vec![
                ListItem::new("📊"),
                ListItem::new("⚙️"),
                ListItem::new("❓"),
                ListItem::new("🔔"),
                ListItem::new("👤"),
            ]
        } else {
            vec![
                ListItem::new("📊 Dashboard"),
                ListItem::new("⚙️ Settings"),
                ListItem::new("❓ Help"),
                ListItem::new("🔔 Notifications"),
                ListItem::new("👤 Profile"),
            ]
        };

        let sidebar_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(if is_narrow { "" } else { "Menu" })
            .style(Style::new().fg(theme::accent::ACCENT_1));

        let sidebar_inner = sidebar_block.inner(chunks[0]);
        sidebar_block.render(chunks[0], frame);

        List::new(sidebar_items)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(sidebar_inner, frame);

        // Main content
        let main_block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Content")
            .style(Style::new().fg(theme::fg::PRIMARY));

        let main_inner = main_block.inner(chunks[1]);
        main_block.render(chunks[1], frame);

        let mode_text = if is_narrow {
            "Icon-only mode (width < 60)"
        } else {
            "Full sidebar mode (width >= 60)"
        };

        let info = format!(
            "Current width: {}\nSidebar mode: {}\n\n\
             The sidebar adapts to terminal width:\n\
             • Narrow: icon-only (4 cols)\n\
             • Wide: full labels (20 cols)\n\n\
             Try resizing your terminal!",
            area.width, mode_text
        );

        Paragraph::new(&*info)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(main_inner, frame);
    }

    // -- Scenario: Flexible Cards -------------------------------------------

    fn render_flexible_cards(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Determine layout based on width
        let use_horizontal = area.width >= 60;

        if use_horizontal {
            // Side-by-side cards
            let chunks = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(40.0),
                    Constraint::Fixed(1),
                    Constraint::Percentage(60.0),
                ])
                .split(area);

            self.render_user_card(frame, chunks[0]);
            self.render_stats_card(frame, chunks[2]);
        } else {
            // Stacked cards
            let chunks = Flex::vertical()
                .constraints([Constraint::Fixed(6), Constraint::Fixed(1), Constraint::Fill])
                .split(area);

            self.render_user_card(frame, chunks[0]);
            self.render_stats_card(frame, chunks[2]);
        }
    }

    fn render_user_card(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" User Info ")
            .style(Style::new().fg(theme::accent::ACCENT_3));

        let inner = block.inner(area);
        block.render(area, frame);

        let info = "Name: Alice\nRole: Admin\nTeam: Platform";
        Paragraph::new(info)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(inner, frame);
    }

    fn render_stats_card(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Stats ")
            .style(Style::new().fg(theme::accent::ACCENT_1));

        let inner = block.inner(area);
        block.render(area, frame);

        let pulse = if self.tick_count % 10 < 5 {
            "▲"
        } else {
            "▼"
        };
        let stats = format!(
            "Requests: 1,234 {pulse}\n\
             Latency:  42ms\n\
             Uptime:   99.9%\n\
             Cache:    847 hits"
        );

        Paragraph::new(&*stats)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(inner, frame);
    }

    // -- Scenario: Auto-Sizing Table ----------------------------------------

    fn render_auto_table(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Auto-Sizing Columns ")
            .style(Style::new().fg(theme::fg::PRIMARY));

        let inner = block.inner(area);
        block.render(area, frame);

        // Create table with content-aware column sizing
        let header = Row::new(["ID", "Name", "Status", "Score"])
            .style(Style::new().fg(theme::accent::ACCENT_1).bold());

        let rows = vec![
            Row::new(["1", "Alice", "Active", "98.5"]),
            Row::new(["2", "Bob (Long Name Here)", "Pending", "87.2"]),
            Row::new(["3", "Charlie", "Active", "92.1"]),
            Row::new(["4", "Diana", "Inactive", "76.8"]),
            Row::new(["5", "Eve", "Active", "95.3"]),
        ];

        // Dynamic column widths based on available space
        let id_width = 6;
        let status_width = 12;
        let score_width = 10;
        let name_width = inner
            .width
            .saturating_sub(id_width + status_width + score_width + 3);

        let widths = [
            Constraint::Fixed(id_width),
            Constraint::Fixed(name_width.max(10)),
            Constraint::Fixed(status_width),
            Constraint::Fixed(score_width),
        ];

        Table::new(rows, widths)
            .header(header)
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(inner, frame);
    }

    // -- Scenario: Responsive Form ------------------------------------------

    fn render_responsive_form(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let use_horizontal = area.width >= 70;

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Responsive Form ")
            .style(Style::new().fg(theme::fg::PRIMARY));

        let inner = block.inner(area);
        block.render(area, frame);

        if use_horizontal {
            // Two columns of form fields
            let rows = Flex::vertical()
                .constraints([
                    Constraint::Fixed(3),
                    Constraint::Fixed(3),
                    Constraint::Fixed(3),
                    Constraint::Fill,
                ])
                .split(inner);

            // Row 1: Name + Email
            let cols1 = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(50.0),
                    Constraint::Fixed(1),
                    Constraint::Percentage(50.0),
                ])
                .split(rows[0]);
            self.render_field("Name", "Alice Smith", frame, cols1[0]);
            self.render_field("Email", "alice@example.com", frame, cols1[2]);

            // Row 2: Phone + Location
            let cols2 = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(50.0),
                    Constraint::Fixed(1),
                    Constraint::Percentage(50.0),
                ])
                .split(rows[1]);
            self.render_field("Phone", "+1 555-0123", frame, cols2[0]);
            self.render_field("Location", "San Francisco", frame, cols2[2]);

            // Row 3: Department + Role
            let cols3 = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(50.0),
                    Constraint::Fixed(1),
                    Constraint::Percentage(50.0),
                ])
                .split(rows[2]);
            self.render_field("Department", "Engineering", frame, cols3[0]);
            self.render_field("Role", "Senior Developer", frame, cols3[2]);

            // Info row
            let info = format!("Wide layout ({}w) - 2 columns", area.width);
            Paragraph::new(&*info)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(rows[3], frame);
        } else {
            // Single column stacked
            let rows = Flex::vertical()
                .constraints([
                    Constraint::Fixed(3),
                    Constraint::Fixed(3),
                    Constraint::Fixed(3),
                    Constraint::Fixed(3),
                    Constraint::Fixed(3),
                    Constraint::Fixed(3),
                    Constraint::Fill,
                ])
                .split(inner);

            self.render_field("Name", "Alice Smith", frame, rows[0]);
            self.render_field("Email", "alice@example.com", frame, rows[1]);
            self.render_field("Phone", "+1 555-0123", frame, rows[2]);
            self.render_field("Location", "San Francisco", frame, rows[3]);
            self.render_field("Department", "Engineering", frame, rows[4]);
            self.render_field("Role", "Senior Developer", frame, rows[5]);

            let info = format!("Narrow layout ({}w) - 1 column", area.width);
            Paragraph::new(&*info)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(rows[6], frame);
        }
    }

    fn render_field(&self, label: &str, value: &str, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let title = format!(" {label} ");
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(&title)
            .style(Style::new().fg(theme::accent::ACCENT_6));

        let inner = block.inner(area);
        block.render(area, frame);

        Paragraph::new(value)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
    }

    // -- Header bar ---------------------------------------------------------

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let title = format!(
            " Intrinsic Sizing Demo • {} ({}/{})",
            self.scenario_title(),
            self.scenario + 1,
            SCENARIO_COUNT
        );

        let style = Style::new()
            .fg(theme::fg::PRIMARY)
            .bg(theme::accent::ACCENT_1)
            .bold();

        Paragraph::new(&*title).style(style).render(area, frame);
    }
}

// ---------------------------------------------------------------------------
// Screen trait
// ---------------------------------------------------------------------------

impl Screen for IntrinsicSizingDemo {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match (*code, *modifiers) {
                // Scenario switching
                (KeyCode::Char('1'), Modifiers::NONE) => self.scenario = 0,
                (KeyCode::Char('2'), Modifiers::NONE) => self.scenario = 1,
                (KeyCode::Char('3'), Modifiers::NONE) => self.scenario = 2,
                (KeyCode::Char('4'), Modifiers::NONE) => self.scenario = 3,
                (KeyCode::Right, Modifiers::NONE) | (KeyCode::Char('n'), Modifiers::NONE) => {
                    self.scenario = (self.scenario + 1) % SCENARIO_COUNT;
                }
                (KeyCode::Left, Modifiers::NONE) | (KeyCode::Char('p'), Modifiers::NONE) => {
                    self.scenario = (self.scenario + SCENARIO_COUNT - 1) % SCENARIO_COUNT;
                }
                _ => {}
            }
        }

        if let Event::Resize { width, .. } = event {
            self.width = *width;
        }

        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Layout: header + content
        let chunks = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Fill])
            .split(area);

        self.render_header(frame, chunks[0]);

        match self.scenario {
            0 => self.render_adaptive_sidebar(frame, chunks[1]),
            1 => self.render_flexible_cards(frame, chunks[1]),
            2 => self.render_auto_table(frame, chunks[1]),
            3 => self.render_responsive_form(frame, chunks[1]),
            _ => {}
        }
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "1-4",
                action: "Switch scenario",
            },
            HelpEntry {
                key: "←/→",
                action: "Prev/Next scenario",
            },
            HelpEntry {
                key: "n/p",
                action: "Next/Prev scenario",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Intrinsic Sizing"
    }

    fn tab_label(&self) -> &'static str {
        "Sizing"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        })
    }

    #[test]
    fn initial_state() {
        let screen = IntrinsicSizingDemo::new();
        assert_eq!(screen.scenario, 0);
        assert_eq!(screen.title(), "Intrinsic Sizing");
        assert_eq!(screen.tab_label(), "Sizing");
    }

    #[test]
    fn scenario_cycling() {
        let mut screen = IntrinsicSizingDemo::new();

        screen.update(&press(KeyCode::Right));
        assert_eq!(screen.scenario, 1);

        screen.update(&press(KeyCode::Right));
        assert_eq!(screen.scenario, 2);

        screen.update(&press(KeyCode::Right));
        assert_eq!(screen.scenario, 3);

        screen.update(&press(KeyCode::Right));
        assert_eq!(screen.scenario, 0); // Wrap

        screen.update(&press(KeyCode::Left));
        assert_eq!(screen.scenario, 3); // Wrap back
    }

    #[test]
    fn direct_scenario_selection() {
        let mut screen = IntrinsicSizingDemo::new();

        screen.update(&press(KeyCode::Char('3')));
        assert_eq!(screen.scenario, 2);

        screen.update(&press(KeyCode::Char('1')));
        assert_eq!(screen.scenario, 0);

        screen.update(&press(KeyCode::Char('4')));
        assert_eq!(screen.scenario, 3);
    }

    #[test]
    fn scenario_titles() {
        let mut screen = IntrinsicSizingDemo::new();

        screen.scenario = 0;
        assert_eq!(screen.scenario_title(), "Adaptive Sidebar");

        screen.scenario = 1;
        assert_eq!(screen.scenario_title(), "Flexible Cards");

        screen.scenario = 2;
        assert_eq!(screen.scenario_title(), "Auto-Sizing Table");

        screen.scenario = 3;
        assert_eq!(screen.scenario_title(), "Responsive Form");
    }

    #[test]
    fn resize_updates_width() {
        let mut screen = IntrinsicSizingDemo::new();
        screen.update(&Event::Resize {
            width: 120,
            height: 40,
        });
        assert_eq!(screen.width, 120);
    }

    #[test]
    fn tick_updates_count() {
        let mut screen = IntrinsicSizingDemo::new();
        screen.tick(42);
        assert_eq!(screen.tick_count, 42);
    }

    #[test]
    fn keybindings_non_empty() {
        let screen = IntrinsicSizingDemo::new();
        assert!(!screen.keybindings().is_empty());
    }

    #[test]
    fn view_empty_area_no_panic() {
        let screen = IntrinsicSizingDemo::new();
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        screen.view(&mut frame, Rect::default());
    }

    #[test]
    fn view_all_scenarios_no_panic() {
        for scenario in 0..SCENARIO_COUNT {
            for (w, h) in [(40, 10), (80, 24), (120, 40), (200, 50)] {
                let mut screen = IntrinsicSizingDemo::new();
                screen.scenario = scenario;
                let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
                let mut frame = Frame::new(w, h, &mut pool);
                screen.view(&mut frame, Rect::new(0, 0, w, h));
            }
        }
    }

    #[test]
    fn adaptive_sidebar_narrow() {
        let mut screen = IntrinsicSizingDemo::new();
        screen.scenario = 0;
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(50, 20, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 50, 20));
        // Should render with icon-only sidebar
    }

    #[test]
    fn adaptive_sidebar_wide() {
        let mut screen = IntrinsicSizingDemo::new();
        screen.scenario = 0;
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 80, 24));
        // Should render with full sidebar
    }

    #[test]
    fn responsive_form_narrow() {
        let mut screen = IntrinsicSizingDemo::new();
        screen.scenario = 3;
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(60, 30, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 60, 30));
        // Should render single-column layout
    }

    #[test]
    fn responsive_form_wide() {
        let mut screen = IntrinsicSizingDemo::new();
        screen.scenario = 3;
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = Frame::new(100, 30, &mut pool);
        screen.view(&mut frame, Rect::new(0, 0, 100, 30));
        // Should render two-column layout
    }

    #[test]
    fn default_impl() {
        let screen = IntrinsicSizingDemo::default();
        assert_eq!(screen.scenario, 0);
    }
}
