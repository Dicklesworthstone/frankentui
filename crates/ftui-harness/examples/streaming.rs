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
//! Ctrl-C requests an interrupt; press again within two seconds to quit.
//! F5 restarts after final output arrives and child cleanup is confirmed.
//! Quitting stops the immediate child, not an entire descendant tree.
//!
//! Run: `cargo run -p ftui-harness --example streaming`
//! Or: `cargo run -p ftui-harness --example streaming -- --exit-when-child-exits -- seq 1 10000`
//! Interactive: `cargo run -p ftui-harness --example streaming -- --stdin --exit-when-child-exits -- cat`

use std::time::{Duration, Instant};

use ftui_core::event::{Event, KeyCode, KeyEventKind, Modifiers, PasteEvent};
use ftui_core::geometry::Rect;
use ftui_layout::{Constraint, Flex};
use ftui_render::frame::Frame;
use ftui_render::sanitize::sanitize;
use ftui_runtime::{
    App, Cmd, Every, Model, ProcessControl, ProcessControlError, ProcessControlStatus,
    ProcessEvent, ProcessInput, ProcessInputError, ProcessInterruptStatus, ProcessSubscription,
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
    process_control: Option<ProcessControl>,
    process_pid: Option<u32>,
    process_closed: bool,
    process_can_restart: bool,
    interrupt_status: ProcessInterruptStatus,
    quit_armed_at: Option<Instant>,
    control_feedback: Option<String>,
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
    Process {
        generation: u64,
        event: ProcessEvent,
    },
    PollControl,
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
            log.push("Child process streaming started; Enter queues input, Ctrl-C interrupts");
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
            process_control: (!options.command.is_empty()).then(ProcessControl::new),
            process_pid: None,
            process_closed: false,
            process_can_restart: true,
            interrupt_status: ProcessInterruptStatus::Idle,
            quit_armed_at: None,
            control_feedback: None,
            input: TextInput::new()
                .with_placeholder("Type a line for the child...")
                .with_focused(options.stdin),
            input_feedback: "Enter sends; Ctrl-D closes input; Ctrl-C interrupts, twice quits.",
            options,
        }
    }

    fn submit_input(&mut self) -> Cmd<Msg> {
        let Some(process_input) = &self.process_input else {
            return Cmd::none();
        };
        self.control_feedback = None;
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
                self.input_feedback = "Stdin closed; draft kept. Ctrl-C interrupts, twice quits.";
                Cmd::none()
            }
            Err(ProcessInputError::TooLong(_)) => {
                self.input_feedback = "Input exceeds 64 KiB; shorten the draft.";
                Cmd::none()
            }
        }
    }

    fn control_note(&mut self, message: String) -> Cmd<Msg> {
        let message = sanitize(&message).into_owned();
        self.control_feedback = Some(message.clone());
        let log = format!("[process control] {message}");
        self.log.push(log.clone());
        Cmd::log(log)
    }

    fn handle_ctrl_c(&mut self, kind: KeyEventKind, now: Instant) -> Cmd<Msg> {
        if kind != KeyEventKind::Press {
            return Cmd::none();
        }
        let Some(control) = &self.process_control else {
            return Cmd::quit();
        };
        if self.child_finished {
            return Cmd::quit();
        }
        if self.quit_armed_at.is_some_and(|armed| {
            now.checked_duration_since(armed)
                .is_some_and(|elapsed| elapsed < Duration::from_secs(2))
        }) {
            return Cmd::quit();
        }
        self.quit_armed_at = Some(now);
        let result = control.request_interrupt();
        self.report_interrupt_request(result)
    }

    fn report_interrupt_request(&mut self, result: Result<(), ProcessControlError>) -> Cmd<Msg> {
        let message = match result {
            Ok(()) => "Interrupt requested; Ctrl-C again within 2s quits.",
            Err(ProcessControlError::NotRunning) => {
                "Child not running yet; Ctrl-C again within 2s quits."
            }
            Err(ProcessControlError::Closed) => {
                "Child control closed; Ctrl-C again within 2s quits."
            }
            Err(ProcessControlError::AlreadyPending) => {
                "Interrupt already pending; Ctrl-C again within 2s quits."
            }
            Err(ProcessControlError::Unsupported) => {
                "Interrupt unsupported here; Ctrl-C again within 2s quits."
            }
        };
        self.control_note(message.to_owned())
    }

    fn apply_control_status(&mut self, status: ProcessControlStatus) -> Cmd<Msg> {
        let restart_became_available =
            self.child_finished && !self.process_can_restart && status.can_restart;
        self.process_pid = status.pid;
        self.process_closed = status.closed;
        self.process_can_restart = status.can_restart;
        if restart_became_available
            && self.control_feedback.as_deref()
                == Some("Restart unavailable: child exit not confirmed.")
        {
            self.control_feedback = None;
        }
        if self.child_finished && self.process_input.is_some() {
            self.input_feedback = if self.process_can_restart {
                "Child finished; F5 restarts with draft kept. Q or Ctrl-C quits."
            } else {
                "Child cleanup unconfirmed; draft kept. Q or Ctrl-C quits."
            };
        }
        if self.interrupt_status == status.interrupt {
            return Cmd::none();
        }
        let message = match &status.interrupt {
            ProcessInterruptStatus::Idle => None,
            ProcessInterruptStatus::Pending => Some("Interrupt pending.".to_owned()),
            ProcessInterruptStatus::Sent => Some("SIGINT sent to the immediate child.".to_owned()),
            ProcessInterruptStatus::Failed(error) => Some(format!("Interrupt failed: {error}")),
            ProcessInterruptStatus::Canceled => {
                Some("Interrupt canceled before sending.".to_owned())
            }
        };
        self.interrupt_status = status.interrupt;
        message.map_or_else(Cmd::none, |message| self.control_note(message))
    }

    fn finish_child(
        &mut self,
        status: String,
        control_status: Option<ProcessControlStatus>,
    ) -> Cmd<Msg> {
        self.child_finished = true;
        self.quit_armed_at = None;
        self.control_feedback = None;
        // A short run can finish before the next poll. Publish an unseen
        // interrupt outcome before the terminal log, including before Quit.
        let control_note =
            control_status.map_or_else(Cmd::none, |snapshot| self.apply_control_status(snapshot));
        if let Some(input) = &self.process_input {
            input.close();
            self.input.set_focused(false);
        }
        self.child_status = status;
        let status = format!("[process] {} lines={}", self.child_status, self.line_count);
        self.log.push(status.clone());
        let mut commands = Vec::with_capacity(3);
        if !matches!(control_note, Cmd::None) {
            commands.push(control_note);
        }
        commands.push(Cmd::log(status));
        if self.options.exit_when_child_exits {
            commands.push(Cmd::Quit);
        }
        Cmd::sequence(commands)
    }

    fn restart_child(&mut self) -> Cmd<Msg> {
        let Some(control) = &self.process_control else {
            return Cmd::none();
        };
        self.restart_child_with_status(control.status())
    }

    fn restart_child_with_status(&mut self, status: ProcessControlStatus) -> Cmd<Msg> {
        let Some(control) = &self.process_control else {
            return Cmd::none();
        };
        self.process_pid = status.pid;
        self.process_closed = status.closed;
        self.process_can_restart = status.can_restart;
        self.interrupt_status = status.interrupt;
        if !self.child_finished {
            return self.control_note(
                "Wait for the current run's final output before restarting.".to_owned(),
            );
        }
        if !self.process_can_restart {
            return self.control_note("Restart unavailable: child exit not confirmed.".to_owned());
        }
        let previous = control.generation();
        let control = ProcessControl::new();
        let generation = control.generation();
        self.process_control = Some(control);
        self.process_input = self.options.stdin.then(ProcessInput::new);
        self.process_pid = None;
        self.process_closed = false;
        self.process_can_restart = true;
        self.interrupt_status = ProcessInterruptStatus::Idle;
        self.quit_armed_at = None;
        self.control_feedback = None;
        self.line_count = 0;
        self.child_finished = false;
        self.child_status = "PROCESS".to_owned();
        self.input.set_focused(self.options.stdin);
        self.input_feedback = "Restarted; draft kept. Enter sends; Ctrl-C interrupts, twice quits.";
        let boundary = format!("[process] RESTART run={previous} -> run={generation}");
        self.log.push(boundary.clone());
        Cmd::log(boundary)
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
                    return self.handle_ctrl_c(k.kind, Instant::now());
                }
                if k.code == KeyCode::F(5) && k.kind == KeyEventKind::Press {
                    return self.restart_child();
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
                            self.control_feedback = None;
                        }
                        KeyCode::PageUp => self.log.page_up(&self.log_state),
                        KeyCode::PageDown => self.log.page_down(&self.log_state),
                        _ => {
                            if self.input.handle_event(&Event::Key(k)) {
                                self.control_feedback = None;
                            }
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
                self.control_feedback = None;
            }
            Msg::PollControl => {
                if let Some(control) = &self.process_control {
                    return self.apply_control_status(control.status());
                }
            }
            Msg::Process { generation, event } => {
                if self
                    .process_control
                    .as_ref()
                    .map(ProcessControl::generation)
                    != Some(generation)
                {
                    return Cmd::none();
                }
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
                let snapshot = self.process_control.as_ref().map(ProcessControl::status);
                return self.finish_child(status, snapshot);
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
        } else if self.process_control.is_some() {
            Flex::vertical()
                .constraints([
                    Constraint::Fixed(1),
                    Constraint::Min(3),
                    Constraint::Fixed(1),
                ])
                .split(area)
        } else {
            Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Min(3)])
                .split(area)
        };

        // Status bar
        let process_status = if let Some(pid) = self.process_pid {
            format!("{} PID {pid}", self.child_status)
        } else if self.process_closed && !self.child_finished {
            "DRAINING".to_owned()
        } else {
            self.child_status.clone()
        };
        let status_text = if self.process_control.is_some() {
            &process_status
        } else if self.paused {
            "PAUSED"
        } else {
            "STREAMING"
        };
        let lines_text = format!("Lines: {}", self.line_count);

        let hint = if self.options.command.is_empty() {
            StatusItem::key_hint("SPACE", "Pause")
        } else if !self.child_finished {
            StatusItem::key_hint("Ctrl-C", "Interrupt")
        } else if self.process_can_restart {
            StatusItem::key_hint("F5", "Restart")
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
            Paragraph::new(
                self.control_feedback
                    .as_deref()
                    .unwrap_or(self.input_feedback),
            )
            .render(chunks[3], frame);
        } else if self.process_control.is_some() {
            let feedback = self.control_feedback.as_deref().unwrap_or(
                if self.child_finished && self.process_can_restart {
                    "F5 restarts; Q or Ctrl-C quits."
                } else if self.child_finished {
                    "Child cleanup unconfirmed; Q or Ctrl-C quits."
                } else {
                    "Q quits; Ctrl-C interrupts, twice within 2s quits."
                },
            );
            Paragraph::new(feedback).render(chunks[2], frame);
        }
    }

    fn subscriptions(&self) -> Vec<Box<dyn Subscription<Self::Message>>> {
        if let Some((program, arguments)) = self.options.command.split_first() {
            return if self.child_finished {
                if self.process_can_restart {
                    Vec::new()
                } else {
                    // A retained reaper can confirm cleanup after the terminal
                    // error. Keep polling, without creating another process.
                    vec![Box::new(Every::new(Duration::from_millis(250), || {
                        Msg::PollControl
                    }))]
                }
            } else {
                let control = self.process_control.as_ref().expect("command has control");
                let generation = control.generation();
                let mut subscription = ProcessSubscription::new(program, move |event| {
                    Msg::Process { generation, event }
                })
                .args(arguments.iter().cloned())
                .control(control.clone());
                if let Some(input) = &self.process_input {
                    subscription = subscription.stdin(input.clone());
                }
                vec![
                    Box::new(subscription),
                    Box::new(Every::new(Duration::from_millis(250), || Msg::PollControl)),
                ]
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

    fn process_message(model: &StreamingHarness, event: ProcessEvent) -> Msg {
        Msg::Process {
            generation: model.process_control.as_ref().unwrap().generation(),
            event,
        }
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
        assert_eq!(model.subscriptions().len(), 2);
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
                model.update(process_message(&model, event)),
                Cmd::Log {
                    text,
                    mode: SanitizeMode::Strip,
                } if text == expected
            ));
        }
        assert_eq!(model.line_count, 2);
        let Cmd::Sequence(commands) =
            model.update(process_message(&model, ProcessEvent::Exited(0)))
        else {
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
        let poll_id = model.subscriptions()[1].id();
        for message in [
            key(KeyCode::Char('q')),
            Msg::from(Event::Paste(PasteEvent::bracketed("uit"))),
            key(KeyCode::Enter),
            process_message(&model, ProcessEvent::Stdout("reply".to_owned())),
            ctrl('d'),
        ] {
            let _ = model.update(message);
            assert_eq!(model.subscriptions().len(), 2);
            assert_eq!(model.subscriptions()[0].id(), id);
            assert_eq!(model.subscriptions()[1].id(), poll_id);
        }
        assert_ne!(interactive_model().subscriptions()[0].id(), id);
        let _ = model.update(process_message(&model, ProcessEvent::Exited(0)));
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
            "Stdin closed; draft kept. Ctrl-C interrupts, twice quits."
        );
        assert!(matches!(model.update(ctrl('d')), Cmd::None));
        let now = Instant::now();
        assert!(matches!(
            model.handle_ctrl_c(KeyEventKind::Press, now),
            Cmd::Log { .. }
        ));
        assert!(matches!(
            model.handle_ctrl_c(KeyEventKind::Press, now + Duration::from_millis(1)),
            Cmd::Quit
        ));

        let mut finished = interactive_model();
        let _ = finished.update(process_message(&finished, ProcessEvent::Exited(0)));
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
        assert!(row(14).starts_with("Stdin closed; draft kept. Ctrl-C interrupts, twice quits."));
    }

    #[test]
    fn synthetic_ctrl_c_window_ignores_repeat_and_expires_at_two_seconds() {
        // Deterministic input timing against an unspawned handle; native/PTY
        // tests separately establish signal delivery to a live child.
        let now = Instant::now();
        for (elapsed, quits) in [
            (Duration::from_millis(1999), true),
            (Duration::from_secs(2), false),
            (Duration::from_millis(2001), false),
        ] {
            let mut model = interactive_model();
            assert!(matches!(
                model.handle_ctrl_c(KeyEventKind::Repeat, now),
                Cmd::None
            ));
            assert_eq!(model.quit_armed_at, None);
            assert!(matches!(
                model.handle_ctrl_c(KeyEventKind::Press, now),
                Cmd::Log { .. }
            ));
            assert_eq!(model.quit_armed_at, Some(now));
            assert!(matches!(
                model.handle_ctrl_c(KeyEventKind::Repeat, now + Duration::from_millis(500)),
                Cmd::None
            ));
            assert!(matches!(
                model.handle_ctrl_c(KeyEventKind::Release, now + Duration::from_millis(600)),
                Cmd::None
            ));
            assert_eq!(model.quit_armed_at, Some(now));
            let command = model.handle_ctrl_c(KeyEventKind::Press, now + elapsed);
            assert_eq!(matches!(command, Cmd::Quit), quits);
            if !quits {
                assert!(matches!(command, Cmd::Log { .. }));
                assert_eq!(model.quit_armed_at, Some(now + elapsed));
            }
        }
        let mut routed = interactive_model();
        assert!(matches!(routed.update(ctrl('c')), Cmd::Log { .. }));
        let mut repeat = KeyEvent::new(KeyCode::Char('c')).with_modifiers(Modifiers::CTRL);
        repeat.kind = KeyEventKind::Repeat;
        assert!(matches!(routed.update(Msg::Key(repeat)), Cmd::None));
        let _ = routed.update(process_message(&routed, ProcessEvent::Exited(0)));
        assert!(matches!(routed.update(ctrl('c')), Cmd::Quit));
    }

    #[test]
    fn synthetic_interrupt_results_report_refusal_failure_and_pid_without_child_claims() {
        let mut model = interactive_model();
        for (result, expected) in [
            (Ok(()), "Interrupt requested;"),
            (
                Err(ProcessControlError::NotRunning),
                "Child not running yet;",
            ),
            (Err(ProcessControlError::Closed), "Child control closed;"),
            (
                Err(ProcessControlError::AlreadyPending),
                "Interrupt already pending;",
            ),
            (
                Err(ProcessControlError::Unsupported),
                "Interrupt unsupported here;",
            ),
        ] {
            assert!(matches!(
                model.report_interrupt_request(result),
                Cmd::Log { text, mode: SanitizeMode::Strip }
                    if text.starts_with(&format!("[process control] {expected}"))
            ));
            assert!(
                model
                    .control_feedback
                    .as_deref()
                    .unwrap()
                    .starts_with(expected)
            );
        }
        // These are supplied status snapshots, not syscalls or live PIDs.
        for (interrupt, expected) in [
            (ProcessInterruptStatus::Pending, "Interrupt pending."),
            (
                ProcessInterruptStatus::Sent,
                "SIGINT sent to the immediate child.",
            ),
            (
                ProcessInterruptStatus::Failed("denied\x1b[2J".to_owned()),
                "Interrupt failed: denied",
            ),
            (
                ProcessInterruptStatus::Canceled,
                "Interrupt canceled before sending.",
            ),
        ] {
            let snapshot = ProcessControlStatus {
                pid: Some(4321),
                closed: false,
                can_restart: false,
                interrupt,
            };
            assert!(matches!(
                model.apply_control_status(snapshot.clone()),
                Cmd::Log { text, mode: SanitizeMode::Strip }
                    if text == format!("[process control] {expected}")
            ));
            assert_eq!(model.process_pid, Some(4321));
            assert_eq!(model.control_feedback.as_deref(), Some(expected));
            assert!(matches!(model.apply_control_status(snapshot), Cmd::None));
        }
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(100, 15, &mut pool);
        model.view(&mut frame);
        let top: String = (0..100)
            .map(|x| {
                frame
                    .buffer
                    .get(x, 0)
                    .unwrap()
                    .content
                    .as_char()
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            top.contains("PID 4321"),
            "view must use the copied PID: {top}"
        );
        assert_eq!(model.process_control.as_ref().unwrap().status().pid, None);
    }

    #[test]
    fn synthetic_terminal_snapshot_publishes_unpolled_interrupt_outcome_before_exit() {
        // Exercise the terminal transition with supplied snapshots. These do
        // not establish real signal delivery, failure, or child cleanup.
        for (interrupt, expected) in [
            (
                ProcessInterruptStatus::Failed("denied\x1b[2J".to_owned()),
                "Interrupt failed: denied",
            ),
            (
                ProcessInterruptStatus::Sent,
                "SIGINT sent to the immediate child.",
            ),
            (
                ProcessInterruptStatus::Canceled,
                "Interrupt canceled before sending.",
            ),
        ] {
            for exit_when_child_exits in [false, true] {
                let mut model = interactive_model();
                model.options.exit_when_child_exits = exit_when_child_exits;
                model.input.set_value("draft kept");
                model.line_count = 2;
                model.quit_armed_at = Some(Instant::now());
                let history_len = model.log.len();
                let Cmd::Sequence(commands) = model.finish_child(
                    "EXIT 0".to_owned(),
                    Some(ProcessControlStatus {
                        pid: None,
                        closed: true,
                        can_restart: true,
                        interrupt: interrupt.clone(),
                    }),
                ) else {
                    panic!("unpolled outcome must precede the terminal status");
                };
                assert_eq!(commands.len(), if exit_when_child_exits { 3 } else { 2 });
                assert!(matches!(
                    &commands[0],
                    Cmd::Log { text, mode: SanitizeMode::Strip }
                        if text == &format!("[process control] {expected}")
                ));
                assert!(matches!(
                    &commands[1],
                    Cmd::Log { text, mode: SanitizeMode::Strip }
                        if text == "[process] EXIT 0 lines=2"
                ));
                if exit_when_child_exits {
                    assert!(matches!(&commands[2], Cmd::Quit));
                }
                assert_eq!(model.log.len(), history_len + 2);
                assert_eq!(model.control_feedback.as_deref(), Some(expected));
                assert_eq!(model.interrupt_status, interrupt);
                assert!(model.child_finished);
                assert_eq!(model.quit_armed_at, None);
                assert!(model.subscriptions().is_empty());
                assert_eq!(model.input.value(), "draft kept");
                assert!(!model.input.focused());
                assert_eq!(
                    model
                        .process_input
                        .as_ref()
                        .unwrap()
                        .try_send_line("later".to_owned()),
                    Err(ProcessInputError::Closed("later".to_owned()))
                );
                let mut pool = GraphemePool::new();
                let mut frame = Frame::new(100, 15, &mut pool);
                model.view(&mut frame);
                let feedback: String = (0..100)
                    .map(|x| {
                        frame
                            .buffer
                            .get(x, 14)
                            .unwrap()
                            .content
                            .as_char()
                            .unwrap_or(' ')
                    })
                    .collect();
                assert!(feedback.starts_with(expected), "{feedback}");
            }
        }
    }

    #[test]
    fn synthetic_terminal_cleanup_polling_stops_only_after_confirmation() {
        // Supplied snapshots exercise consumer transitions, not kernel cleanup.
        for stdin in [false, true] {
            let mut model = StreamingHarness::new(StreamingOptions {
                command: vec!["cat".to_owned()],
                stdin,
                ..StreamingOptions::default()
            });
            let initial = model.subscriptions();
            let process_id = initial[0].id();
            let poll_id = initial[1].id();
            let generation = model.process_control.as_ref().unwrap().generation();
            model.input.set_value("retained draft");
            model.line_count = 7;
            let unconfirmed = ProcessControlStatus {
                pid: None,
                closed: true,
                can_restart: false,
                interrupt: ProcessInterruptStatus::Idle,
            };
            let terminal_status = "ERROR: child exit unconfirmed";
            assert!(matches!(
                model.finish_child(terminal_status.to_owned(), Some(unconfirmed.clone())),
                Cmd::Log { text, mode: SanitizeMode::Strip }
                    if text == "[process] ERROR: child exit unconfirmed lines=7"
            ));
            for _ in 0..3 {
                assert!(matches!(
                    model.apply_control_status(unconfirmed.clone()),
                    Cmd::None
                ));
                let subscriptions = model.subscriptions();
                assert_eq!(subscriptions.len(), 1, "only cleanup polling remains");
                assert_eq!(subscriptions[0].id(), poll_id);
                assert_ne!(subscriptions[0].id(), process_id);
            }
            assert!(matches!(
                model.restart_child_with_status(unconfirmed.clone()),
                Cmd::Log { text, .. }
                    if text == "[process control] Restart unavailable: child exit not confirmed."
            ));
            let history_len = model.log.len();
            let mut confirmed = unconfirmed;
            confirmed.can_restart = true;
            assert!(matches!(model.apply_control_status(confirmed), Cmd::None));
            assert!(model.subscriptions().is_empty());
            assert!(model.child_finished, "confirmation must not start a child");
            assert_eq!(model.child_status, terminal_status);
            assert_eq!(model.line_count, 7);
            assert_eq!(model.log.len(), history_len);
            assert_eq!(
                model.process_control.as_ref().unwrap().generation(),
                generation
            );
            assert_eq!(model.input.value(), "retained draft");
            assert_eq!(model.control_feedback, None);
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(100, 15, &mut pool);
            model.view(&mut frame);
            let row = |y| -> String {
                (0..100)
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
            assert!(row(0).contains("F5"), "restart hint must refresh");
            assert!(row(14).contains("F5 restarts"));
            assert!(!row(14).contains("unconfirmed"));
        }
    }

    #[test]
    fn synthetic_late_poll_uses_current_handle_and_preserves_failure_feedback() {
        // The live handle is unspawned: its can_restart=true means no child
        // exists. This proves message routing, not delayed process reaping.
        let mut model = interactive_model();
        let generation = model.process_control.as_ref().unwrap().generation();
        let _ = model.finish_child(
            "ERROR: retained cleanup".to_owned(),
            Some(ProcessControlStatus {
                pid: None,
                closed: true,
                can_restart: false,
                interrupt: ProcessInterruptStatus::Idle,
            }),
        );
        let _ = model.control_note("Interrupt failed: denied".to_owned());
        let history_len = model.log.len();
        assert_eq!(model.subscriptions().len(), 1);
        assert!(matches!(model.update(Msg::PollControl), Cmd::None));
        assert!(model.process_can_restart);
        assert!(model.subscriptions().is_empty());
        assert!(model.child_finished);
        assert_eq!(model.child_status, "ERROR: retained cleanup");
        assert_eq!(model.log.len(), history_len);
        assert_eq!(
            model.process_control.as_ref().unwrap().generation(),
            generation
        );
        assert_eq!(
            model.control_feedback.as_deref(),
            Some("Interrupt failed: denied")
        );
    }

    #[test]
    fn synthetic_restart_requires_current_terminal_event_and_confirmed_child_cleanup() {
        let mut model = interactive_model();
        let generation = model.process_control.as_ref().unwrap().generation();
        let closed = ProcessControlStatus {
            pid: None,
            closed: true,
            can_restart: true,
            interrupt: ProcessInterruptStatus::Idle,
        };
        let _ = model.apply_control_status(closed.clone());
        assert!(matches!(
            model.restart_child_with_status(closed),
            Cmd::Log { .. }
        ));
        assert_eq!(
            model.process_control.as_ref().unwrap().generation(),
            generation
        );
        assert!(
            !model.child_finished,
            "early closure cannot discard queued output"
        );
        assert!(matches!(model.update(key(KeyCode::F(5))), Cmd::Log { .. }));
        assert_eq!(
            model.process_control.as_ref().unwrap().generation(),
            generation
        );
        let _ = model.update(process_message(
            &model,
            ProcessEvent::Stdout("last line".to_owned()),
        ));
        assert_eq!(model.line_count, 1);
        let _ = model.update(process_message(
            &model,
            ProcessEvent::Error("cleanup failed".to_owned()),
        ));
        assert!(model.child_finished);
        let unconfirmed = ProcessControlStatus {
            pid: Some(4321),
            closed: true,
            can_restart: false,
            interrupt: ProcessInterruptStatus::Idle,
        };
        assert!(matches!(
            model.restart_child_with_status(unconfirmed),
            Cmd::Log { text, .. }
                if text == "[process control] Restart unavailable: child exit not confirmed."
        ));
        assert_eq!(
            model.process_control.as_ref().unwrap().generation(),
            generation
        );
        assert_eq!(model.line_count, 1);
        assert!(model.child_finished);
    }

    #[test]
    fn synthetic_restart_preserves_history_and_draft_and_ignores_every_old_process_event() {
        let mut model = interactive_model();
        let previous = model.process_control.as_ref().unwrap().generation();
        let old_subscription = model.subscriptions()[0].id();
        let old_input = model.process_input.as_ref().unwrap().clone();
        let _ = model.update(process_message(
            &model,
            ProcessEvent::Stdout("previous output".to_owned()),
        ));
        model.input.set_value("retained draft");
        let _ = model.update(process_message(&model, ProcessEvent::Exited(42)));
        model.quit_armed_at = Some(Instant::now());
        let history_len = model.log.len();
        let command = model.restart_child_with_status(ProcessControlStatus {
            pid: None,
            closed: true,
            can_restart: true,
            interrupt: ProcessInterruptStatus::Canceled,
        });
        let generation = model.process_control.as_ref().unwrap().generation();
        assert_ne!(generation, previous);
        assert_ne!(model.subscriptions()[0].id(), old_subscription);
        assert!(matches!(
            command,
            Cmd::Log { text, mode: SanitizeMode::Strip }
                if text == format!("[process] RESTART run={previous} -> run={generation}")
        ));
        assert_eq!(model.log.len(), history_len + 1);
        assert_eq!(model.input.value(), "retained draft");
        assert!(model.input.focused());
        assert_eq!(model.quit_armed_at, None);
        assert_eq!(model.process_pid, None);
        assert_eq!(model.interrupt_status, ProcessInterruptStatus::Idle);
        assert_eq!(model.line_count, 0);
        assert!(!model.child_finished);
        assert_eq!(model.child_status, "PROCESS");
        assert_eq!(
            old_input.try_send_line("old".to_owned()),
            Err(ProcessInputError::Closed("old".to_owned()))
        );
        assert_eq!(
            model
                .process_input
                .as_ref()
                .unwrap()
                .try_send_line("new".to_owned()),
            Ok(())
        );
        for event in [
            ProcessEvent::Stdout("stale output".to_owned()),
            ProcessEvent::Stderr("stale warning".to_owned()),
            ProcessEvent::Exited(0),
            ProcessEvent::Signaled(2),
            ProcessEvent::Killed,
            ProcessEvent::Error("stale failure".to_owned()),
        ] {
            assert!(matches!(
                model.update(Msg::Process {
                    generation: previous,
                    event,
                }),
                Cmd::None
            ));
            assert_eq!(model.line_count, 0);
            assert!(!model.child_finished);
            assert_eq!(model.child_status, "PROCESS");
            assert_eq!(model.input.value(), "retained draft");
            assert_eq!(model.log.len(), history_len + 1);
            assert_eq!(model.control_feedback, None);
        }
        let _ = model.update(process_message(
            &model,
            ProcessEvent::Stdout("fresh output".to_owned()),
        ));
        assert_eq!(model.line_count, 1);
        assert_eq!(model.options.command, ["cat"]);
    }
}
