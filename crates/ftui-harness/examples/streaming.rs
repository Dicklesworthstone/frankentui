//! High-Volume Log Streaming Example
//!
//! Demonstrates streaming log output at high frequency without flicker.
//! Shows how the LogViewer handles rapid updates while maintaining smooth UI.
//! Each generated line is also submitted through `Cmd::log_sgr_only`.
//! Logs accumulate when a scroll region is active; overlay shows the latest line.
//! Without arguments this generates messages. Pass a command after `--` to
//! stream a real child's stdout/stderr through the same model and log writer.
//! Child output uses plain-text sanitization; the generated demo retains color.
//! ProcessSubscription delivers complete UTF-8 lines up to 64 KiB. Oversized
//! lines or invalid UTF-8 terminate the child and report incomplete output.
//! Partial lines wait for a newline or EOF; binary output is unsupported.
//! Child stdin is closed unless --stdin enables a single-line input editor.
//! Quitting stops the immediate child, not an entire descendant tree.
//!
//! Run: `cargo run -p ftui-harness --example streaming`
//! Or: `cargo run -p ftui-harness --example streaming -- --exit-when-child-exits -- seq 1 10000`
//! Interactive: `cargo run -p ftui-harness --example streaming -- --stdin --exit-when-child-exits -- cat`

use std::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers, PasteEvent};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_render::sanitize::sanitize;
use ftui_runtime::{
    App, Cmd, Every, Model, ProcessEvent, ProcessInput, ProcessInputError, ProcessSubscription,
    ScreenMode, Subscription,
};
use ftui_widgets::block::Block;
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::input::TextInput;
use ftui_widgets::log_viewer::{LogViewer, LogViewerState};
use ftui_widgets::paragraph::Paragraph;
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
    process_input: Option<ProcessInput>,
    input: TextInput,
    input_feedback: &'static str,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StreamingOptions {
    command: Vec<String>,
    exit_when_child_exits: bool,
    stdin: bool,
}

impl StreamingOptions {
    fn parse(arguments: impl IntoIterator<Item = String>) -> std::io::Result<Self> {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--exit-when-child-exits" => options.exit_when_child_exits = true,
                "--stdin" => options.stdin = true,
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
                        "usage: streaming [--stdin] [--exit-when-child-exits] [-- COMMAND ARGS...]",
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
        if options.stdin && options.command.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "--stdin requires a command after --",
            ));
        }
        Ok(options)
    }
}

#[derive(Debug)]
enum Msg {
    Key(ftui_core::event::KeyEvent),
    Paste(PasteEvent),
    StreamTick,
    Process(ProcessEvent),
    Noop,
}

impl From<Event> for Msg {
    fn from(e: Event) -> Self {
        match e {
            Event::Key(k) => Msg::Key(k),
            Event::Paste(paste) => Msg::Paste(paste),
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
        } else if options.stdin {
            log.push("Child process streaming started; Enter queues input, Ctrl-C quits");
        } else {
            log.push("Child process streaming started; Q to quit");
        }
        log.push("---");

        Self {
            log,
            log_state: LogViewerState::default(),
            line_count: 0,
            paused: false,
            child_finished: false,
            child_status: "PROCESS".to_owned(),
            process_input: options.stdin.then(ProcessInput::new),
            input: TextInput::new()
                .with_placeholder("Type a line for the child...")
                .with_focused(options.stdin),
            input_feedback: "Enter queues a line; Ctrl-D requests EOF; Ctrl-C quits.",
            options,
        }
    }

    fn submit_input(&mut self) -> Cmd<Msg> {
        let Some(process_input) = &self.process_input else {
            return Cmd::none();
        };
        let draft = self.input.value().to_owned();
        match process_input.try_send_line(draft) {
            Ok(()) => {
                let echo = format!("[stdin queued] {}", sanitize(self.input.value()));
                self.input.clear();
                self.input_feedback = "Queued for the child.";
                self.log.push(echo.clone());
                Cmd::log(echo)
            }
            Err(ProcessInputError::Full(_)) => {
                self.input_feedback = "Input queue full; retry Enter after the child reads.";
                Cmd::none()
            }
            Err(ProcessInputError::Closed(_)) => {
                self.input_feedback = "Stdin closed; draft kept. Ctrl-C quits.";
                Cmd::none()
            }
            Err(ProcessInputError::TooLong(_)) => {
                self.input_feedback = "Input exceeds 64 KiB; shorten the draft.";
                Cmd::none()
            }
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
            Msg::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                if k.modifiers.contains(Modifiers::CTRL) && k.code == KeyCode::Char('c') {
                    return Cmd::Quit;
                }
                if self.process_input.is_some() && !self.child_finished {
                    match k.code {
                        KeyCode::Enter if k.kind == KeyEventKind::Press => {
                            return self.submit_input();
                        }
                        KeyCode::Char('d') if k.modifiers.contains(Modifiers::CTRL) => {
                            if let Some(input) = &self.process_input {
                                input.close();
                            }
                            self.input_feedback =
                                "EOF requested; queued input drains before closing stdin.";
                        }
                        KeyCode::PageUp => self.log.page_up(&self.log_state),
                        KeyCode::PageDown => self.log.page_down(&self.log_state),
                        _ => {
                            self.input.handle_event(&Event::Key(k));
                        }
                    }
                    return Cmd::none();
                }
                if k.kind != KeyEventKind::Press {
                    return Cmd::none();
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
            Msg::Paste(paste) if self.process_input.is_some() && !self.child_finished => {
                self.input.handle_event(&Event::Paste(paste));
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
                if let Some(input) = &self.process_input {
                    input.close();
                    self.input.set_focused(false);
                    self.input_feedback = "Child finished; draft kept. Q or Ctrl-C quits.";
                }
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

        let chunks = if self.process_input.is_some() {
            Flex::vertical()
                .constraints([
                    Constraint::Fixed(1),
                    Constraint::Min(3),
                    Constraint::Fixed(1),
                    Constraint::Fixed(1),
                ])
                .split(area)
        } else {
            Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Min(3)])
                .split(area)
        };

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
        } else if self.process_input.is_some() && !self.child_finished {
            StatusItem::key_hint("Ctrl-C", "Quit")
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

        if self.process_input.is_some() {
            let input_parts = Flex::horizontal()
                .constraints([Constraint::Fixed(2), Constraint::Min(1)])
                .split(chunks[2]);
            Paragraph::new("> ").render(input_parts[0], frame);
            self.input.render(input_parts[1], frame);
            Paragraph::new(self.input_feedback).render(chunks[3], frame);
        }
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        if let Some((program, arguments)) = self.options.command.split_first() {
            return if self.child_finished {
                Vec::new()
            } else {
                let mut subscription =
                    ProcessSubscription::new(program, Msg::Process).args(arguments.iter().cloned());
                if let Some(input) = &self.process_input {
                    subscription = subscription.stdin(input.clone());
                }
                vec![Box::new(subscription)]
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
    use ftui_core::event::KeyEvent;
    use ftui_render::grapheme_pool::GraphemePool;
    use ftui_render::sanitize::SanitizeMode;
    use ftui_runtime::process_subscription::{MAX_PROCESS_LINE_BYTES, PROCESS_INPUT_CAPACITY};

    fn interactive_model() -> StreamingHarness {
        StreamingHarness::new(StreamingOptions {
            command: vec!["cat".to_owned()],
            stdin: true,
            ..StreamingOptions::default()
        })
    }

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code))
    }

    fn ctrl(character: char) -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Char(character)).with_modifiers(Modifiers::CTRL))
    }

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
        assert!(!options.stdin);
        let interactive = StreamingOptions::parse(
            ["--stdin", "--exit-when-child-exits", "--", "cat", "--stdin"].map(str::to_owned),
        )
        .unwrap();
        assert!(interactive.stdin);
        assert!(interactive.exit_when_child_exits);
        assert_eq!(interactive.command, ["cat", "--stdin"]);
        assert_eq!(
            StreamingOptions::parse([]).unwrap(),
            StreamingOptions::default()
        );
        for arguments in [
            vec!["--"],
            vec!["--exit-when-child-exits"],
            vec!["--stdin"],
            vec!["--stdin", "--"],
            vec!["echo"],
        ] {
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
            stdin: false,
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

    #[test]
    fn interactive_subscription_identity_survives_typing_submission_and_eof() {
        let mut model = interactive_model();
        let id = model.subscriptions()[0].id();
        for message in [
            key(KeyCode::Char('q')),
            Msg::from(Event::Paste(PasteEvent::bracketed("uit"))),
            key(KeyCode::Enter),
            Msg::Process(ProcessEvent::Stdout("reply".to_owned())),
            ctrl('d'),
        ] {
            let _ = model.update(message);
            assert_eq!(model.subscriptions().len(), 1);
            assert_eq!(model.subscriptions()[0].id(), id);
        }
        assert_ne!(interactive_model().subscriptions()[0].id(), id);
        let _ = model.update(Msg::Process(ProcessEvent::Exited(0)));
        assert!(model.subscriptions().is_empty());
    }

    #[test]
    fn accepted_input_clears_and_echoes_sanitized_text_but_full_queue_keeps_draft() {
        // Exercise actual admission without a child: the unclaimed input queue
        // cannot drain. These assertions do not imply child acknowledgment.
        let mut model = interactive_model();
        model.input.set_value("first\x1b[2J");
        assert!(matches!(
            model.update(key(KeyCode::Enter)),
            Cmd::Log { text, mode: SanitizeMode::Strip } if text == "[stdin queued] first"
        ));
        assert!(model.input.value().is_empty());
        assert_eq!(model.input_feedback, "Queued for the child.");
        assert!(matches!(
            model.update(key(KeyCode::Enter)),
            Cmd::Log { text, mode: SanitizeMode::Strip } if text == "[stdin queued] "
        ));
        for index in 2..PROCESS_INPUT_CAPACITY {
            model.input.set_value(format!("line {index}"));
            assert!(matches!(
                model.update(key(KeyCode::Enter)),
                Cmd::Log { text, mode: SanitizeMode::Strip }
                    if text == format!("[stdin queued] line {index}")
            ));
            assert!(model.input.value().is_empty());
        }
        model.input.set_value("keep this draft");
        for _ in 0..2 {
            assert!(matches!(model.update(key(KeyCode::Enter)), Cmd::None));
            assert_eq!(model.input.value(), "keep this draft");
            assert_eq!(
                model.input_feedback,
                "Input queue full; retry Enter after the child reads."
            );
        }
        assert_eq!(model.line_count, 0, "input echoes are not child output");
    }

    #[test]
    fn oversized_utf8_draft_is_preserved_until_the_user_shortens_it() {
        let mut model = interactive_model();
        let oversized = "é".repeat(MAX_PROCESS_LINE_BYTES / 2 + 1);
        let _ = model.update(Msg::from(Event::Paste(PasteEvent::bracketed(&oversized))));
        assert!(matches!(model.update(key(KeyCode::Enter)), Cmd::None));
        assert_eq!(model.input.value(), oversized);
        assert_eq!(
            model.input_feedback,
            "Input exceeds 64 KiB; shorten the draft."
        );

        model.input.set_value("revised");
        assert!(matches!(
            model.update(key(KeyCode::Enter)),
            Cmd::Log { text, mode: SanitizeMode::Strip } if text == "[stdin queued] revised"
        ));
        assert!(model.input.value().is_empty());
    }

    #[test]
    fn interactive_q_is_text_and_eof_keeps_unsent_draft_without_quitting() {
        let mut model = interactive_model();
        assert!(matches!(model.update(key(KeyCode::Char('q'))), Cmd::None));
        let _ = model.update(Msg::from(Event::Paste(PasteEvent::bracketed("uit\nnow"))));
        assert_eq!(model.input.value(), "quit now");
        assert!(matches!(model.update(ctrl('d')), Cmd::None));
        assert_eq!(model.input.value(), "quit now");
        assert_eq!(
            model.input_feedback,
            "EOF requested; queued input drains before closing stdin."
        );
        assert!(!model.child_finished);
        assert_eq!(
            model
                .process_input
                .as_ref()
                .unwrap()
                .try_send_line("later".to_owned()),
            Err(ProcessInputError::Closed("later".to_owned()))
        );
        assert!(matches!(model.update(key(KeyCode::Enter)), Cmd::None));
        assert_eq!(model.input.value(), "quit now");
        assert_eq!(
            model.input_feedback,
            "Stdin closed; draft kept. Ctrl-C quits."
        );
        assert!(matches!(model.update(ctrl('d')), Cmd::None));
        assert!(matches!(model.update(ctrl('c')), Cmd::Quit));

        let mut finished = interactive_model();
        let _ = finished.update(Msg::Process(ProcessEvent::Exited(0)));
        assert!(!finished.input.focused());
        assert!(matches!(
            finished.update(key(KeyCode::Char('q'))),
            Cmd::Quit
        ));
        let mut generated = StreamingHarness::new(StreamingOptions::default());
        assert!(matches!(
            generated.update(key(KeyCode::Char('q'))),
            Cmd::Quit
        ));
    }

    #[test]
    fn interactive_draft_and_rejection_feedback_fit_the_fifteen_row_chrome() {
        let mut model = interactive_model();
        model.input.set_value("retained draft");
        model.process_input.as_ref().unwrap().close();
        let _ = model.update(key(KeyCode::Enter));
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 15, &mut pool);
        model.view(&mut frame);
        let row = |y| -> String {
            (0..80)
                .map(|x| {
                    frame
                        .buffer
                        .get(x, y)
                        .unwrap()
                        .content
                        .as_char()
                        .unwrap_or(' ')
                })
                .collect()
        };
        assert!(row(13).starts_with("> retained draft"));
        assert!(row(14).starts_with("Stdin closed; draft kept. Ctrl-C quits."));
    }
}
