//! High-Volume Log Streaming Example
//!
//! Demonstrates streaming log output at high frequency without flicker.
//! Shows how the LogViewer handles rapid updates while maintaining smooth UI.
//! Each generated line is also submitted through `Cmd::log_sgr_only`.
//! Logs accumulate when a scroll region is active; overlay shows the latest line.
//! Without arguments this generates messages. Pass a command after `--` to
//! stream a real child's stdout/stderr through the same model and log writer.
//! Child output uses plain-text sanitization; the generated demo retains color.
//! ProcessSubscription currently reads UTF-8 lines, so an unterminated line can
//! grow without a byte limit and invalid UTF-8 is not supported. Child stdin is
//! closed; quitting stops the immediate child, not an entire descendant tree.
//!
//! Run: `cargo run -p ftui-harness --example streaming`
//! Or: `cargo run -p ftui-harness --example streaming -- --exit-when-child-exits -- seq 1 10000`

use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_render::sanitize::sanitize;
use ftui_runtime::{
    App, Cmd, Every, Model, ProcessEvent, ProcessSubscription, ScreenMode, Subscription,
};
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
    options: StreamingOptions,
    child_finished: bool,
    child_status: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StreamingOptions {
    command: Vec<String>,
    exit_when_child_exits: bool,
}

impl StreamingOptions {
    fn parse(arguments: impl IntoIterator<Item = String>) -> std::io::Result<Self> {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--exit-when-child-exits" => options.exit_when_child_exits = true,
                "--" => {
                    options.command.extend(arguments);
                    if options.command.is_empty() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "expected a command after --",
                        ));
                    }
                    break;
                }
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "usage: streaming [--exit-when-child-exits] [-- COMMAND ARGS...]",
                    ));
                }
            }
        }
        if options.exit_when_child_exits && options.command.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "--exit-when-child-exits requires a command after --",
            ));
        }
        Ok(options)
    }
}

#[derive(Debug)]
enum Msg {
    Key(ftui_core::event::KeyEvent),
    StreamTick,
    Process(ProcessEvent),
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
    fn new(options: StreamingOptions) -> Self {
        let mut log = LogViewer::new(10_000);
        if options.command.is_empty() {
            log.push("High-volume streaming demo started");
            log.push("Press SPACE to pause/resume, Q to quit");
        } else {
            log.push("Child process streaming started; Q to quit");
        }
        log.push("---");

        Self {
            log,
            log_state: LogViewerState::default(),
            line_count: 0,
            paused: false,
            options,
            child_finished: false,
            child_status: "PROCESS".to_owned(),
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
                    KeyCode::Char(' ') if self.options.command.is_empty() => {
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
            Msg::Process(event) => {
                let status = match event {
                    ProcessEvent::Stdout(line) => {
                        self.line_count = self.line_count.saturating_add(1);
                        self.log.push(sanitize(&line).into_owned());
                        return Cmd::log(line);
                    }
                    ProcessEvent::Stderr(line) => {
                        self.line_count = self.line_count.saturating_add(1);
                        let text = format!("[stderr] {line}");
                        self.log.push(sanitize(&text).into_owned());
                        return Cmd::log(text);
                    }
                    ProcessEvent::Exited(code) => format!("EXIT {code}"),
                    ProcessEvent::Signaled(signal) => format!("SIGNAL {signal}"),
                    ProcessEvent::Killed => "KILLED".to_owned(),
                    ProcessEvent::Error(error) => format!("ERROR: {}", sanitize(&error)),
                };
                self.child_finished = true;
                self.child_status = status;
                let status = format!("[process] {} lines={}", self.child_status, self.line_count);
                self.log.push(status.clone());
                let log = Cmd::log(status);
                return if self.options.exit_when_child_exits {
                    Cmd::sequence(vec![log, Cmd::Quit])
                } else {
                    log
                };
            }
            Msg::StreamTick if !self.paused => {
                // Push multiple lines per tick to simulate burst output
                let mut logs = Vec::with_capacity(5);
                for _ in 0..5 {
                    self.line_count += 1;
                    let line = self.generate_log_line();
                    self.log.push(line.clone());
                    let color = match self.line_count % 10 {
                        0 => 31,
                        1 | 2 => 33,
                        _ => 36,
                    };
                    logs.push(Cmd::log_sgr_only(format!("\x1b[{color}m{line}")));
                }
                return Cmd::batch(logs);
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
        let status_text = if !self.options.command.is_empty() {
            &self.child_status
        } else if self.paused {
            "PAUSED"
        } else {
            "STREAMING"
        };
        let lines_text = format!("Lines: {}", self.line_count);

        let hint = if self.options.command.is_empty() {
            StatusItem::key_hint("SPACE", "Pause")
        } else {
            StatusItem::key_hint("Q", "Quit")
        };
        let status = StatusLine::new()
            .left(StatusItem::text(status_text))
            .center(StatusItem::text(&lines_text))
            .right(hint);

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
        if let Some((program, arguments)) = self.options.command.split_first() {
            return if self.child_finished {
                Vec::new()
            } else {
                vec![Box::new(
                    ProcessSubscription::new(program, Msg::Process).args(arguments.iter().cloned()),
                )]
            };
        }
        // Stream at 20 ticks per second (50ms interval)
        vec![Box::new(Every::new(Duration::from_millis(50), || {
            Msg::StreamTick
        }))]
    }
}

fn main() -> std::io::Result<()> {
    let options = StreamingOptions::parse(std::env::args().skip(1))?;
    App::new(StreamingHarness::new(options))
        .screen_mode(ScreenMode::Inline { ui_height: 15 })
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::sanitize::SanitizeMode;

    #[test]
    fn command_arguments_remain_literal_and_invalid_options_fail() {
        let options = StreamingOptions::parse(
            [
                "--exit-when-child-exits",
                "--",
                "echo",
                "--flag",
                "a b",
                "$(literal)",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(options.command, ["echo", "--flag", "a b", "$(literal)"]);
        assert!(options.exit_when_child_exits);
        assert_eq!(
            StreamingOptions::parse([]).unwrap(),
            StreamingOptions::default()
        );
        for arguments in [vec!["--"], vec!["--exit-when-child-exits"], vec!["echo"]] {
            assert_eq!(
                StreamingOptions::parse(arguments.into_iter().map(str::to_owned))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn process_messages_log_with_strip_policy_and_exit_after_final_status() {
        let mut model = StreamingHarness::new(StreamingOptions {
            command: vec!["echo".to_owned()],
            exit_when_child_exits: true,
        });
        assert_eq!(model.subscriptions().len(), 1);
        for (event, expected) in [
            (
                ProcessEvent::Stdout("first\x1b[2J".to_owned()),
                "first\x1b[2J",
            ),
            (
                ProcessEvent::Stderr("warning".to_owned()),
                "[stderr] warning",
            ),
        ] {
            assert!(matches!(
                model.update(Msg::Process(event)),
                Cmd::Log {
                    text,
                    mode: SanitizeMode::Strip,
                } if text == expected
            ));
        }
        assert_eq!(model.line_count, 2);
        let Cmd::Sequence(commands) = model.update(Msg::Process(ProcessEvent::Exited(0))) else {
            panic!("final log must precede quit");
        };
        assert!(matches!(
            commands.as_slice(),
            [Cmd::Log { text, mode: SanitizeMode::Strip }, Cmd::Quit]
                if text == "[process] EXIT 0 lines=2"
        ));
        assert!(
            model.subscriptions().is_empty(),
            "completed child must not restart"
        );
    }
}
