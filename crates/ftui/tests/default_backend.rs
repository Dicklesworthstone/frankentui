//! The published facade must be able to open a terminal with its default
//! features (the README's Minimal API Example depends on it). These tests pin
//! the backend that `ftui::App::run` selects in a default build and that the
//! prelude carries everything the README example imports.

#![cfg(feature = "runtime")]

use ftui::prelude::*;
use ftui::widgets::paragraph::Paragraph;

/// A default build compiles a real backend: native on Unix, Crossterm elsewhere.
#[test]
fn default_features_select_a_terminal_backend() {
    let expected = if cfg!(unix) { "native" } else { "crossterm" };
    if cfg!(feature = "backend") {
        assert_eq!(ftui::DEFAULT_BACKEND, expected);
    } else {
        // `--no-default-features --features runtime`: no backend, and the
        // constant must say so rather than lie.
        assert_eq!(ftui::DEFAULT_BACKEND, "none");
    }
}

/// The README example compiles against the prelude alone plus the widget it
/// renders: `Widget` (for `.render`), `Rect`, `Frame`, `Event`, `App`, `Cmd`,
/// `Model`, `ScreenMode` all come from `ftui::prelude::*`.
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

#[test]
fn readme_example_model_builds_through_the_prelude() {
    let mut app = TickApp { ticks: 0 };
    let _ = app.update(Msg::Tick);
    assert_eq!(app.ticks, 1);
    // The builder type-checks with the prelude imports; running it needs a
    // terminal and is covered by scripts/consumer_smoke_e2e.sh.
    let _builder = App::new(TickApp { ticks: 0 }).screen_mode(ScreenMode::Inline { ui_height: 1 });
}
