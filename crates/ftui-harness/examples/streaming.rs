//! High-Volume Log Streaming Example
//!
//! Demonstrates streaming log output at high frequency without flicker.
//! Shows how the LogViewer handles rapid updates while maintaining smooth UI.
//!
//! Run: `cargo run -p ftui-harness --example streaming`

use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_runtime::{App, Cmd, Every, Model, ScreenMode, Subscription};
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::log_viewer::{LogViewer, LogViewerState};
use ftui_widgets::status_line::{StatusItem, StatusLine};
use ftui_widgets::{StatefulWidget, Widget};

struct StreamingHarness {
    log: LogViewer,
    log_state: LogViewerState,
    line_count: usize,
    paused: bool,
}

#[derive(Debug)]
enum Msg {
    Key(ftui_core::event::KeyEvent),
    StreamTick,
    Noop,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(k) => Msg::Key(k),
            _ => Msg::Noop,
        }
    }
}

impl StreamingHarness {
    fn new() -> Self {
        let mut log = LogViewer::new(10_000);
        log.push("High-volume streaming demo started");
        log.push("Press SPACE to pause/resume, Q to quit");
        log.push("---");

        Self {
            log,
            log_state: LogViewerState::default(),
            line_count: 0,
            paused: false,
        }
    }

    fn generate_log_line(&self) -> String {
        let level = match self.line_count % 10 {
            0 => "[ERROR]",
            1 | 2 => "[WARN] ",
            _ => "[INFO] ",
        };
        format!(
            "{} Line {:06}: Processing task {} of batch {}",
            level,
            self.line_count,
            self.line_count % 100,
            self.line_count / 100
        )
    }
}

impl Model for StreamingHarness {
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
                match k.code {
                    KeyCode::Char('q') => return Cmd::Quit,
                    KeyCode::Char(' ') => {
                        self.paused = !self.paused;
                        self.log.push(if self.paused {
                            "--- PAUSED ---".to_string()
                        } else {
                            "--- RESUMED ---".to_string()
                        });
                    }
                    KeyCode::PageUp => self.log.page_up(&self.log_state),
                    KeyCode::PageDown => self.log.page_down(&self.log_state),
                    KeyCode::Home => self.log.scroll_to_top(),
                    KeyCode::End => self.log.scroll_to_bottom(),
                    _ => {}
                }
            }
            Msg::StreamTick if !self.paused => {
                // Push multiple lines per tick to simulate burst output
                for _ in 0..5 {
                    self.line_count += 1;
                    let line = self.generate_log_line();
                    self.log.push(line);
                }
            }
            _ => {}
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame) {
        let area = Rect::from_size(frame.buffer.width(), frame.buffer.height());

        let chunks = Flex::vertical()
            .constraints([Constraint::Fixed(1), Constraint::Min(3)])
            .split(area);

        // Status bar
        let status_text = if self.paused { "PAUSED" } else { "STREAMING" };
        let lines_text = format!("Lines: {}", self.line_count);

        let status = StatusLine::new()
            .left(StatusItem::text(status_text))
            .center(StatusItem::text(&lines_text))
            .right(StatusItem::key_hint("SPACE", "Pause"));

        status.render(chunks[0], frame);

        // Log viewer with border
        let log_block = Block::new()
            .title(" Stream Output ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        let inner = log_block.inner(chunks[1]);
        log_block.render(chunks[1], frame);

        let mut state = self.log_state.clone();
        self.log.render(inner, frame, &mut state);
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        // Stream at 20 ticks per second (50ms interval)
        vec![Box::new(Every::new(Duration::from_millis(50), || {
            Msg::StreamTick
        }))]
    }
}

fn main() -> std::io::Result<()> {
    App::new(StreamingHarness::new())
        .screen_mode(ScreenMode::Inline { ui_height: 15 })
        .run()
}
