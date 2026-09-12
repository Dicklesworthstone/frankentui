//! `cargo run -p ftui-harness --example minimal`; q, Escape or Ctrl-C quits.
//! FTUI_HARNESS_EXIT_AFTER_MS rounds up to a 250 ms tick; unset/invalid/zero disables it.
use ftui_core::event::{Event, KeyCode::Escape, KeyEventKind};
use ftui_render::frame::Frame;
use ftui_runtime::{App, Cmd, Model, ScreenMode, Subscription, tick_every};
use ftui_widgets::{Widget, block::Block, paragraph::Paragraph};
use std::time::Duration;
struct Hello {
    ticks: u64,
    exit_tick: u64,
}
impl Model for Hello {
    type Message = Event;
    fn update(&mut self, event: Event) -> Cmd<Event> {
        match event {
            Event::Key(k) if k.kind == KeyEventKind::Release => return Cmd::none(),
            Event::Key(k) if k.is_char('q') || k.code == Escape => return Cmd::quit(),
            Event::Key(k) if k.ctrl() && k.is_char('c') => return Cmd::quit(),
            Event::Tick => self.ticks = self.ticks.saturating_add(1),
            _ => {}
        }
        if self.exit_tick > 0 && self.ticks >= self.exit_tick {
            return Cmd::quit();
        }
        Cmd::none()
    }
    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Event>>> {
        vec![tick_every(Duration::from_millis(250), || Event::Tick)]
    }
    fn view(&self, frame: &mut Frame) {
        Paragraph::new(format!("Hello from FrankenTUI ticks: {}", self.ticks))
            .block(Block::bordered().padding(0).title("minimal"))
            .render(frame.area(), frame);
    }
}
fn main() -> std::io::Result<()> {
    let exit_tick = std::env::var("FTUI_HARNESS_EXIT_AFTER_MS").unwrap_or_default();
    let exit_tick = exit_tick.parse::<u64>().unwrap_or(0).div_ceil(250);
    App::new(Hello {
        ticks: 0,
        exit_tick,
    })
    .screen_mode(ScreenMode::Inline { ui_height: 3 })
    .run()
}
