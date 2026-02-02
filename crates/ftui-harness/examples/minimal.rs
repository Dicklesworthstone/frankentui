//! Minimal Agent Harness Example - Under 50 Lines
//!
//! Demonstrates the absolute minimum code for an agent harness UI.
//!
//! Run: `cargo run -p ftui-harness --example minimal`

use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_runtime::{App, Cmd, Every, Model, ScreenMode, Subscription};
use ftui_widgets::StatefulWidget;
use ftui_widgets::log_viewer::{LogViewer, LogViewerState};

struct Harness {
    log: LogViewer,
    state: LogViewerState,
}

enum Msg {
    Key(ftui_core::event::KeyEvent),
    Tick,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(k) => Msg::Key(k),
            _ => Msg::Tick,
        }
    }
}

impl Model for Harness {
    type Message = Msg;

    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::None
    }

    fn update(&mut self, msg: Msg) -> Cmd<Self::Message> {
        match msg {
            Msg::Key(k) if k.kind == KeyEventKind::Press => {
                if k.modifiers.contains(Modifiers::CTRL) && k.code == KeyCode::Char('c') {
                    return Cmd::Quit;
                }
                self.log.push(format!("Key: {:?}", k.code));
            }
            Msg::Tick => self.log.push("Tick..."),
            _ => {}
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());
        let mut state = self.state.clone();
        self.log.render(area, frame, &mut state);
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        vec![Box::new(Every::new(Duration::from_secs(1), || Msg::Tick))]
    }
}

fn main() -> std::io::Result<()> {
    let mut log = LogViewer::new(1000);
    log.push("Minimal harness started. Press Ctrl+C to quit.");

    App::new(Harness {
        log,
        state: LogViewerState::default(),
    })
    .screen_mode(ScreenMode::Inline { ui_height: 5 })
    .run()
}
