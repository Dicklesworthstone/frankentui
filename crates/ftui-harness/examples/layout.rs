//! Layout Example - Grid and Flex Layouts
//!
//! Demonstrates grid-based layout with header, sidebar, content, and footer.
//! Press Ctrl+C to quit.
//!
//! Run: `cargo run -p ftui-harness --example layout`

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Grid};
use ftui_render::frame::Frame;
use ftui_runtime::{App, Cmd, Model, ScreenMode};
use ftui_widgets::Widget;
use ftui_widgets::paragraph::Paragraph;

struct LayoutDemo;

enum Msg {
    Quit,
    Noop,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                kind: KeyEventKind::Press,
                ..
            }) if modifiers.contains(Modifiers::CTRL) => Msg::Quit,
            _ => Msg::Noop,
        }
    }
}

impl Model for LayoutDemo {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Self::Message> {
        match msg {
            Msg::Quit => Cmd::Quit,
            Msg::Noop => Cmd::None,
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());

        // Create a 3-row, 2-column grid:
        //   Row 0: Header (spans both columns)
        //   Row 1: Sidebar | Content
        //   Row 2: Footer (spans both columns)
        let grid = Grid::new()
            .rows([
                Constraint::Fixed(1),
                Constraint::Min(0),
                Constraint::Fixed(1),
            ])
            .columns([Constraint::Fixed(15), Constraint::Min(0)]);

        let layout = grid.split(area);

        // Header - spans both columns
        let header_area = layout.span(0, 0, 1, 2);
        let header = Paragraph::new("  Layout Demo - Header");
        header.render(header_area, frame);

        // Sidebar
        let sidebar_area = layout.cell(1, 0);
        let sidebar = Paragraph::new("  Sidebar");
        sidebar.render(sidebar_area, frame);

        // Content
        let content_area = layout.cell(1, 1);
        let content = Paragraph::new("  Main content area. Press Ctrl+C to quit.");
        content.render(content_area, frame);

        // Footer - spans both columns
        let footer_area = layout.span(2, 0, 1, 2);
        let footer = Paragraph::new("  Footer | Ctrl+C to quit");
        footer.render(footer_area, frame);
    }
}

fn main() -> std::io::Result<()> {
    App::new(LayoutDemo)
        .screen_mode(ScreenMode::Inline { ui_height: 10 })
        .run()
}
