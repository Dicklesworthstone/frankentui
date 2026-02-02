//! Counter Example - Basic State Management
//!
//! Demonstrates the Model/Update/View pattern with a simple counter.
//! Press Up/Down to increment/decrement, Ctrl+C to quit.
//!
//! Run: `cargo run -p ftui-harness --example counter`

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::{App, Cmd, Model, ScreenMode};
use ftui_widgets::Widget;
use ftui_widgets::paragraph::Paragraph;

struct Counter {
    count: i64,
}

enum Msg {
    Increment,
    Decrement,
    Quit,
    Noop,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(KeyEvent {
                code: KeyCode::Up,
                kind: KeyEventKind::Press,
                ..
            }) => Msg::Increment,
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                kind: KeyEventKind::Press,
                ..
            }) => Msg::Decrement,
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

impl Model for Counter {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Self::Message> {
        match msg {
            Msg::Increment => self.count += 1,
            Msg::Decrement => self.count -= 1,
            Msg::Quit => return Cmd::Quit,
            Msg::Noop => {}
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());

        // Split into 3 rows: title, counter, instructions
        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .split(area);

        // Title
        let title = Paragraph::new("Counter Example");
        title.render(rows[0], frame);

        // Counter value
        let value = Paragraph::new(format!("  Count: {}", self.count));
        value.render(rows[1], frame);

        // Instructions
        let help = Paragraph::new("  Up/Down: ±1 | Ctrl+C: Quit");
        help.render(rows[2], frame);
    }
}

fn main() -> std::io::Result<()> {
    App::new(Counter { count: 0 })
        .screen_mode(ScreenMode::Inline { ui_height: 3 })
        .run()
}
