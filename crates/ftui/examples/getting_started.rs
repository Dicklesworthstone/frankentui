use std::time::Duration;

use ftui::core::event::{Event, KeyCode, KeyEventKind, Modifiers};
use ftui::core::geometry::Rect;
use ftui::render::frame::Frame;
use ftui::runtime::{Every, Subscription};
use ftui::widgets::StatefulWidget;
use ftui::widgets::log_viewer::{LogViewer, LogViewerState};
use ftui::{App, Cmd, Model, ScreenMode};

struct Harness {
    log: LogViewer,
    state: LogViewerState,
}

enum Msg {
    Key(ftui::KeyEvent),
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
        Cmd::none()
    }

    fn update(&mut self, msg: Msg) -> Cmd<Self::Message> {
        match msg {
            Msg::Key(k) if k.kind == KeyEventKind::Press => {
                if k.modifiers.contains(Modifiers::CTRL) && k.code == KeyCode::Char('c') {
                    return Cmd::quit();
                }
                self.log.push(format!("Key: {:?}", k.code));
            }
            Msg::Tick => self.log.push("Tick..."),
            _ => {}
        }
        Cmd::none()
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

fn main() -> ftui::Result<()> {
    let mut log = LogViewer::new(1000);
    log.push("Started. Press Ctrl+C to quit.");

    App::new(Harness {
        log,
        state: LogViewerState::default(),
    })
    .screen_mode(ScreenMode::Inline { ui_height: 5 })
    .run()?;

    Ok(())
}
