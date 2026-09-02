use ftui::prelude::*;
use ftui::widgets::paragraph::Paragraph;

struct TickApp {
    ticks: u64,
}

#[derive(Debug, Clone)]
enum Msg {
    Tick,
    Quit,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(k) if k.is_char('q') => Msg::Quit,
            _ => Msg::Tick,
        }
    }
}

impl Model for TickApp {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Tick => {
                self.ticks += 1;
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let text = format!("Ticks: {}  (press 'q' to quit)", self.ticks);
        let area = Rect::new(0, 0, frame.width(), 1);
        Paragraph::new(text).render(area, frame);
    }
}

fn main() -> std::io::Result<()> {
    App::new(TickApp { ticks: 0 })
        .screen_mode(ScreenMode::Inline { ui_height: 1 })
        .run()
}
